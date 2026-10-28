mod chrome;
mod config;
mod fetch;
mod menu;
mod scripts;
mod ssh_config;
mod supervisor;
mod transport;

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{
    AppHandle, Manager, RunEvent, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_notification::NotificationExt;

use config::{HostConfig, Store};
use supervisor::{Command, HostStatus, Supervisor};

const BRIDGE_JS: &str = include_str!("bridge.js");
const HOSTS_LABEL: &str = "hosts";

pub struct AppState {
    pub store: Mutex<Store>,
    sessions: Mutex<HashMap<String, Supervisor>>,
    zoom: Mutex<HashMap<String, f64>>,
    unread: Mutex<i64>,
}

pub fn host_label(id: &str) -> String {
    format!("host-{id}")
}

fn host_id(label: &str) -> Option<&str> {
    label.strip_prefix("host-")
}

pub fn connect_page_url(id: &str) -> Option<Url> {
    // The app's own pages are served from the `tauri` custom protocol.
    let base = if cfg!(windows) { "http://tauri.localhost/" } else { "tauri://localhost/" };
    Url::parse(&format!("{base}connect.html?host={id}")).ok()
}

// ---------------------------------------------------------------- windows

fn open_host_window(app: &AppHandle, id: &str) -> Result<(), String> {
    let label = host_label(id);
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }
    let state = app.state::<AppState>();
    let host = state.store.lock().unwrap().get(id).cloned().ok_or("Unknown host.")?;

    let settings = state.store.lock().unwrap().settings.clone();
    let integrated = settings.desktop_integration;
    let seamless = integrated && cfg!(target_os = "macos");
    let bridge_config = serde_json::json!({
        "hooks": integrated,
        "vibrancy": seamless && settings.vibrancy,
        "host": host_identity(&host),
        "platform": std::env::consts::OS.replace("macos", "darwin").replace("windows", "win32"),
        // Height of the strip that drags the window (Kimi's header height).
        "dragStrip": if seamless { 48 } else { 0 },
    });
    let builder = WebviewWindowBuilder::new(app, &label, WebviewUrl::App(format!("connect.html?host={id}").into()))
        .title(&host.name)
        .inner_size(1280.0, 820.0)
        .min_inner_size(640.0, 440.0)
        .initialization_script(format!("window.__KD_CONFIG__ = {bridge_config};\n{BRIDGE_JS}"))
        // Let file drops reach the page, which handles attachments itself.
        .disable_drag_drop_handler()
        .on_navigation(|url| {
            if is_app_url(url) {
                return true;
            }
            open_external(url);
            false
        })
        .on_new_window(|url, _features| {
            open_external(&url);
            tauri::webview::NewWindowResponse::Deny
        })
        .on_page_load({
            // The init script carries the settings from when the window was
            // made; reloads and reconnects get the current ones.
            let app = app.clone();
            let id = id.to_string();
            move |webview, payload| {
                if payload.event() != tauri::webview::PageLoadEvent::Finished {
                    return;
                }
                let state = app.state::<AppState>();
                let store = state.store.lock().unwrap();
                let Some(host) = store.get(&id) else { return };
                let vibrancy = seamless && store.settings.vibrancy;
                let _ = webview.eval(format!(
                    "window.__kd && (window.__kd.setHost({}), window.__kd.setVibrancy({vibrancy}))",
                    host_identity(host)
                ));
            }
        })
        .on_download({
            let app = app.clone();
            move |_webview, event| {
                match event {
                    tauri::webview::DownloadEvent::Requested { url, destination } => {
                        if let Some(dir) = app.path().download_dir().ok() {
                            *destination = unique_path(&dir, destination, &url);
                        }
                    }
                    tauri::webview::DownloadEvent::Finished { success, .. } => {
                        let title = if success { "Download finished" } else { "Download failed" };
                        let _ = app.notification().builder().title(title).body("Saved to Downloads").show();
                    }
                    _ => {}
                }
                true
            }
        });
    #[cfg(target_os = "macos")]
    let builder = if seamless {
        // The material is always there; the page decides whether its
        // sidebar lets it show through (so the setting applies live).
        builder
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true)
            .transparent(true)
            .effects(sidebar_material())
    } else {
        builder
    }
    .tabbing_identifier("kimi-host");

    let window = builder.build().map_err(|e| e.to_string())?;
    if seamless {
        chrome::align_traffic_lights(&window);
    }
    let supervisor = Supervisor::spawn(app.clone(), &host);
    let generation = supervisor.generation;
    let app_handle = app.clone();
    let host_id = id.to_string();
    let aligned = window.clone();
    window.on_window_event(move |event| match event {
        // AppKit puts the traffic lights back on these.
        WindowEvent::Resized(_) | WindowEvent::ThemeChanged(_) | WindowEvent::Focused(false) if seamless => {
            chrome::align_traffic_lights(&aligned);
        }
        WindowEvent::Focused(true) => {
            clear_unread(&app_handle);
            if seamless {
                chrome::align_traffic_lights(&aligned);
            }
        }
        WindowEvent::CloseRequested { .. } => {
            app_handle.state::<AppState>().store.lock().unwrap().set_open(&host_id, false);
        }
        WindowEvent::Destroyed => {
            // A replacement window may already own this host.
            let state = app_handle.state::<AppState>();
            let mut sessions = state.sessions.lock().unwrap();
            if sessions.get(&host_id).is_some_and(|s| s.generation == generation) {
                sessions.remove(&host_id);
            }
            drop(sessions);
            menu::refresh(&app_handle);
        }
        _ => {}
    });

    state.store.lock().unwrap().set_open(id, true);
    state.sessions.lock().unwrap().insert(id.to_string(), supervisor);
    menu::refresh(app);
    Ok(())
}

pub fn show_hosts_window(app: &AppHandle, focus: Option<&str>) {
    if let Some(window) = app.get_webview_window(HOSTS_LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        if let Some(id) = focus {
            let _ = window.eval(format!("window.editHost && window.editHost({})", serde_json::json!(id)));
        }
        return;
    }
    let url = match focus {
        Some(id) => format!("index.html?edit={id}"),
        None => "index.html".into(),
    };
    let builder = WebviewWindowBuilder::new(app, HOSTS_LABEL, WebviewUrl::App(url.into()))
        .title("Hosts")
        .inner_size(860.0, 600.0)
        .min_inner_size(640.0, 420.0)
        .resizable(true)
        .on_navigation(|url| is_app_url(url));
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true)
        .transparent(true)
        .effects(sidebar_material());
    if let Ok(window) = builder.build() {
        chrome::align_traffic_lights(&window);
        let aligned = window.clone();
        window.on_window_event(move |event| {
            if matches!(event, WindowEvent::Resized(_) | WindowEvent::Focused(_) | WindowEvent::ThemeChanged(_)) {
                chrome::align_traffic_lights(&aligned);
            }
        });
    }
}

/// What the page bridge needs to show which host a window belongs to.
pub fn host_identity(host: &HostConfig) -> serde_json::Value {
    serde_json::json!({ "id": host.id, "name": host.name, "color": host.color })
}

#[cfg(target_os = "macos")]
fn sidebar_material() -> tauri::utils::config::WindowEffectsConfig {
    tauri::utils::config::WindowEffectsConfig {
        effects: vec![tauri::window::Effect::Sidebar],
        state: Some(tauri::window::EffectState::FollowsWindowActiveState),
        ..Default::default()
    }
}

fn is_app_url(url: &Url) -> bool {
    match url.scheme() {
        "tauri" | "about" | "blob" | "data" => true,
        "http" | "https" => matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "tauri.localhost")),
        _ => false,
    }
}

fn open_external(url: &Url) {
    if !matches!(url.scheme(), "http" | "https" | "mailto") {
        return;
    }
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url.as_str()).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url.as_str()).spawn();
    #[cfg(windows)]
    let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url.as_str()]).spawn();
}

fn unique_path(dir: &std::path::Path, suggested: &std::path::Path, url: &Url) -> std::path::PathBuf {
    let name = suggested
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .or_else(|| url.path_segments().and_then(|mut s| s.next_back()).map(str::to_string))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "download".into());
    let candidate = dir.join(&name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), format!(".{ext}")),
        _ => (name.clone(), String::new()),
    };
    (2..)
        .map(|n| dir.join(format!("{stem} {n}{ext}")))
        .find(|p| !p.exists())
        .unwrap()
}

/// Shows connection state on the page. The window title stays the host name:
/// retitling makes AppKit re-lay out the title bar on every status change.
pub fn on_status_changed(app: &AppHandle, status: &HostStatus) {
    let Some(window) = app.get_webview_window(&host_label(&status.id)) else { return };
    // On a Kimi page, the bridge shows a small unobtrusive banner instead.
    if status.ever_ready || status.phase == supervisor::Phase::Ready {
        let payload = serde_json::to_string(status).unwrap_or_default();
        let _ = window.eval(format!("window.__kd && window.__kd.status({payload})"));
    }
}

fn clear_unread(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mut unread = state.unread.lock().unwrap();
    if *unread != 0 {
        *unread = 0;
        if let Some(window) = app.webview_windows().values().next() {
            let _ = window.set_badge_count(None);
        }
    }
}

pub fn focused_window(app: &AppHandle) -> Option<WebviewWindow> {
    app.webview_windows().into_values().find(|w| w.is_focused().unwrap_or(false))
}

pub fn focused_host(app: &AppHandle) -> Option<(String, WebviewWindow)> {
    let window = focused_window(app)?;
    let id = host_id(window.label())?.to_string();
    Some((id, window))
}

pub fn send_command(app: &AppHandle, id: &str, command: Command) {
    if let Some(session) = app.state::<AppState>().sessions.lock().unwrap().get(id) {
        session.send(command);
    }
}

pub fn adjust_zoom(app: &AppHandle, window: &WebviewWindow, delta: Option<f64>) {
    let state = app.state::<AppState>();
    let mut zoom = state.zoom.lock().unwrap();
    let level = zoom.entry(window.label().to_string()).or_insert(1.0);
    *level = match delta {
        Some(d) => (*level + d).clamp(0.5, 2.0),
        None => 1.0,
    };
    let _ = window.set_zoom(*level);
}

// ------------------------------------------------- commands: app pages

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HostView {
    #[serde(flatten)]
    config: HostConfig,
    status: Option<HostStatus>,
}

#[tauri::command]
fn list_hosts(state: tauri::State<AppState>) -> Vec<HostView> {
    let sessions = state.sessions.lock().unwrap();
    state
        .store
        .lock()
        .unwrap()
        .hosts
        .iter()
        .map(|h| HostView { config: h.clone(), status: sessions.get(&h.id).map(|s| s.status()) })
        .collect()
}

#[tauri::command]
fn save_host(app: AppHandle, state: tauri::State<AppState>, host: HostConfig) -> Result<HostConfig, String> {
    let saved = state.store.lock().unwrap().upsert(host)?;
    if let Some(window) = app.get_webview_window(&host_label(&saved.id)) {
        let _ = window.set_title(&saved.name);
        chrome::align_traffic_lights(&window);
        let _ = window.eval(format!("window.__kd && window.__kd.setHost({})", host_identity(&saved)));
    }
    menu::refresh(&app);
    Ok(saved)
}

#[tauri::command]
fn delete_host(app: AppHandle, state: tauri::State<AppState>, id: String) {
    if let Some(window) = app.get_webview_window(&host_label(&id)) {
        let _ = window.destroy();
    }
    state.store.lock().unwrap().remove(&id);
    menu::refresh(&app);
}

#[tauri::command]
fn reorder_hosts(app: AppHandle, state: tauri::State<AppState>, ids: Vec<String>) {
    state.store.lock().unwrap().reorder(&ids);
    menu::refresh(&app);
}

#[tauri::command]
fn open_host(app: AppHandle, id: String) -> Result<(), String> {
    open_host_window(&app, &id)
}

#[tauri::command]
fn show_hosts(app: AppHandle, focus: Option<String>) {
    show_hosts_window(&app, focus.as_deref());
}

#[tauri::command]
fn ssh_config_hosts() -> Vec<String> {
    ssh_config::list_hosts()
}

#[tauri::command]
fn host_status(state: tauri::State<AppState>, id: String) -> Option<HostStatus> {
    state.sessions.lock().unwrap().get(&id).map(|s| s.status())
}

#[tauri::command]
fn host_action(app: AppHandle, id: String, action: String) -> Result<(), String> {
    let command = match action.as_str() {
        "retry" | "show" => Command::Retry,
        "restart" => Command::Restart,
        "stop" => Command::Stop,
        _ => return Err("Unknown action.".into()),
    };
    if !app.state::<AppState>().sessions.lock().unwrap().contains_key(&id) {
        return open_host_window(&app, &id);
    }
    send_command(&app, &id, command);
    Ok(())
}

#[tauri::command]
fn get_settings(state: tauri::State<AppState>) -> config::Settings {
    state.store.lock().unwrap().settings.clone()
}

#[tauri::command]
fn set_settings(app: AppHandle, state: tauri::State<AppState>, settings: config::Settings) {
    let previous = state.store.lock().unwrap().settings.clone();
    if settings.vibrancy != previous.vibrancy {
        set_vibrancy(&app, &state, settings.vibrancy);
    }
    if settings.desktop_integration == previous.desktop_integration {
        return;
    }
    {
        let mut store = state.store.lock().unwrap();
        store.settings.desktop_integration = settings.desktop_integration;
        store.save();
    }
    // Window chrome is fixed at creation: reopen host windows to apply.
    let open: Vec<String> = state.sessions.lock().unwrap().keys().cloned().collect();
    for id in &open {
        if let Some(window) = app.get_webview_window(&host_label(id)) {
            let _ = window.destroy();
        }
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        for id in open {
            let _ = open_host_window(&app, &id);
        }
    });
}

// ------------------------------- commands: Kimi pages (remote origin)
// Kept deliberately tiny: these are reachable from the served web UI.

#[tauri::command]
fn kd_set_theme(window: WebviewWindow, theme: String) {
    let theme = match theme.as_str() {
        "light" => Some(tauri::Theme::Light),
        "dark" => Some(tauri::Theme::Dark),
        _ => None,
    };
    let _ = window.set_theme(theme);
}

#[tauri::command]
fn kd_set_vibrancy(app: AppHandle, state: tauri::State<AppState>, enabled: bool) {
    set_vibrancy(&app, &state, enabled);
}

#[tauri::command]
fn kd_set_onboarded(state: tauri::State<AppState>) {
    let mut store = state.store.lock().unwrap();
    if !store.settings.onboarded {
        store.settings.onboarded = true;
        store.save();
    }
}

fn set_vibrancy(app: &AppHandle, state: &AppState, enabled: bool) {
    {
        let mut store = state.store.lock().unwrap();
        store.settings.vibrancy = enabled;
        store.save();
    }
    for (label, window) in app.webview_windows() {
        if host_id(&label).is_some() {
            let _ = window.eval(format!("window.__kd && window.__kd.setVibrancy({enabled})"));
        }
    }
}

#[tauri::command]
fn kd_show_window(window: WebviewWindow) {
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

#[tauri::command]
fn kd_notify(app: AppHandle, window: WebviewWindow, title: String, body: Option<String>) {
    let host = window.title().unwrap_or_default();
    let mut builder = app.notification().builder().title(truncate(&title, 200));
    let body = body.map(|b| truncate(&b, 500)).unwrap_or_default();
    builder = builder.body(if host.is_empty() { body } else if body.is_empty() { host } else { format!("{host} · {body}") });
    let _ = builder.show();
    if focused_window(&app).is_none() {
        let state = app.state::<AppState>();
        let mut unread = state.unread.lock().unwrap();
        *unread += 1;
        let _ = window.set_badge_count(Some(*unread));
        // One bounce, like Mail: noticeable without nagging.
        let _ = window.request_user_attention(Some(tauri::UserAttentionType::Informational));
    }
}

/// Lists a custom provider's models for Kimi's settings page, from the host.
#[tauri::command]
async fn kd_fetch_provider_models(app: AppHandle, window: WebviewWindow, request: fetch::Request) -> serde_json::Value {
    let host = host_id(window.label()).and_then(|id| app.state::<AppState>().store.lock().unwrap().get(id).cloned());
    match host {
        Some(host) => fetch::run(&transport::Transport::for_host(&host), request).await,
        None => serde_json::json!({ "ok": false, "kind": "network" }),
    }
}

/// The host switcher, popped up from the host chip in Kimi's sidebar.
#[tauri::command]
fn kd_host_menu(app: AppHandle, window: WebviewWindow, x: f64, y: f64) {
    menu::popup_hosts(&app, &window, x, y);
}

#[tauri::command]
fn kd_retry(app: AppHandle, window: WebviewWindow) {
    if let Some(id) = host_id(window.label()) {
        send_command(&app, id, Command::Retry);
    }
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

// -------------------------------------------------------------------- app

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            transport::install_askpass(&app.path().app_data_dir()?);
            transport::collect_orphans();
            app.manage(AppState {
                store: Mutex::new(Store::load(&config_dir)),
                sessions: Mutex::new(HashMap::new()),
                zoom: Mutex::new(HashMap::new()),
                unread: Mutex::new(0),
            });
            let handle = app.handle().clone();
            menu::install(&handle)?;

            // Restore the host windows that were open at quit.
            let reopen = app.state::<AppState>().store.lock().unwrap().open_hosts.clone();
            let mut opened = false;
            for id in reopen {
                opened |= open_host_window(&handle, &id).is_ok();
            }
            if !opened {
                show_hosts_window(&handle, None);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_hosts,
            save_host,
            delete_host,
            reorder_hosts,
            open_host,
            show_hosts,
            ssh_config_hosts,
            host_status,
            host_action,
            get_settings,
            set_settings,
            kd_set_theme,
            kd_set_vibrancy,
            kd_set_onboarded,
            kd_show_window,
            kd_notify,
            kd_retry,
            kd_host_menu,
            kd_fetch_provider_models,
        ])
        .build(tauri::generate_context!())
        .expect("error while building the app");

    app.run(|app, event| match event {
        // Clicking the Dock icon with no windows brings back the host list.
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { has_visible_windows: false, .. } => show_hosts_window(app, None),
        // Like any Mac app, stay alive with no windows open.
        #[cfg(target_os = "macos")]
        RunEvent::ExitRequested { code: None, api, .. } => api.prevent_exit(),
        RunEvent::ExitRequested { .. } => {
            // Servers keep running on their hosts; only tunnels go away.
            app.state::<AppState>().sessions.lock().unwrap().clear();
        }
        _ => {}
    });
}
