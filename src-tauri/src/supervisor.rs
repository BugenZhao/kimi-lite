//! One supervisor per open host window: brings the host's Kimi server up,
//! keeps a tunnel to it, watches its health, and reconnects forever (with
//! backoff) once it has been reachable. The local port is kept stable, so a
//! reconnect never changes the page origin: the web UI keeps its storage
//! and simply re-opens its own WebSocket.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Url};
use tokio::process::Child;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::config::HostConfig;
use crate::scripts::{self, Mode};
use crate::transport::{self, Transport};
use crate::AppState;

const DEFAULT_SERVER_PORT: u16 = 58627;
const HEALTH_INTERVAL: Duration = Duration::from_secs(5);
/// A tick arriving much later than scheduled means the machine slept.
const WAKE_GAP: Duration = Duration::from_secs(15);
const FIRST_CONNECT_ATTEMPTS: u32 = 3;
const READY_TIMEOUT: Duration = Duration::from_secs(20);
const LOG_LINES: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Connecting,
    Ready,
    Reconnecting,
    Failed,
    Stopped,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostStatus {
    pub id: String,
    pub name: String,
    pub phase: Phase,
    pub message: String,
    pub attempt: u32,
    /// Seconds until the next automatic attempt, when one is scheduled.
    pub retry_in: Option<u64>,
    pub log: Vec<String>,
    pub ever_ready: bool,
}

#[derive(Debug)]
pub enum Command {
    Retry,
    Restart,
    Stop,
    Shutdown,
}

pub struct Supervisor {
    /// Distinguishes this supervisor from a later one for the same host.
    pub generation: u64,
    tx: mpsc::UnboundedSender<Command>,
    status: Arc<Mutex<HostStatus>>,
}

impl Supervisor {
    pub fn spawn(app: AppHandle, host: &HostConfig) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let status = Arc::new(Mutex::new(HostStatus {
            id: host.id.clone(),
            name: host.name.clone(),
            phase: Phase::Connecting,
            message: "Starting…".into(),
            attempt: 0,
            retry_in: None,
            log: Vec::new(),
            ever_ready: false,
        }));
        let runner = Runner { ctx: Ctx { app, id: host.id.clone(), status: status.clone() }, rx };
        tauri::async_runtime::spawn(runner.run());
        static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self { generation, tx, status }
    }

    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }

    pub fn status(&self) -> HostStatus {
        self.status.lock().unwrap().clone()
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
    }
}

struct Connection {
    port: u16,
    token: String,
    /// The ssh process carrying the tunnel (the master, or a dedicated
    /// tunnel on Windows); dropping it closes the tunnel.
    tunnel: Option<Child>,
}

enum Exit {
    Lost(String),
    Restart,
    Stop,
    Shutdown,
}

#[derive(Clone)]
struct Ctx {
    app: AppHandle,
    id: String,
    status: Arc<Mutex<HostStatus>>,
}

struct Runner {
    ctx: Ctx,
    rx: mpsc::UnboundedReceiver<Command>,
}

impl Runner {
    async fn run(mut self) {
        let mut attempt: u32 = 0;
        let mut mode = Mode::Ensure;
        let mut unresponsive: u32 = 0;
        loop {
            let Some(host) = self.ctx.host() else { return };
            let transport = match &host.ssh_host {
                Some(ssh) => Transport::ssh(ssh, &host.id),
                None => Transport::Local,
            };
            let ever_ready = self.ctx.status.lock().unwrap().ever_ready;
            let phase = if ever_ready { Phase::Reconnecting } else { Phase::Connecting };
            self.ctx.update(|s| {
                s.name = host.name.clone();
                s.phase = phase;
                s.attempt = attempt;
                s.retry_in = None;
                s.message = match (&host.ssh_host, mode) {
                    (_, Mode::Restart) => "Restarting Kimi Code…".into(),
                    (Some(ssh), _) => format!("Connecting to {ssh}…"),
                    (None, _) => "Starting Kimi Code…".into(),
                };
            });

            let outcome = {
                let connect = self.ctx.connect(&host, &transport, mode);
                tokio::select! {
                    result = connect => Ok(result),
                    command = self.rx.recv() => Err(command),
                }
            };
            let result = match outcome {
                Ok(result) => result,
                Err(Some(Command::Retry)) => { attempt = 0; continue; }
                Err(Some(Command::Restart)) => { mode = Mode::Restart; continue; }
                Err(Some(Command::Stop)) => {
                    if !self.stop_server(&host, &transport).await { return; }
                    continue;
                }
                Err(Some(Command::Shutdown)) | Err(None) => return,
            };

            match result {
                Ok(connection) => {
                    attempt = 0;
                    unresponsive = 0;
                    mode = Mode::Ensure;
                    match self.serve(connection).await {
                        Exit::Lost(reason) => {
                            self.ctx.log(&reason);
                            self.ctx.update(|s| {
                                s.phase = Phase::Reconnecting;
                                s.message = format!("{reason} Reconnecting…");
                            });
                        }
                        Exit::Restart => mode = Mode::Restart,
                        Exit::Stop => {
                            if !self.stop_server(&host, &transport).await {
                                return;
                            }
                        }
                        Exit::Shutdown => return,
                    }
                }
                Err(error) => {
                    attempt += 1;
                    self.ctx.log(&error);
                    if error == NOT_RESPONDING {
                        unresponsive += 1;
                        // A live process that never answers is wedged.
                        if unresponsive >= 2 {
                            mode = Mode::Restart;
                        }
                    } else {
                        mode = Mode::Ensure;
                    }
                    let ever_ready = self.ctx.status.lock().unwrap().ever_ready;
                    if !ever_ready && attempt >= FIRST_CONNECT_ATTEMPTS {
                        self.ctx.update(|s| {
                            s.phase = Phase::Failed;
                            s.message = error.clone();
                            s.retry_in = None;
                        });
                        match self.wait_for_command(None).await {
                            Some(Command::Retry) => attempt = 0,
                            Some(Command::Restart) => { attempt = 0; mode = Mode::Restart; }
                            Some(Command::Stop) => {
                                attempt = 0;
                                if !self.stop_server(&host, &transport).await { return; }
                            }
                            Some(Command::Shutdown) | None => return,
                        }
                        continue;
                    }
                    let delay = backoff(attempt);
                    self.ctx.update(|s| {
                        s.phase = if s.ever_ready { Phase::Reconnecting } else { Phase::Connecting };
                        s.message = error.clone();
                        s.retry_in = Some(delay.as_secs());
                    });
                    match self.wait_for_command(Some(delay)).await {
                        Some(Command::Retry) => {}
                        Some(Command::Restart) => mode = Mode::Restart,
                        Some(Command::Stop) => {
                            if !self.stop_server(&host, &transport).await {
                                return;
                            }
                        }
                        Some(Command::Shutdown) | None => return,
                    }
                }
            }
        }
    }

    /// Shows the host and watches it until something goes wrong.
    async fn serve(&mut self, mut connection: Connection) -> Exit {
        let settings = self.ctx.app.state::<AppState>().store.lock().unwrap().settings.clone();
        let url = kimi_url(connection.port, &connection.token, &settings);
        self.ctx.log(&format!("Serving on 127.0.0.1:{}", connection.port));
        self.ctx.update(|s| {
            s.phase = Phase::Ready;
            s.message = "Connected".into();
            s.attempt = 0;
            s.retry_in = None;
            s.ever_ready = true;
        });
        self.ctx.show(&url);

        let mut ticker = tokio::time::interval(HEALTH_INTERVAL);
        ticker.tick().await;
        let mut last_tick = Instant::now();
        loop {
            let tunnel_exit = async {
                match connection.tunnel.as_mut() {
                    Some(child) => child.wait().await.ok(),
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = tunnel_exit => return Exit::Lost("SSH connection dropped.".into()),
                _ = ticker.tick() => {
                    let woke = last_tick.elapsed() > WAKE_GAP;
                    last_tick = Instant::now();
                    // One quick re-probe rides out a single dropped request.
                    let healthy = transport::probe(connection.port).await || (!woke && {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        transport::probe(connection.port).await
                    });
                    if !healthy {
                        return Exit::Lost(if woke { "Lost the connection while asleep.".into() } else { "Kimi Code stopped responding.".into() });
                    }
                }
                command = self.rx.recv() => match command {
                    Some(Command::Retry) => return Exit::Lost("Reconnect requested.".into()),
                    Some(Command::Restart) => return Exit::Restart,
                    Some(Command::Stop) => return Exit::Stop,
                    Some(Command::Shutdown) | None => return Exit::Shutdown,
                }
            }
        }
    }

    /// Returns false when the supervisor should exit.
    async fn stop_server(&mut self, host: &HostConfig, transport: &Transport) -> bool {
        self.ctx.update(|s| {
            s.phase = Phase::Connecting;
            s.message = "Stopping Kimi Code…".into();
        });
        let result = transport.run_script(&scripts::stop(host.hide_gpus), |_| {}).await;
        self.ctx.update(|s| {
            s.phase = Phase::Stopped;
            s.ever_ready = false;
            s.retry_in = None;
            s.message = match &result {
                Ok(_) => "Kimi Code was stopped on this host.".into(),
                Err(e) => format!("Could not stop Kimi Code: {e}"),
            };
        });
        self.ctx.show_connect_page();
        loop {
            match self.wait_for_command(None).await {
                Some(Command::Retry) | Some(Command::Restart) => return true,
                Some(Command::Stop) => continue,
                Some(Command::Shutdown) | None => return false,
            }
        }
    }

    async fn wait_for_command(&mut self, timeout: Option<Duration>) -> Option<Command> {
        match timeout {
            Some(delay) => tokio::time::timeout(delay, self.rx.recv()).await.unwrap_or(Some(Command::Retry)),
            None => self.rx.recv().await,
        }
    }
}

impl Ctx {
    async fn connect(&self, host: &HostConfig, transport: &Transport, mode: Mode) -> Result<Connection, String> {
        let preferred = match transport {
            Transport::Local => host.local_port.unwrap_or(DEFAULT_SERVER_PORT),
            Transport::Ssh { .. } => DEFAULT_SERVER_PORT,
        };
        let master = transport.open_master().await?;
        let script = scripts::ensure(&host.command, host.hide_gpus, preferred, mode);
        let output = transport
            .run_script(&script, |line| {
                if let Some(step) = line.strip_prefix(scripts::STEP) {
                    let step = step.trim().to_string();
                    self.log(&step);
                    self.update(|s| s.message = format!("{step}…"));
                } else if !line.starts_with(scripts::READY) && !line.starts_with(scripts::ERROR) {
                    self.log(line);
                }
            })
            .await?;
        let (server_port, token) = output
            .lines
            .iter()
            .find_map(|l| scripts::parse_ready(l))
            .ok_or_else(|| "Kimi Code did not report its address.".to_string())?;

        let (port, tunnel) = match transport {
            Transport::Local => (server_port, None),
            Transport::Ssh { host: ssh, .. } => {
                // Keep the same local port as last time whenever possible.
                let port = host
                    .local_port
                    .filter(|p| transport::port_is_free(*p))
                    .or_else(transport::free_port)
                    .ok_or("No free local port for the tunnel.")?;
                self.update(|s| s.message = format!("Opening tunnel to {ssh}…"));
                let tunnel = transport.forward(port, server_port).await?;
                (port, master.or(tunnel))
            }
        };
        self.app.state::<AppState>().store.lock().unwrap().set_local_port(&host.id, port);

        // A freshly started server can take a while to answer HTTP, but it
        // prints its URL only once listening: refusals mean it is gone.
        let deadline = Instant::now() + READY_TIMEOUT;
        let mut refused = 0;
        while Instant::now() < deadline {
            match transport::probe_detailed(port).await {
                transport::Probe::Healthy => return Ok(Connection { port, token, tunnel }),
                transport::Probe::Refused => {
                    refused += 1;
                    if refused >= 8 {
                        return Err("Kimi Code exited right after starting.".into());
                    }
                }
                transport::Probe::Unhealthy => refused = 0,
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Err(NOT_RESPONDING.into())
    }

    fn host(&self) -> Option<HostConfig> {
        self.app.state::<AppState>().store.lock().unwrap().get(&self.id).cloned()
    }

    fn log(&self, line: &str) {
        #[cfg(debug_assertions)]
        eprintln!("[{}] {line}", self.id);
        let mut status = self.status.lock().unwrap();
        status.log.push(line.to_string());
        let excess = status.log.len().saturating_sub(LOG_LINES);
        status.log.drain(..excess);
    }

    fn update(&self, f: impl FnOnce(&mut HostStatus)) {
        let snapshot = {
            let mut status = self.status.lock().unwrap();
            f(&mut status);
            status.clone()
        };
        #[cfg(debug_assertions)]
        eprintln!("[{}] {:?}: {}", snapshot.id, snapshot.phase, snapshot.message);
        let _ = self.app.emit("host-status", &snapshot);
        crate::on_status_changed(&self.app, &snapshot);
    }

    fn window(&self) -> Option<tauri::WebviewWindow> {
        self.app.get_webview_window(&crate::host_label(&self.id))
    }

    /// Navigates to the server unless the window already shows it, in which
    /// case the page stays put and reconnects its own socket.
    fn show(&self, url: &Url) {
        let Some(window) = self.window() else { return };
        let same_origin = window.url().map(|current| current.origin() == url.origin()).unwrap_or(false);
        if same_origin {
            let _ = window.eval("window.__kd ? window.__kd.reconnected() : location.reload()");
        } else {
            replace_location(&window, url);
        }
    }

    fn show_connect_page(&self) {
        if let Some(window) = self.window() {
            if let Some(url) = crate::connect_page_url(&self.id) {
                replace_location(&window, &url);
            }
        }
    }
}

/// Swaps pages without adding history: a Back gesture (or a page router
/// popping state) must never land on the connection screen.
fn replace_location(window: &tauri::WebviewWindow, url: &Url) {
    let target = serde_json::to_string(url.as_str()).unwrap_or_default();
    if window.eval(format!("location.replace({target})")).is_err() {
        let _ = window.navigate(url.clone());
    }
}

const NOT_RESPONDING: &str = "Kimi Code is running but not responding.";

fn backoff(attempt: u32) -> Duration {
    const STEPS: [u64; 7] = [1, 2, 3, 5, 10, 20, 30];
    Duration::from_secs(STEPS[(attempt.saturating_sub(1) as usize).min(STEPS.len() - 1)])
}

pub fn kimi_url(port: u16, token: &str, settings: &crate::config::Settings) -> Url {
    if !settings.desktop_integration {
        return Url::parse(&format!("http://127.0.0.1:{port}/#token={token}")).expect("valid url");
    }
    // Kimi's web UI has an opt-in desktop presentation (traffic-light
    // spacing on macOS). Unknown query flags are ignored by older builds.
    let platform = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(windows) {
        "win32"
    } else {
        "linux"
    };
    let mut flags = String::new();
    if !cfg!(target_os = "macos") {
        flags.push_str("&kimi_native_titlebar=1");
    } else if settings.vibrancy {
        flags.push_str("&kimi_vibrancy=1");
    }
    if settings.onboarded {
        flags.push_str("&kimi_onboarded=1");
    }
    Url::parse(&format!("http://127.0.0.1:{port}/?kimi_desktop=1&platform={platform}{flags}#token={token}"))
    .expect("valid url")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_capped() {
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(100), Duration::from_secs(30));
    }
}
