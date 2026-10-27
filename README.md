# Kimi Lite

A lightweight native shell for [Kimi Code](https://github.com/MoonshotAI/kimi-code)'s web UI, on this computer or on any SSH host. It's the successor to `kimi-desktop`, rebuilt on Tauri (system WebView, no Electron).

## What's different

- **Seamless instead of wrapped.** Each host is its own native window, and the Kimi UI fills the whole window. Kimi's opt-in desktop presentation (`?kimi_desktop=1`) puts its header in the title bar next to the traffic lights. There is no shell sidebar and no second visual style. Hosts are switched from the Window menu (⌘1…⌘9, alongside the standard window list and tiling commands) or the Hosts window (⌘⇧O).
- **Reconnects for real.** Servers run detached on the host (`nohup`/`setsid`), so a dropped connection, a sleeping laptop or a closed window doesn't stop running agents. Each host keeps a **stable local port**, so after a reconnect the page keeps its origin, storage and state. The app tells the page with a standard `online` event, and the page reopens its own socket without a reload.
- **Fast failure detection.** The app owns an SSH master connection (`ssh -M`). If it exits, the app knows immediately. A `GET /` health probe every 5s (plus wake-from-sleep detection) catches everything else. Backoff is 1, 2, 3, 5, 10, 20, then 30s, forever, with *Retry now* in a small banner.
- **One SSH login per connect.** Scripts and port forwards share the master. On macOS, password, passphrase and host-key prompts appear as native dialogs (`SSH_ASKPASS`).
- **Native bits.** Real menu bar, Edit menu (copy/paste), zoom, file drops into the page, native notifications (with a Dock badge while the app is in the background), downloads to `~/Downloads`, external links in the default browser, and windows restored on relaunch.
- **Glass and native styling.** On macOS the sidebar sits on the system's frosted-glass material, as in the official app. The Hosts window, connection screen and reconnect banner use Kimi's own font (Schibsted Grotesk), grays, buttons and switches, so they read as part of Kimi.
- **Onboarding once.** Each host is its own origin, so Kimi would show its welcome screens for every new host. After you finish them once, every host is told so.
- **Small.** The release bundle is about 6 MB (it uses the system WebView instead of bundling Chromium).

## Coupling to Kimi Code (kept deliberately thin)

Kimi Code moves fast, so the app relies on as little of it as possible.

| Relied on | Kind |
| --- | --- |
| `kimi web --no-open --port N` prints `http://127.0.0.1:N/#token=…` | CLI contract |
| `GET /` returns 2xx | HTTP |
| `new Notification(...)`, the `online` event | Web standards |
| `?kimi_desktop=1&platform=…&kimi_vibrancy=1&kimi_onboarded=1`, optional `window.kimiDesktop` hooks (`onMenuAction`, `showWindow`, `setTheme`, `get/setVibrancy`, `setOnboarded`) | Opt-in, degrades to plain web mode |
| Frosted sidebar shim: `--color-sidebar-bg` and the page/`#app`/`.app` backgrounds go transparent | Cosmetic; falls back to an opaque sidebar if the markup changes |

The app does **not** read `~/.kimi-code` internals (server registry, tokens), call private API routes, or reach into the page's framework state. Servers are tracked with a pidfile and log the app owns, under `${XDG_STATE_HOME:-~/.local/state}/kimi-lite/` on each host.

If a Kimi update ever breaks the desktop presentation, turn off **Settings → Seamless title bar** (or just **Frosted sidebar**). The app then shows the plain web UI under a standard title bar.

## How a connection works

```text
Host window ──► 127.0.0.1:<stable port> ──► ssh -M master ──► 127.0.0.1:<server port> on host
                                              │
                                              └─ sh: ensure server (reuse pidfile, else nohup kimi web)
```

1. Open the SSH master (skipped for this computer).
2. Run the ensure script under the host's login shell: reuse the tracked server if it's alive (pid plus start time), otherwise start a new one on a free port.
3. Forward the stable local port to it (`ssh -O forward`).
4. Probe `GET /`, then show the page, or keep the current page if the origin is unchanged.

**Hide GPUs** starts a separate server variant with empty `CUDA/NVIDIA/HIP/ROCR_VISIBLE_DEVICES`, and shims `nvidia-smi`/`rocm-smi` so agents go through Slurm.

## Development

You need Rust (stable), Node.js 22+ and pnpm.

```sh
pnpm install
pnpm dev          # run in development
pnpm build        # release bundle (.app / .dmg)
cd src-tauri && cargo test
```

Layout:

```text
src-tauri/src/
  lib.rs         windows, IPC commands, navigation/download/notification policy
  supervisor.rs  per-host state machine: connect → serve → health → reconnect
  transport.rs   local shell / OpenSSH master, forwards, askpass, probe
  scripts.rs     POSIX sh run on hosts (ensure / stop server, GPU mask)
  bridge.js      injected into Kimi pages: notifications, drag strip, banner, hooks
  menu.rs        native menu bar
  config.rs      hosts.json
ui/              Hosts window and connection screen (plain HTML)
```

## Security

- Kimi listens on loopback only, and its bearer token stays on. Tunnels bind to `127.0.0.1`.
- Kimi pages can call exactly four app commands (notify, show window, retry, set theme) plus window dragging. Everything else is limited to the app's own pages (`capabilities/`).
- Navigation is restricted to loopback and app pages. Other links open in the default browser.
- SSH hosts and commands are validated and passed as arguments, never through a local shell.

## License and branding

MIT. “Kimi” and related marks belong to their respective owners. The bundled Schibsted Grotesk font is licensed under the SIL Open Font License 1.1. The app icon is taken from the official Kimi Code desktop app, for personal use only; replace it before distributing builds.
