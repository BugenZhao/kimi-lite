//! Running scripts and opening tunnels on a host, through the user's own
//! OpenSSH (so `~/.ssh/config`, ProxyJump, agents and keys all just work).

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

use crate::scripts;

const SCRIPT_TIMEOUT: Duration = Duration::from_secs(90);
const TUNNEL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub enum Transport {
    Local,
    /// `control` is the multiplexing socket of the master this app owns.
    Ssh { host: String, control: PathBuf },
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
    pub async fn open_master(&self) -> Result<Option<Child>, String> {
        let Transport::Ssh { host, control } = self else { return Ok(None) };
        if cfg!(windows) {
            return Ok(None);
        }
        let _ = std::fs::remove_file(control);
        let mut child = base_ssh_command()
            .args(["-M", "-N", "-T", "-o", "ControlMaster=yes", "-o", "ControlPersist=no", "-S"])
            .arg(control)
            .arg(host)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("Could not start ssh: {e}"))?;
        let deadline = tokio::time::Instant::now() + SCRIPT_TIMEOUT;
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                return Err(exit_reason(&mut child, status).await);
            }
            if control.exists() && self.control(&["-O", "check"]).await {
                return Ok(Some(child));
            }
            if tokio::time::Instant::now() > deadline {
                return Err("Timed out connecting.".into());
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    /// Forwards `127.0.0.1:local_port` to the remote loopback `remote_port`:
    /// through the master when there is one (it lives and dies with it),
    /// otherwise as a dedicated tunnel process that is returned.
    pub async fn forward(&self, local_port: u16, remote_port: u16) -> Result<Option<Child>, String> {
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
                return Ok(tunnel);
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
        let dir = PathBuf::from(format!("/tmp/kd-{user}"));
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
  /usr/bin/osascript -e 'on run argv' -e "display dialog (item 1 of argv) with title \"Kimi Desktop\" with icon caution $buttons" -e 'end run' "$prompt" >/dev/null || exit 1
  echo "$answer"
else
  /usr/bin/osascript -e 'on run argv' -e "text returned of (display dialog (item 1 of argv) with title \"Kimi Desktop\" default answer \"\" $hidden $buttons)" -e 'end run' "$prompt"
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

/// `GET /` over plain TCP: the cheapest end-to-end check through the tunnel
/// that does not depend on any Kimi API route.
pub async fn probe(port: u16) -> bool {
    use tokio::io::AsyncReadExt;
    let attempt = async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.ok()?;
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
    tokio::time::timeout(Duration::from_secs(5), attempt).await.ok().flatten().unwrap_or(false)
}

pub fn free_port() -> Option<u16> {
    std::net::TcpListener::bind(("127.0.0.1", 0)).ok()?.local_addr().ok().map(|a| a.port())
}

pub fn port_is_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}
