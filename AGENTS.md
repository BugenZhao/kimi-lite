# AGENTS.md

Notes for anyone (human or agent) working on Kimi Lite's code.

## Commands

```sh
pnpm install
pnpm dev                                   # run unbundled (separate WebView storage)
pnpm build                                 # release .app
pnpm tauri build --debug --bundles app     # debug .app (stderr logging of status changes)
cd src-tauri && cargo test                 # unit tests, incl. `sh -n` on generated scripts
```

`pnpm dev` runs an unbundled binary, which gets its own WebView data store. Use a bundled build (`pnpm build` or `--debug --bundles app`) to test anything that involves storage, onboarding, notifications or the Dock.

## Layout

```text
src-tauri/src/
  lib.rs         windows, IPC commands, navigation/download/notification policy
  supervisor.rs  per-host state machine: connect → serve → health → reconnect
  transport.rs   local login shell / OpenSSH master + forwards, askpass, probes, orphan cleanup
  scripts.rs     POSIX sh run on hosts: ensure / stop the server, GPU mask
  bridge.js      injected into Kimi pages: notifications, desktop hooks, glass, cursor, drag strip,
                 host chip, reconnect banner
  chrome.rs      traffic-light placement (AppKit), host color dots, palette
  menu.rs        menu bar and the host switcher popup
  config.rs      hosts.json and settings
  ssh_config.rs  Host aliases from ~/.ssh/config (follows Include)
src-tauri/capabilities/
  app.json       what the app's own pages may call
  kimi.json      what Kimi's page (loopback origin) may call: keep it minimal
ui/              Hosts window and connection screen (plain HTML/CSS, no build step)
scripts/         leakcheck.sh
```

## Principles

**Couple to Kimi Code as little as possible.** It changes fast. Allowed:

| Contract | Notes |
| --- | --- |
| `kimi web --no-open --port N` prints `http://127.0.0.1:N/#token=…` | The only CLI contract |
| `GET /` answers 2xx | Health probe |
| Web standards: `Notification`, the `online` event | Polyfilled or dispatched by the bridge |
| `?kimi_desktop=1&platform=…&kimi_vibrancy=1&kimi_onboarded=1` and `window.kimiDesktop` hooks (`onMenuAction`, `showWindow`, `setTheme`, `getVibrancy`/`setVibrancy`, `setOnboarded`, `onUpdateStatus`/`getUpdateStatus`) | Kimi's own opt-in desktop presentation. Once `kimiDesktop` exists, the page calls `onUpdateStatus`/`getUpdateStatus` unconditionally, so they must exist. |
| Frosted-sidebar shim: `--color-sidebar-bg`, `#app`, `.app`, `.side` | Cosmetic, applied only once `#app .app` exists; degrades to opaque |
| `cursor: pointer` rewrite in same-origin stylesheets | Cosmetic, no class names |

Not allowed: reading `~/.kimi-code` internals (server registry, tokens, session files), private API routes, or the page's framework state (Pinia and the like). The **Seamless title bar** setting must keep working as an escape hatch: off means plain web UI, no hooks.

**Servers belong to the host, not the window.** `kimi web` runs detached (`nohup`, plus `setsid` when available) and is tracked with a pidfile and log under `${XDG_STATE_HOME:-~/.local/state}/kimi-lite/` on each host. A tracked server is reused only if its pid, start time *and* listening port all check out: Kimi can linger for a long time after SIGTERM with its socket closed. Only an explicit Stop kills it.

**Stable origins.** Each host keeps its local port (`localPort` in hosts.json), so the page keeps its storage and its WebSocket can simply reconnect. Pages are swapped with `location.replace`, so Back never lands on the connection screen.

**Look like Kimi.** `ui/style.css` mirrors Kimi's design tokens (font, grays, `ui-button`/`ui-input`/`ui-switch`, settings groups). When Kimi's look changes, re-derive the tokens from its served CSS rather than inventing new ones. Use the arrow cursor (`cursor: default`), never the hand.

## SSH lifecycle (read before touching transport.rs)

- Per host, the app owns one master: `ssh -M -T -S <control> host 'cat >/dev/null'`. Scripts run as mux clients (`ControlMaster=no`), and the tunnel is added with `ssh -O forward`. One authentication per connect.
- The master's stdin is a **lifeline**: the remote `cat` exits when the app's end of the pipe closes, so the master dies with the app even on SIGKILL. Keep the `ChildStdin` out of the `Child` (`SshProcess`): tokio's `Child::wait()` closes stdin before waiting, which once made every connection drop right after it came up.
- Master exit means the connection is gone. Otherwise a 5s `GET /` probe (with one quick re-probe) and a tick-gap check for sleep catch hangs.
- `collect_orphans()` at launch kills masters left by older builds (parent PID 1, our control path) and removes stale sockets. Dropping an `SshProcess` removes its control socket.
- The control directory is `/tmp/kimi-lite-$USER` (Unix socket paths are length-limited).
- Windows has no multiplexing: the tunnel is a separate `ssh -N -L` process there.

## macOS quirks

- **Traffic lights:** tao re-applies a custom inset only from a `drawRect:` that the WebView covers, and AppKit resets the buttons on title changes, resizes and focus changes. `chrome::align_traffic_lights` places them itself, centered on Kimi's 48px header, and repeats after AppKit's lazy layout passes. Keep the title bar container **narrow** (buttons only): a full-width one sits above the page and swallows clicks on Kimi's header. Don't change window titles for transient state.
- **WKWebView ignores `-webkit-app-region`.** The bridge implements a drag strip over the top 48px. It skips interactive elements and anything whose `cursor: pointer` was rewritten.
- **`strip` is off in release** (`Cargo.toml`): the system `strip` on recent macOS produces proc-macro dylibs that dyld refuses to load.
- **No Web Notifications in WKWebView:** the bridge polyfills `Notification` through the notification plugin.
- **Native window tabs don't work** with the transparent, full-size-content title bar.

## Testing

- `cargo test`: unit tests, plus a `sh -n` syntax check of every generated script.
- **Live SSH lifecycle (opt-in):** checks that the master stays up while idle, that the forward works, and that the master exits when its lifeline closes.

  ```sh
  KL_TEST_HOST=<ssh host> [KL_TEST_PORT=<remote loopback port>] cargo test --lib live_ -- --ignored --nocapture
  ```

  Agents that require per-app approval (e.g. 1Password) may refuse the test binary; use a host with plain key auth.
- **Leaks:** `scripts/leakcheck.sh <ssh host>` snapshots the app's memory and file descriptors, ssh processes, control sockets and forwards, plus the remote server, state files and sshd sessions. Counts must not grow across faults.
- **Fault drills** before touching connection code, with a host open:
  - kill the master (dropped network);
  - `kill -STOP` the master (hung network);
  - `kill -9` and `kill -TERM` the remote server;
  - `kill -STOP` the app for 25s while dropping the master (sleep);
  - Stop and Start;
  - close and reopen the window;
  - Quit and relaunch;
  - `kill -9` the app.

  Each should recover on the same local port, reuse the server where one exists, settle on a single master, and leave nothing behind after Quit or a crash.

## Conventions

- Rust: small modules, comments explain *why*. Keep the IPC surface for Kimi's page (`capabilities/kimi.json`, `bridge.js`) minimal and declared in `build.rs`.
- Generated shell must stay POSIX `sh` and pass `sh -n`. Strings embedded into scripts must not contain quotes or `$`.
- Commit messages: imperative subject, body explaining the why.

## Releases

`.github/workflows/release.yml` builds on an Apple Silicon runner (`macos-15`): tests, then the `.app` and `.dmg`, ad-hoc signed (`APPLE_SIGNING_IDENTITY=-`; there is no Developer ID or notarization yet). To release, bump `version` in `src-tauri/tauri.conf.json` (and `Cargo.toml`, `package.json`), commit, then push a matching tag:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

The workflow refuses a tag that doesn't match the app version, and drafts a GitHub release with the `.dmg`, a zipped `.app` and `SHA256SUMS`. Review and publish the draft by hand. Running the workflow manually (Actions → Release → Run workflow) builds the same files as a workflow artifact without a release.

## Icons

The PNG and ICO files in `src-tauri/icons/` were generated from `src-tauri/icons/source.png` with `pnpm tauri icon`. `icon.icns` ships as-is, and that command overwrites it, so restore it afterwards (`git checkout src-tauri/icons/icon.icns`).
