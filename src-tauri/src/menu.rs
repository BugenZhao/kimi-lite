//! Native menu bar. Page-level actions (New Chat, Settings) are forwarded
//! to the web UI through the optional `kimiDesktop.onMenuAction` hook.

use tauri::menu::{IconMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, LogicalPosition, Manager, WebviewWindow, Wry};

use crate::chrome;
use crate::config::HostConfig;

use crate::supervisor::Command;
use crate::AppState;

const HOST_PREFIX: &str = "open-host:";

pub fn install(app: &AppHandle) -> tauri::Result<()> {
    apply(app)?;
    app.on_menu_event(|app, event| handle(app, event.id().as_ref()));
    Ok(())
}

fn apply(app: &AppHandle) -> tauri::Result<()> {
    let (menu, window) = build(app)?;
    app.set_menu(menu)?;
    // Registering after installation lets AppKit add its window list and
    // native tab commands (Merge All Windows, Show Tab Bar…).
    #[cfg(target_os = "macos")]
    window.set_as_windows_menu_for_nsapp()?;
    #[cfg(not(target_os = "macos"))]
    let _ = window;
    Ok(())
}

pub fn refresh(app: &AppHandle) {
    let handle = app.clone();
    // Menus must be rebuilt on the main thread.
    let _ = app.run_on_main_thread(move || {
        let _ = apply(&handle);
    });
}

fn item(app: &AppHandle, id: &str, text: &str, accelerator: Option<&str>) -> tauri::Result<MenuItem<Wry>> {
    MenuItem::with_id(app, id, text, true, accelerator)
}

fn build(app: &AppHandle) -> tauri::Result<(Menu<Wry>, Submenu<Wry>)> {
    let sep = || PredefinedMenuItem::separator(app);
    let name = app.package_info().name.clone();

    let app_menu = Submenu::with_items(
        app,
        &name,
        true,
        &[
            &PredefinedMenuItem::about(app, Some(&format!("About {name}")), None)?,
            &item(app, "check-updates", "Check for Updates…", None)?,
            &sep()?,
            &item(app, "page:open-settings", "Settings…", Some("CmdOrCtrl+,"))?,
            &item(app, "hosts", "Hosts…", Some("CmdOrCtrl+Shift+H"))?,
            &sep()?,
            &PredefinedMenuItem::services(app, None)?,
            &sep()?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &sep()?,
            &PredefinedMenuItem::quit(app, None)?,
        ],
    )?;

    let file = Submenu::with_items(
        app,
        "File",
        true,
        &[
            &item(app, "page:new-chat", "New Chat", Some("CmdOrCtrl+N"))?,
            &item(app, "switch-host", "Switch Host…", Some("CmdOrCtrl+Shift+O"))?,
            &item(app, "new-host", "New SSH Host…", None)?,
            &sep()?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;

    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &sep()?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;

    let view = Submenu::with_items(
        app,
        "View",
        true,
        &[
            &item(app, "reload", "Reload Page", Some("CmdOrCtrl+R"))?,
            &sep()?,
            &item(app, "zoom-reset", "Actual Size", Some("CmdOrCtrl+0"))?,
            &item(app, "zoom-in", "Zoom In", Some("CmdOrCtrl+="))?,
            &item(app, "zoom-out", "Zoom Out", Some("CmdOrCtrl+-"))?,
            &sep()?,
            &PredefinedMenuItem::fullscreen(app, None)?,
        ],
    )?;

    let host = Submenu::with_items(
        app,
        "Host",
        true,
        &[
            &item(app, "reconnect", "Reconnect", Some("CmdOrCtrl+Alt+R"))?,
            &item(app, "restart", "Restart Kimi Code", None)?,
            &item(app, "stop", "Stop Kimi Code on Host", None)?,
            &sep()?,
            &item(app, "edit-host", "Edit Host…", None)?,
        ],
    )?;

    let window = Submenu::with_id(app, "window", "Window", true)?;
    window.append(&PredefinedMenuItem::minimize(app, None)?)?;
    window.append(&PredefinedMenuItem::maximize(app, None)?)?;
    window.append(&sep()?)?;
    let hosts = app.state::<AppState>().store.lock().unwrap().hosts.clone();
    for (index, host) in hosts.iter().enumerate() {
        window.append(&host_item(app, host, index)?)?;
    }
    window.append(&sep()?)?;
    window.append(&PredefinedMenuItem::bring_all_to_front(app, None)?)?;

    let menu = Menu::with_items(app, &[&app_menu, &file, &edit, &view, &host, &window])?;
    Ok((menu, window))
}

/// A host entry: its color dot, name, and ⌘1…⌘9 for the first nine.
fn host_item(app: &AppHandle, host: &HostConfig, index: usize) -> tauri::Result<IconMenuItem<Wry>> {
    let accelerator = (index < 9).then(|| format!("CmdOrCtrl+{}", index + 1));
    IconMenuItem::with_id(
        app,
        format!("{HOST_PREFIX}{}", host.id),
        &host.name,
        true,
        Some(chrome::dot_icon(host.color.as_deref())),
        accelerator.as_deref(),
    )
}

/// Native host switcher, shown at a point in `window` (logical pixels).
pub fn popup_hosts(app: &AppHandle, window: &WebviewWindow, x: f64, y: f64) {
    let build = || -> tauri::Result<Menu<Wry>> {
        let menu = Menu::new(app)?;
        let hosts = app.state::<AppState>().store.lock().unwrap().hosts.clone();
        for (index, host) in hosts.iter().enumerate() {
            menu.append(&host_item(app, host, index)?)?;
        }
        menu.append(&PredefinedMenuItem::separator(app)?)?;
        menu.append(&item(app, "new-host", "New SSH Host…", None)?)?;
        menu.append(&item(app, "hosts", "Hosts…", None)?)?;
        Ok(menu)
    };
    if let Ok(menu) = build() {
        let _ = window.popup_menu_at(&menu, LogicalPosition::new(x, y));
    }
}

fn handle(app: &AppHandle, id: &str) {
    if let Some(host) = id.strip_prefix(HOST_PREFIX) {
        let _ = crate::open_host_window(app, host);
        return;
    }
    if let Some(action) = id.strip_prefix("page:") {
        if let Some((_, window)) = crate::focused_host(app) {
            let _ = window.eval(format!("window.__kd && window.__kd.menu({})", serde_json::json!(action)));
        } else if action == "new-chat" {
            crate::show_hosts_window(app, None);
        }
        return;
    }
    match id {
        "hosts" => crate::show_hosts_window(app, None),
        "check-updates" => crate::updates::check_now(app),
        "new-host" => crate::show_hosts_window(app, Some("new")),
        "switch-host" => match crate::focused_window(app) {
            // Under the host chip in Kimi's sidebar header.
            Some(window) if window.label().starts_with("host-") => popup_hosts(app, &window, 96.0, 40.0),
            Some(window) => popup_hosts(app, &window, 24.0, 40.0),
            None => crate::show_hosts_window(app, None),
        },
        "reload" => {
            if let Some(window) = crate::focused_window(app) {
                let _ = window.reload();
            }
        }
        "zoom-in" | "zoom-out" | "zoom-reset" => {
            if let Some(window) = crate::focused_window(app) {
                let delta = match id {
                    "zoom-in" => Some(0.1),
                    "zoom-out" => Some(-0.1),
                    _ => None,
                };
                crate::adjust_zoom(app, &window, delta);
            }
        }
        "reconnect" | "restart" | "stop" => {
            if let Some((host, _)) = crate::focused_host(app) {
                let command = match id {
                    "reconnect" => Command::Retry,
                    "restart" => Command::Restart,
                    _ => Command::Stop,
                };
                crate::send_command(app, &host, command);
            }
        }
        "edit-host" => {
            let focus = crate::focused_host(app).map(|(host, _)| host);
            crate::show_hosts_window(app, focus.as_deref());
        }
        _ => {}
    }
}
