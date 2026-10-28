//! POSIX `sh` scripts run on the target machine (locally or over SSH).
//!
//! The only Kimi contract relied on is the CLI: `kimi web --no-open --port N`
//! prints a loopback URL carrying `#token=…`. Servers are started detached
//! (`nohup`/`setsid`) and tracked through a pidfile and log owned by this
//! app, so they survive dropped connections and can be found again on
//! reconnect without reading Kimi's internal state files.

pub const READY: &str = "__KD_READY__";
pub const STEP: &str = "__KD_STEP__";
pub const ERROR: &str = "__KD_ERROR__";
pub const FOUND: &str = "__KD_FOUND__";
pub const FETCH_BODY: &str = "__KD_BODY__";
pub const FETCH_STATUS: &str = "__KD_HTTP__";
pub const FETCH_EXIT: &str = "__KD_CURL__";

/// Text shown by the GPU tool shims. Must stay free of `$`, backticks,
/// single and double quotes: it is embedded in generated shell.
const GPU_HINT: &str = "Note: local GPUs are intentionally hidden from this session. To run GPU work, request GPUs through Slurm (e.g. srun --gres=gpu:1 <command> or sbatch with --gres=gpu:1).";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Reuse the tracked server when it is alive, otherwise start one.
    Ensure,
    /// Stop the tracked server (if any) and start a fresh one.
    Restart,
}

fn variant(hide_gpus: bool) -> &'static str {
    if hide_gpus {
        "server-nogpu"
    } else {
        "server"
    }
}

fn prelude(hide_gpus: bool) -> String {
    format!(
        r#"umask 077
state="${{XDG_STATE_HOME:-$HOME/.local/state}}/kimi-lite"
mkdir -p "$state" || {{ echo "{ERROR} cannot create $state"; exit 1; }}
log="$state/{name}.log"
pidf="$state/{name}.pid"
url_of() {{
  [ -f "$log" ] || return 0
  tr -d '\033' < "$log" | grep -Eo 'https?://(127\.0\.0\.1|localhost|\[::1\]):[0-9]+/?#token=[A-Za-z0-9._~-]+' | tail -n 1
}}
started_at() {{ ps -o lstart= -p "$1" 2>/dev/null | tr -s ' '; }}
alive() {{
  [ -f "$pidf" ] || return 1
  pid="$(sed -n 1p "$pidf" 2>/dev/null)"
  [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null || return 1
  # Pids get reused: compare the start time recorded at launch (Kimi
  # renames its process title, so its arguments cannot be matched).
  recorded="$(sed -n 2p "$pidf" 2>/dev/null)"
  [ -z "$recorded" ] || [ "$recorded" = "$(started_at "$pid")" ]
}}
# Listening sockets only (netstat -an also lists TIME_WAIT and friends).
listening() {{
  {{ ss -ltn 2>/dev/null || netstat -an 2>/dev/null | grep -i listen || lsof -nP -iTCP -sTCP:LISTEN 2>/dev/null; }} | grep -Eq "[:.]$1[[:space:]]"
}}
stop_server() {{
  if alive; then
    kill "$pid" 2>/dev/null
    i=0
    while [ $i -lt 20 ] && kill -0 "$pid" 2>/dev/null; do sleep 0.25; i=$((i + 1)); done
    kill -9 "$pid" 2>/dev/null
  fi
  rm -f "$pidf"
}}
"#,
        name = variant(hide_gpus)
    )
}

/// Sets `bin` to the Kimi Code executable, or leaves it empty.
fn find_kimi(command: &str) -> (String, String) {
    let command = command.strip_prefix("~/").map(|rest| format!("$HOME/{rest}")).unwrap_or_else(|| command.to_string());
    let lookup = if command.contains('/') {
        format!(r#"bin="{command}""#)
    } else {
        // Non-interactive shells often miss user-level installs on PATH.
        format!(
            r#"bin="$(command -v {command} 2>/dev/null || true)"
for c in "${{KIMI_CODE_HOME:-$HOME/.kimi-code}}/bin/{command}" "$HOME/.local/bin/{command}"; do
  [ -n "$bin" ] && break
  [ -x "$c" ] && bin="$c"
done"#
        )
    };
    (command, lookup)
}

/// Whether this host has the session, asked through Kimi's own CLI. Only a
/// yes or no leaves the host. `session` must be plain characters.
pub fn has_session(command: &str, session: &str) -> String {
    let (command, lookup) = find_kimi(command);
    format!(
        r#"{lookup}
if [ -z "$bin" ] || [ ! -x "$bin" ]; then echo "{ERROR} Could not find '{command}' on this host."; exit 127; fi
if "$bin" session list --all --archived --json 2>/dev/null | grep -Eq '"id": *"{session}"'; then
  echo "{FOUND} yes"
else
  echo "{FOUND} no"
fi
"#
    )
}

pub fn ensure(command: &str, hide_gpus: bool, preferred_port: u16, mode: Mode) -> String {
    let (command, lookup) = find_kimi(command);
    let (gpu_setup, env_prefix) = if hide_gpus { (gpu_mask_setup(), GPU_ENV_PREFIX) } else { (String::new(), "") };
    let restart = if mode == Mode::Restart { "stop_server" } else { "" };
    format!(
        r#"{prelude}
{restart}
if alive; then
  u="$(url_of)"
  p="$(printf '%s' "$u" | sed -E 's#^.*:([0-9]+)/.*$#\1#')"
  # After SIGTERM Kimi closes its socket but may linger for a long time:
  # only a server that still listens is worth reusing.
  if [ -n "$u" ] && listening "$p"; then echo "{READY} $pid $u"; exit 0; fi
  stop_server
fi
{lookup}
if [ -z "$bin" ] || [ ! -x "$bin" ]; then
  echo "{ERROR} Could not find '{command}' on PATH or in ~/.kimi-code/bin. Install Kimi Code on this machine first."
  exit 127
fi
port={preferred_port}
i=0
while [ $i -lt 50 ] && listening "$port"; do port=$((port + 1)); i=$((i + 1)); done
echo "{STEP} Starting Kimi Code on port $port"
{gpu_setup}
detach=""
command -v setsid >/dev/null 2>&1 && detach="setsid"
: > "$log"
nohup $detach {env_prefix}"$bin" web --no-open --port "$port" > "$log" 2>&1 < /dev/null &
pid=$!
{{ echo "$pid"; started_at "$pid"; }} > "$pidf"
n=0
while [ $n -lt 120 ]; do
  u="$(url_of)"
  if [ -n "$u" ]; then echo "{READY} $pid $u"; exit 0; fi
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "{ERROR} Kimi Code exited during startup:"
    tail -n 15 "$log"
    rm -f "$pidf"
    exit 1
  fi
  sleep 0.5
  n=$((n + 1))
done
echo "{ERROR} Timed out waiting for Kimi Code to start:"
tail -n 15 "$log"
exit 1
"#,
        prelude = prelude(hide_gpus),
    )
}

pub fn stop(hide_gpus: bool) -> String {
    format!("{}\nstop_server\necho \"{READY} stopped\"\n", prelude(hide_gpus))
}

/// Empty device lists make CUDA/ROCm unusable; the shim dir shadows
/// nvidia-smi/rocm-smi unless a scheduler allocation re-exposed GPUs.
const GPU_ENV_PREFIX: &str = r#"env CUDA_VISIBLE_DEVICES= NVIDIA_VISIBLE_DEVICES= HIP_VISIBLE_DEVICES= ROCR_VISIBLE_DEVICES= PATH="$maskpath" "#;

fn gpu_mask_setup() -> String {
    format!(
        r#"maskdir="${{TMPDIR:-/tmp}}/kimi-gpu-mask-$(id -u 2>/dev/null || echo user)"
mkdir -m 700 -p "$maskdir" 2>/dev/null || maskdir=""
if [ -n "$maskdir" ] && [ -O "$maskdir" ]; then
  chmod 700 "$maskdir" 2>/dev/null
  for tool in nvidia-smi rocm-smi; do
    real="$(command -v "$tool" 2>/dev/null || true)"
    case "$real" in "$maskdir"/*|"") continue ;; esac
    printf '%s\n' '#!/bin/sh' \
      'if [ -n "$CUDA_VISIBLE_DEVICES" ] || [ -n "$ROCR_VISIBLE_DEVICES" ]; then' \
      "exec \"$real\" \"\$@\"" 'fi' 'echo "No devices were found"' 'echo "{GPU_HINT}"' 'exit 0' > "$maskdir/$tool"
    chmod 755 "$maskdir/$tool"
  done
else
  maskdir=""
fi
maskpath="${{maskdir:+$maskdir:}}$PATH""#
    )
}

/// GETs a URL with curl. `config` is a base64-encoded curl config (`-K`), so
/// headers such as API keys stay off command lines; the body follows a marker
/// line because login shells may print their own output first.
pub fn fetch(config: &str) -> String {
    format!(
        r#"command -v curl >/dev/null 2>&1 || {{ echo "{ERROR} curl is not installed on this host."; exit 1; }}
echo {FETCH_BODY}
printf '%s' '{config}' | base64 -d | curl -K -
echo "{FETCH_EXIT} $?"
"#
    )
}

/// Parses `__KD_READY__ <pid> <url>` into the server port and token.
pub fn parse_ready(line: &str) -> Option<(u16, String)> {
    let rest = line.strip_prefix(READY)?.trim();
    let url = rest.split_whitespace().nth(1)?;
    let (base, token) = url.split_once("#token=")?;
    let port = base.trim_end_matches('/').rsplit(':').next()?.parse().ok()?;
    (!token.is_empty()).then(|| (port, token.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ready_line() {
        let (port, token) = parse_ready("__KD_READY__ 4242 http://127.0.0.1:58627/#token=abc_-9").unwrap();
        assert_eq!(port, 58627);
        assert_eq!(token, "abc_-9");
        assert!(parse_ready("__KD_READY__ 1 http://127.0.0.1:1/").is_none());
    }

    #[test]
    fn scripts_parse_with_sh() {
        for script in [
            ensure("kimi", false, 58627, Mode::Ensure),
            ensure("~/bin/kimi", true, 58627, Mode::Restart),
            stop(true),
            fetch("Zm9vCg=="),
            has_session("kimi", "session_2e802548-0f7b-4fc1-aafc-9cdcf9a8d768"),
        ] {
            let status = std::process::Command::new("sh").args(["-n", "-c", &script]).status().unwrap();
            assert!(status.success(), "{script}");
        }
    }
}

