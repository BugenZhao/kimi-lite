//! Self-updates from GitHub Releases, the way Mac apps usually do it: a quiet
//! check at launch and every few hours, "Check for Updates…" in the app menu,
//! and an alert offering to install and relaunch. The release workflow
//! publishes `latest.json` and an archive signed with the app's updater key
//! (verified here, on top of Apple's notarization).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Manager};
use tauri_plugin_updater::UpdaterExt;

use crate::AppState;

const FIRST_CHECK: Duration = Duration::from_secs(20);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// Release notes beyond this are cut short in the alert.
const NOTES_LIMIT: usize = 1200;

static CHECKING: AtomicBool = AtomicBool::new(false);
/// Set by "Check for Updates…" while a background check holds an update
/// back until the app is in use: show it now instead.
static ASK_NOW: AtomicBool = AtomicBool::new(false);

/// Starts the background checks (each skipped while turned off in Settings).
pub fn spawn_background(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        loop {
            let enabled = app.state::<AppState>().store.lock().unwrap().settings.check_for_updates;
            if enabled {
                check(&app, false).await;
            }
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

/// "Check for Updates…": always answers, even when there is nothing new.
pub fn check_now(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move { check(&app, true).await });
}

async fn check(app: &AppHandle, interactive: bool) {
    if CHECKING.swap(true, Ordering::SeqCst) {
        if interactive {
            ASK_NOW.store(true, Ordering::SeqCst);
        }
        return;
    }
    ASK_NOW.store(false, Ordering::SeqCst);
    let result = run(app, interactive).await;
    CHECKING.store(false, Ordering::SeqCst);
    if let Err(error) = result {
        eprintln!("update check failed: {error}");
        if interactive {
            let text = format!("Kimi Lite couldn't check for updates. Try again later.\n\n{error}");
            crate::chrome::alert(app, "Update Error", &text, &["OK"]).await;
        }
    }
}

async fn run(app: &AppHandle, interactive: bool) -> Result<(), String> {
    let name = app.package_info().name.clone();
    let update = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())?;
    let Some(update) = update else {
        if interactive {
            let version = app.package_info().version.to_string();
            let text = format!("{name} {version} is currently the newest version available.");
            crate::chrome::alert(app, "You're up to date!", &text, &["OK"]).await;
        }
        return Ok(());
    };
    let skipped = app.state::<AppState>().store.lock().unwrap().settings.skipped_version.clone();
    if !interactive && skipped.as_deref() == Some(update.version.as_str()) {
        return Ok(());
    }

    // Download first, so "Install and Relaunch" is instant.
    let bytes = update.download(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
    if !interactive {
        // Don't pull a background app forward; wait until it is in use.
        while crate::focused_window(app).is_none() && !ASK_NOW.swap(false, Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    let mut text = format!("{name} {} is now available—you have {}.", update.version, update.current_version);
    if let Some(notes) = update.body.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        text.push_str("\n\n");
        text.push_str(&truncate(notes, NOTES_LIMIT));
    }
    let title = format!("A new version of {name} is available!");
    match crate::chrome::alert(app, &title, &text, &["Install and Relaunch", "Later", "Skip This Version"]).await {
        Some(0) => {
            update.install(bytes).map_err(|e| e.to_string())?;
            app.restart();
        }
        Some(2) => {
            let state = app.state::<AppState>();
            let mut store = state.store.lock().unwrap();
            store.settings.skipped_version = Some(update.version.clone());
            store.save();
        }
        _ => {}
    }
    Ok(())
}

fn truncate(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((cut, _)) => format!("{}…", text[..cut].trim_end()),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::truncate;

    #[test]
    fn truncates_notes() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("one two three", 7), "one two…");
        assert_eq!(truncate("版本更新说明", 2), "版本…");
    }
}
