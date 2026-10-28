# Kimi Lite

A lightweight native desktop app for [Kimi Code](https://github.com/MoonshotAI/kimi-code)'s web UI, on your own computer or on any machine you can reach over SSH.

Kimi Lite starts `kimi web` where you want it, keeps a private tunnel to it, and shows its UI in a native window that looks like it belongs there. It is about 6 MB (it uses the system WebView) and is built with [Tauri](https://tauri.app).

## Features

- **Seamless windows.** Each host gets its own window, and Kimi's UI fills it completely, with its header in the title bar and a frosted-glass sidebar on macOS. No extra chrome.
- **Local and SSH hosts.** It uses your own OpenSSH, so `~/.ssh/config`, `ProxyJump`, keys and agents all work. It needs one login per connection, and shows native prompts for passwords, passphrases and host keys on macOS.
- **Survives disconnects.** Kimi keeps running on the host when the connection drops, your laptop sleeps or you close the window, so agents keep working. Kimi Lite reconnects by itself and the page picks up where it was, without a reload.
- **Many hosts at once.** Each host has a color: it tints the sidebar, Kimi's accent (buttons, toggles, links) and selected text, draws a thin line along the top of the window, and marks the host in the switcher. The host chip sits in the sidebar header, or at the right of the header while the sidebar is collapsed. Click the host chip in the sidebar header, or press ⇧⌘O, to switch hosts, or press ⌘1…⌘9.
- **Native details.** A real menu bar, copy and paste, zoom, file drag and drop, system notifications with a Dock badge and bounce, downloads to `~/Downloads`, links opening in your browser, the arrow cursor, and windows restored on relaunch.
- **Custom model providers that just work.** "Fetch model list" in Kimi's provider settings runs on the host instead of in the window, so it isn't blocked by CORS, and endpoints only the host can reach (a vLLM on its localhost, a cluster-internal name) list their models too.
- **GPU-aware.** An optional per-host setting hides local GPUs from the agent, so GPU work goes through Slurm (`srun`/`sbatch --gres=gpu:N`).

## Requirements

- macOS 12 or later. The code builds for Linux and Windows too, but they are untested so far.
- [Kimi Code](https://github.com/MoonshotAI/kimi-code) installed and signed in on every host you use, including your own computer if you use it locally. Kimi Lite finds `kimi` on `PATH` or in `~/.kimi-code/bin`.
- For SSH hosts: OpenSSH, with key or agent authentication (password prompts work too), and a POSIX `sh` on the host.

## Install

Download the `.dmg` (Apple Silicon) from [Releases](https://github.com/BugenZhao/kimi-lite/releases) and drag Kimi Lite to Applications. Releases are signed with a Developer ID and notarized by Apple. Kimi Lite then keeps itself up to date: it checks GitHub Releases in the background and offers to install new versions, or use **Kimi Lite → Check for Updates…**. Automatic checks can be turned off in Settings.

### Build from source

You need Rust (stable), Node.js 22+ and pnpm:

```sh
git clone https://github.com/BugenZhao/kimi-lite.git
cd kimi-lite
pnpm install
pnpm build
```

The app is written to `src-tauri/target/release/bundle/macos/Kimi Lite.app`.

## Usage

The **Hosts** window (⇧⌘H) lists your hosts. **This Mac** is always there. **Add SSH Host** takes an alias from `~/.ssh/config` (suggestions are offered) or `user@host`. Opening a host starts Kimi Code there if it isn't running, or reuses it if it is.

| Shortcut | Action |
| --- | --- |
| ⇧⌘O | Switch host |
| ⌘1 … ⌘9 | Go to a host |
| ⇧⌘H | Hosts window |
| ⌥⌘R | Reconnect |
| ⌘R | Reload page |
| ⌘= / ⌘- / ⌘0 | Zoom |

Kimi Code keeps running on a host after its window closes. To shut it down, use **Host → Stop Kimi Code on Host**, or **Stop Kimi Code** in the Hosts window.

**Settings:**
- **Seamless title bar** turns off Kimi's desktop layout and shows the plain web UI under a standard title bar.
- **Frosted sidebar** turns off the glass effect.

## Troubleshooting

- **Stuck on "Waiting for SSH authentication…".** Your SSH agent is probably waiting for approval (for example, 1Password asks the first time an app uses a key). Approve it, or test with `ssh <host>` in a terminal.
- **Kimi Code fails to start on a host.** Open **Show details** on the connection screen. The server's own log is at `~/.local/state/kimi-lite/server.log` on the host.
- **Something looks off after a Kimi Code update.** Turn off **Seamless title bar** in Settings; Kimi Lite then shows the plain web UI.

## How it works

Kimi Lite runs `kimi web --no-open` on the host, detached so it outlives the connection, and tracks it with its own pidfile. It forwards a local port that stays the same for each host, so the page keeps its storage across reconnects, through one SSH connection it owns. It watches that connection and a plain `GET /` health check, and reconnects with backoff when either fails.

It relies only on Kimi Code's public CLI and web standards, plus the optional desktop hooks Kimi's web UI exposes. It never reads Kimi's internal files or private APIs.

## Security

- Kimi Code listens on loopback only, with its token authentication on. Tunnels bind to `127.0.0.1`.
- Kimi's page can call only a handful of narrow app functions (notify, show window, retry, theme, glass, onboarding, host switcher, listing a model provider's models). Everything else is limited to the app's own pages. The model list is fetched with an HTTP(S) GET from the host, so endpoints that only the host can reach work too.
- The window only navigates to loopback and app pages. Other links open in your browser.

## License

MIT, see [LICENSE](LICENSE).

Kimi Lite is an independent project, not affiliated with or endorsed by Moonshot AI. “Kimi” and “Kimi Code” are trademarks of their respective owners. The bundled Schibsted Grotesk font is licensed under the [SIL Open Font License 1.1](ui/fonts/OFL.txt).
