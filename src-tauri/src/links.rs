//! `kimi://` links. `kimi://sessions/<session-id>` opens a Kimi Code session
//! in the window of whichever host has it: session IDs are random UUIDs, so
//! links carry no host and work wherever the same hosts are set up, however
//! they are named there. The host is found in a local index of sessions seen
//! here, then by asking the hosts with `kimi session list` (Kimi's own CLI).
//! On the page, a session lives at `/sessions/<session-id>`.

use tauri::{AppHandle, Manager, Url};

use crate::supervisor::Phase;
use crate::transport::Transport;
use crate::{scripts, AppState};

const SCHEME: &str = "kimi";

#[derive(Debug, PartialEq)]
pub enum Link {
    /// `kimi://`: bring the app forward.
    App,
    Session(String),
}

pub fn parse(url: &str) -> Option<Link> {
    let url = Url::parse(url).ok()?;
    if url.scheme() != SCHEME {
        return None;
    }
    match url.host_str().unwrap_or("") {
        "" if url.path().trim_matches('/').is_empty() => Some(Link::App),
        "sessions" | "session" => session_id(url.path().trim_matches('/')).map(Link::Session),
        _ => None,
    }
}

/// Kimi's session IDs look like `session_<uuid>`; a bare UUID is accepted too.
/// Only IDs of plain characters pass, since they end up in shell and URLs.
fn session_id(raw: &str) -> Option<String> {
    let ok = !raw.is_empty() && raw.len() <= 128 && raw.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        return None;
    }
    Some(if raw.starts_with("session_") { raw.to_string() } else { format!("session_{raw}") })
}

pub fn session_link(session: &str) -> String {
    format!("{SCHEME}://sessions/{session}")
}

/// The session a Kimi page URL shows, if any.
pub fn session_of_page(url: &Url) -> Option<String> {
    let id = url.path().strip_prefix("/sessions/")?;
    session_id(&percent_decode(id)).filter(|id| !id.contains('/'))
}

fn percent_decode(text: &str) -> String {
    // Session IDs are plain characters; Kimi still percent-encodes them.
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn handle(app: &AppHandle, url: &str) {
    match parse(url) {
        Some(Link::App) | None => crate::show_hosts_window(app, None),
        Some(Link::Session(session)) => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move { open_session(&app, &session).await });
        }
    }
}

async fn open_session(app: &AppHandle, session: &str) {
    let state = app.state::<AppState>();
    let (hosts, known) = {
        let store = state.store.lock().unwrap();
        (store.hosts.clone(), store.session_host(session).map(str::to_string))
    };
    if let Some(host) = known.filter(|id| hosts.iter().any(|h| &h.id == id)) {
        return go(app, &host, session);
    }

    // Ask the connected hosts first; only then connect to the others (which
    // may ask for SSH authentication).
    let connected: Vec<_> = {
        let sessions = state.sessions.lock().unwrap();
        hosts.iter().map(|h| sessions.get(&h.id).is_some_and(|s| s.status().phase == Phase::Ready)).collect()
    };
    for want_connected in [true, false] {
        let candidates: Vec<_> = hosts.iter().zip(&connected).filter(|(_, c)| **c == want_connected).map(|(h, _)| h.clone()).collect();
        if candidates.is_empty() {
            continue;
        }
        let lookups: Vec<_> = candidates
            .iter()
            .map(|host| {
                let script = scripts::has_session(&host.command, session);
                let transport = Transport::for_host(host);
                tauri::async_runtime::spawn(async move { transport.run_script(&script, |_| {}).await })
            })
            .collect();
        let mut found = Vec::new();
        for (host, lookup) in candidates.iter().zip(lookups) {
            if let Ok(Ok(result)) = lookup.await {
                if result.lines.iter().any(|l| l.trim() == format!("{} yes", scripts::FOUND)) {
                    found.push(host.id.clone());
                }
            }
        }
        // Several hosts can share a home directory (and so the session):
        // prefer one whose window is already open.
        let open = found.iter().find(|id| app.get_webview_window(&crate::host_label(id)).is_some());
        if let Some(host) = open.or(found.first()).cloned() {
            state.store.lock().unwrap().remember_session(session, &host);
            return go(app, &host, session);
        }
    }
    let text = format!("None of your hosts has the session {session}. It may have been deleted, or live on a host that isn't set up here.");
    crate::chrome::alert(app, "Session not found", &text, &["OK"]).await;
}

/// Shows `session` in the host's window: right away when Kimi is on screen,
/// otherwise once the window connects.
fn go(app: &AppHandle, host: &str, session: &str) {
    let path = format!("/sessions/{session}");
    let state = app.state::<AppState>();
    let ready = state.sessions.lock().unwrap().get(host).is_some_and(|s| s.status().phase == Phase::Ready);
    match app.get_webview_window(&crate::host_label(host)) {
        Some(window) if ready => {
            let _ = window.eval(format!("location.assign({} + location.search)", serde_json::json!(path)));
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
        _ => {
            state.pending_paths.lock().unwrap().insert(host.to_string(), path);
            let _ = crate::open_host_window(app, host);
        }
    }
}

/// Edit > Copy Session Link: the focused window's session as a `kimi://` link.
pub fn copy_session_link(app: &AppHandle) {
    let page = crate::focused_host(app).and_then(|(host, window)| Some((host, window.url().ok()?)));
    match page.and_then(|(host, url)| Some((host, session_of_page(&url)?))) {
        Some((host, session)) => {
            app.state::<AppState>().store.lock().unwrap().remember_session(&session, &host);
            crate::chrome::copy_text(&session_link(&session));
        }
        None => crate::chrome::beep(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_links() {
        let id = "session_2e802548-0f7b-4fc1-aafc-9cdcf9a8d768";
        assert_eq!(parse(&format!("kimi://sessions/{id}")), Some(Link::Session(id.into())));
        assert_eq!(parse("kimi://sessions/2e802548-0f7b-4fc1-aafc-9cdcf9a8d768"), Some(Link::Session(id.into())));
        assert_eq!(parse("kimi://"), Some(Link::App));
        assert_eq!(parse("kimi://sessions/a'b"), None);
        assert_eq!(parse("kimi://sessions/"), None);
        assert_eq!(parse("kimi-code://sessions/x"), None);
        assert_eq!(parse("kimi://elsewhere/x"), None);
    }

    #[test]
    fn reads_sessions_from_pages() {
        let page = |p: &str| Url::parse(&format!("http://127.0.0.1:1{p}")).unwrap();
        assert_eq!(session_of_page(&page("/sessions/session_abc?kimi_desktop=1")), Some("session_abc".into()));
        assert_eq!(session_of_page(&page("/sessions/session%5Fabc")), Some("session_abc".into()));
        assert_eq!(session_of_page(&page("/")), None);
        assert_eq!(session_of_page(&page("/admin/sessions")), None);
        assert_eq!(session_link("session_abc"), "kimi://sessions/session_abc");
    }
}
