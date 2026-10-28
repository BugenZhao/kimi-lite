//! Running scripts and opening tunnels on a host, through the user's own
//! OpenSSH (so `~/.ssh/config`, ProxyJump, agents and keys all just work).

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};

use crate::scripts;

const SCRIPT_TIMEOUT: Duration = Duration::from_secs(90);
const TUNNEL_TIMEOUT: Duration = Duration::from_secs(30);
/// Past this, a connection is most likely waiting on authentication.
const SLOW_CONNECT: Duration = Duration::from_secs(8);

#[derive(Clone)]
pub enum Transport {
    Local,
    /// `control` is the multiplexing socket of the master this app owns.
    Ssh { host: String, control: PathBuf },
}

/// An ssh process the app owns: the master (or a Windows tunnel). Dropping
/// it kills the process; its lifeline pipe is held apart from the `Child`,
/// because `Child::wait` closes stdin before waiting.
pub struct SshProcess {
    child: Child,
    _lifeline: Option<ChildStdin>,
    /// The master's control socket: ssh cannot unlink it when killed.
    control: Option<PathBuf>,
}

impl Drop for SshProcess {
    fn drop(&mut self) {
        if let Some(control) = &self.control {
            let _ = std::fs::remove_file(control);
        }
    }
}

impl SshProcess {
    fn new(mut child: Child, control: Option<PathBuf>) -> Self {
        let lifeline = child.stdin.take();
        Self { child, _lifeline: lifeline, control }
    }

    /// Resolves when the process exits.
    pub async fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.wait().await.ok()
    }
}

pub struct ScriptResult {
    pub lines: Vec<String>,
}

impl Transport {
    /// Runs `script` with `sh` under the user's login shell, streaming marker
    /// lines (`__KD_STEP__`, …) and ssh diagnostics to `on_line`.
    pub async fn run_script(&self, script: &str, mut on_line: impl FnMut(&str)) -> Result<ScriptResult, String> {
        let mut cmd = match self {
            // Login + interactive picks up PATH from .zshrc/.bashrc too, like
            // a terminal would; GUI apps otherwise start with a bare PATH.
            // The script travels in an env var: interactive shell startup
            // (prompt frameworks and the like) may consume stdin.
            Transport::Local => {
                let mut cmd = Command::new("/bin/sh");
                cmd.args(["-c", r#"exec "${SHELL:-/bin/sh}" -l -i -c 'exec sh -c "$KD_SCRIPT"'"#])
                    .env("KD_SCRIPT", format!("unset KD_SCRIPT\n{script}"))
                    // No tty here; a TERM keeps prompt themes (tput) quiet.
                    .env("TERM", std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into()));
                cmd
            }
            Transport::Ssh { host, control } => {
                let mut cmd = ssh_command(control);
                // Parsed by the remote login shell, whichever it is (fish too).
                cmd.args(["-T", host, r#"sh -c 'exec "${SHELL:-/bin/sh}" -l -c "exec sh -s"'"#]);
                cmd
            }
        };
        let via_stdin = matches!(self, Transport::Ssh { .. });
        cmd.stdin(if via_stdin { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| format!("Could not run {}: {e}", self.program()))?;

        if let Some(mut stdin) = child.stdin.take() {
            let script = script.to_string();
            tokio::spawn(async move {
                let _ = stdin.write_all(script.as_bytes()).await;
                let _ = stdin.shutdown().await;
            });
        }

        let mut out = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut err = BufReader::new(child.stderr.take().unwrap()).lines();
        let mut lines = Vec::new();
        let mut diagnostics = Vec::new();
        let collect = async {
            let (mut out_done, mut err_done) = (false, false);
            while !(out_done && err_done) {
                tokio::select! {
                    line = out.next_line(), if !out_done => match line {
                        Ok(Some(line)) => { on_line(&line); lines.push(line); }
                        _ => out_done = true,
                    },
                    line = err.next_line(), if !err_done => match line {
                        Ok(Some(line)) => {
                            if is_diagnostic(&line) { on_line(&line); diagnostics.push(line); }
                        }
                        _ => err_done = true,
                    },
                }
            }
            child.wait().await
        };
        let status = tokio::time::timeout(SCRIPT_TIMEOUT, collect)
            .await
            .map_err(|_| "Timed out talking to the host.".to_string())?
            .map_err(|e| e.to_string())?;

        if let Some(error) = lines.iter().find_map(|l| l.strip_prefix(scripts::ERROR)) {
            let detail: Vec<_> = lines.iter().skip_while(|l| !l.starts_with(scripts::ERROR)).skip(1).cloned().collect();
            return Err(format!("{}{}", error.trim(), if detail.is_empty() { String::new() } else { format!("\n{}", detail.join("\n")) }));
        }
        if !status.success() && !lines.iter().any(|l| l.starts_with(scripts::READY)) {
            let reason = diagnostics.last().cloned().unwrap_or_else(|| match status.code() {
                Some(255) => "SSH connection failed.".into(),
                Some(code) => format!("Exited with code {code}."),
                None => "Terminated.".into(),
            });
            return Err(reason);
        }
        Ok(ScriptResult { lines })
    }

    pub fn ssh(host: &str, id: &str) -> Self {
        Transport::Ssh { host: host.to_string(), control: control_dir().join(id) }
    }

    /// Opens the authenticated master connection every other ssh call rides
    /// on (one password/2FA prompt per connect). Its exit means the
    /// connection is gone. On Windows, where OpenSSH cannot multiplex, there
    /// is no master and each call connects on its own.
    ///
    /// The master's session runs `cat` on the host, reading a pipe this app
    /// holds: if the app goes away for any reason (quit, crash, SIGKILL),
    /// the pipe closes, `cat` ends, and the master and its forwards go with
    /// it instead of lingering as orphans.
    pub async fn open_master(&self, on_slow: impl FnOnce()) -> Result<Option<SshProcess>, String> {
        let Transport::Ssh { host, control } = self else { return Ok(None) };
        if cfg!(windows) {
            return Ok(None);
        }
        let _ = std::fs::remove_file(control);
        let mut child = base_ssh_command()
            .args(["-M", "-T", "-o", "ControlMaster=yes", "-o", "ControlPersist=no", "-S"])
            .arg(control)
            .args([host.as_str(), "cat >/dev/null"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("Could not start ssh: {e}"))?;
        let started = tokio::time::Instant::now();
        let mut on_slow = Some(on_slow);
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                return Err(exit_reason(&mut child, status).await);
            }
            if control.exists() && self.control(&["-O", "check"]).await {
                return Ok(Some(SshProcess::new(child, Some(control.clone()))));
            }
            if started.elapsed() > SLOW_CONNECT {
                if let Some(notify) = on_slow.take() {
                    notify();
                }
            }
            if started.elapsed() > SCRIPT_TIMEOUT {
                return Err("Timed out connecting.".into());
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    /// Forwards `127.0.0.1:local_port` to the remote loopback `remote_port`:
    /// through the master when there is one (it lives and dies with it),
    /// otherwise as a dedicated tunnel process that is returned.
    pub async fn forward(&self, local_port: u16, remote_port: u16) -> Result<Option<SshProcess>, String> {
        let Transport::Ssh { host, control } = self else { return Err("Local hosts need no tunnel.".into()) };
        let spec = format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}");
        let mut tunnel = None;
        if cfg!(windows) {
            let child = ssh_command(control)
                .args(["-N", "-T", "-o", "ExitOnForwardFailure=yes", "-L", &spec, host])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .map_err(|e| format!("Could not start ssh: {e}"))?;
            tunnel = Some(child);
        } else if !self.control(&["-O", "forward", "-L", &spec]).await {
            return Err(format!("Could not forward local port {local_port}."));
        }

        let deadline = tokio::time::Instant::now() + TUNNEL_TIMEOUT;
        loop {
            if let Some(child) = tunnel.as_mut() {
                if let Ok(Some(status)) = child.try_wait() {
                    return Err(exit_reason(child, status).await);
                }
            }
            if tokio::net::TcpStream::connect(("127.0.0.1", local_port)).await.is_ok() {
                return Ok(tunnel.map(|child| SshProcess::new(child, None)));
            }
            if tokio::time::Instant::now() > deadline {
                return Err("SSH tunnel did not come up in time.".into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Runs `ssh -S <control> <args> host`, a control command for the master.
    async fn control(&self, args: &[&str]) -> bool {
        let Transport::Ssh { host, control } = self else { return false };
        let child = base_ssh_command()
            .arg("-S")
            .arg(control)
            .args(args)
            .arg(host)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn();
        let Ok(mut child) = child else { return false };
        matches!(
            tokio::time::timeout(Duration::from_secs(10), child.wait()).await,
            Ok(Ok(status)) if status.success()
        )
    }

    fn program(&self) -> &'static str {
        match self {
            Transport::Local => "the login shell",
            Transport::Ssh { .. } => "ssh",
        }
    }
}

fn is_diagnostic(line: &str) -> bool {
    let line = line.trim();
    !line.is_empty() && !line.starts_with("Warning: Permanently added")
}

async fn exit_reason(child: &mut Child, status: std::process::ExitStatus) -> String {
    use tokio::io::AsyncReadExt;
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = tokio::time::timeout(Duration::from_secs(1), pipe.read_to_string(&mut stderr)).await;
    }
    let last = stderr.lines().filter(|l| is_diagnostic(l)).last().unwrap_or("").trim().to_string();
    if last.is_empty() {
        format!("SSH exited ({status}).")
    } else {
        last
    }
}

/// A client call: rides the app's master when it is up, else connects.
fn ssh_command(control: &std::path::Path) -> Command {
    let mut cmd = base_ssh_command();
    if !cfg!(windows) {
        cmd.args(["-o", "ControlMaster=no", "-S"]).arg(control);
    }
    cmd
}

fn base_ssh_command() -> Command {
    let mut cmd = Command::new(ssh_binary());
    cmd.args([
        "-C",
        "-o", "ServerAliveInterval=10",
        "-o", "ServerAliveCountMax=3",
        "-o", "TCPKeepAlive=yes",
        "-o", "ConnectTimeout=20",
    ]);
    if let Some(askpass) = askpass() {
        cmd.env("SSH_ASKPASS", askpass).env("SSH_ASKPASS_REQUIRE", "force").env("DISPLAY", ":0");
    }
    cmd
}

fn ssh_binary() -> PathBuf {
    // Git's MSYS ssh stalls on bulk forwarded traffic; prefer Win32-OpenSSH.
    if cfg!(windows) {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let system = PathBuf::from(root).join(r"System32\OpenSSH\ssh.exe");
        if system.exists() {
            return system;
        }
    }
    PathBuf::from("ssh")
}

/// Unix socket paths are length-limited, so keep the control dir short.
fn control_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
        let dir = PathBuf::from(format!("/tmp/kimi-lite-{user}"));
        let _ = std::fs::create_dir_all(&dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        dir
    })
    .clone()
}

/// Clears what an earlier run that did not exit cleanly may have left:
/// orphaned ssh masters of ours (they would hold the hosts' stable ports)
/// and their stale control sockets.
pub fn collect_orphans() {
    if cfg!(windows) {
        return;
    }
    let dir = control_dir();
    let marker = format!("-S {}/", dir.display());
    if let Ok(out) = std::process::Command::new("ps").args(["-axo", "pid=,ppid=,args="]).output() {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let mut fields = line.split_whitespace();
            let (Some(pid), Some(ppid)) = (fields.next(), fields.next()) else { continue };
            let args = fields.collect::<Vec<_>>().join(" ");
            if ppid == "1" && args.starts_with("ssh ") && args.contains(&marker) {
                let _ = std::process::Command::new("kill").arg(pid).status();
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

static ASKPASS: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Installs a native password/passphrase prompt for ssh (macOS only).
pub fn install_askpass(dir: &std::path::Path) {
    ASKPASS.get_or_init(|| {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let path = dir.join("askpass.sh");
        let script = r#"#!/bin/sh
# ssh calls this with the prompt as $1; answers go to stdout.
prompt="${1:-SSH}"
case "$prompt" in
  *"(yes/no"*) buttons='buttons {"Cancel", "Connect"} default button "Connect"' ; hidden="" ; answer="yes" ;;
  *) buttons='buttons {"Cancel", "OK"} default button "OK"' ; hidden="with hidden answer" ; answer="" ;;
esac
if [ -n "$answer" ]; then
  /usr/bin/osascript -e 'on run argv' -e "display dialog (item 1 of argv) with title \"Kimi Lite\" with icon caution $buttons" -e 'end run' "$prompt" >/dev/null || exit 1
  echo "$answer"
else
  /usr/bin/osascript -e 'on run argv' -e "text returned of (display dialog (item 1 of argv) with title \"Kimi Lite\" default answer \"\" $hidden $buttons)" -e 'end run' "$prompt"
fi
"#;
        std::fs::create_dir_all(dir).ok()?;
        std::fs::write(&path, script).ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).ok()?;
        }
        Some(path)
    });
}

fn askpass() -> Option<PathBuf> {
    ASKPASS.get().cloned().flatten()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Healthy,
    /// Nothing is listening at all.
    Refused,
    /// Listening, but no good answer (yet).
    Unhealthy,
}

/// `GET /` over plain TCP: the cheapest end-to-end check through the tunnel
/// that does not depend on any Kimi API route.
pub async fn probe(port: u16) -> bool {
    probe_detailed(port).await == Probe::Healthy
}

pub async fn probe_detailed(port: u16) -> Probe {
    use tokio::io::AsyncReadExt;
    let stream = match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
        Ok(stream) => stream,
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => return Probe::Refused,
        Err(_) => return Probe::Unhealthy,
    };
    let attempt = async move {
        let mut stream = stream;
        let request = format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.ok()?;
        let mut head = [0u8; 16];
        let mut read = 0;
        while read < 12 {
            let n = stream.read(&mut head[read..]).await.ok()?;
            if n == 0 {
                break;
            }
            read += n;
        }
        let head = std::str::from_utf8(&head[..read]).ok()?;
        Some(head.starts_with("HTTP/1.") && head[8..].trim_start().starts_with('2'))
    };
    match tokio::time::timeout(Duration::from_secs(5), attempt).await {
        Ok(Some(true)) => Probe::Healthy,
        _ => Probe::Unhealthy,
    }
}

pub fn free_port() -> Option<u16> {
    std::net::TcpListener::bind(("127.0.0.1", 0)).ok()?.local_addr().ok().map(|a| a.port())
}

pub fn port_is_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Against a real host, opt-in:
/// `KL_TEST_HOST=<ssh host> cargo test --lib live_ -- --ignored --nocapture`
#[cfg(test)]
mod live_tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn live_master_survives_idle_and_dies_with_its_owner() {
        let Ok(host) = std::env::var("KL_TEST_HOST") else { return };
        let transport = Transport::ssh(&host, "live-test");
        let mut master = transport.open_master(|| eprintln!("waiting for auth…")).await.unwrap().unwrap();

        // The forward works: talk to a remote loopback service through it
        // (the host's sshd by default, or KL_TEST_PORT, e.g. an HTTP server).
        let remote: u16 = std::env::var("KL_TEST_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(22);
        let port = free_port().unwrap();
        assert!(transport.forward(port, remote).await.unwrap().is_none());
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(b"GET / HTTP/1.0\r\n\r\n").await.unwrap();
        let mut head = [0u8; 4];
        use tokio::io::AsyncReadExt;
        stream.read_exact(&mut head).await.unwrap();
        assert!(&head == b"SSH-" || &head == b"HTTP", "unexpected reply {head:?}");
        drop(stream);

        // Watched exactly like the supervisor does, an idle master stays up.
        let idle = tokio::time::timeout(Duration::from_secs(20), master.exited()).await;
        assert!(idle.is_err(), "master exited while idle: {idle:?}");

        // Its owner going away closes the lifeline: the master follows, and
        // the forwarded port is released.
        drop(master._lifeline.take());
        let gone = tokio::time::timeout(Duration::from_secs(20), master.child.wait()).await;
        assert!(gone.is_ok(), "master outlived its lifeline");
        assert!(port_is_free(port), "forward still listening");
        eprintln!("master exited after lifeline closed: {:?}", gone.unwrap());
    }
}
