//! Native window chrome tweaks: traffic-light placement (macOS) and the
//! colored dots that identify hosts in menus.

use tauri::image::Image;
use tauri::{AppHandle, WebviewWindow};

/// Left inset of the close button, and the vertical center the buttons line
/// up with: the middle of Kimi's 48px header row.
const TRAFFIC_LIGHT_X: f64 = 16.0;
const TRAFFIC_LIGHT_CENTER_Y: f64 = 24.0;

/// Places the traffic lights in line with Kimi's header. AppKit puts them
/// back on title changes, resizes and focus changes, and the WebView covering
/// the window keeps tao's own re-apply (in `drawRect:`) from running, so
/// callers invoke this after each of those. AppKit re-lays out the title bar
/// lazily, so the placement is repeated after its next layout passes too.
pub fn align_traffic_lights(window: &WebviewWindow) {
    #[cfg(target_os = "macos")]
    {
        place_on_main(window);
        let target = window.clone();
        std::thread::spawn(move || {
            for delay in [30, 120, 400] {
                std::thread::sleep(std::time::Duration::from_millis(delay));
                place_on_main(&target);
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = window;
}

#[cfg(target_os = "macos")]
fn place_on_main(window: &WebviewWindow) {
    let target = window.clone();
    let _ = window.run_on_main_thread(move || {
        if let Ok(ptr) = target.ns_window() {
            // SAFETY: `ns_window` is the live NSWindow of this window, used on
            // the main thread.
            unsafe { place_traffic_lights(&*(ptr as *const objc2_app_kit::NSWindow)) };
        }
    });
}

#[cfg(target_os = "macos")]
unsafe fn place_traffic_lights(window: &objc2_app_kit::NSWindow) {
    use objc2_app_kit::NSWindowButton;

    let buttons = [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ]
    .map(|kind| window.standardWindowButton(kind));
    let [Some(close), Some(miniaturize), Some(_)] = &buttons else { return };
    let Some(container) = close.superview().and_then(|v| v.superview()) else { return };

    // Make the title bar container twice the target center tall, so the
    // buttons can be centered in it, but only as wide as the buttons: it sits
    // above the page and would otherwise swallow clicks on Kimi's header.
    let close_frame = close.frame();
    let spacing = miniaturize.frame().origin.x - close_frame.origin.x;
    let height = TRAFFIC_LIGHT_CENTER_Y * 2.0;
    let mut container_frame = container.frame();
    container_frame.size.height = height;
    container_frame.size.width = TRAFFIC_LIGHT_X + 2.0 * spacing + close_frame.size.width + 4.0;
    container_frame.origin.y = window.frame().size.height - height;
    container.setFrame(container_frame);

    let Some(parent) = close.superview() else { return };
    let parent_height = parent.frame().size.height;
    for (i, button) in buttons.iter().flatten().enumerate() {
        let mut origin = button.frame().origin;
        origin.x = TRAFFIC_LIGHT_X + i as f64 * spacing;
        origin.y = (parent_height - close_frame.size.height) / 2.0;
        button.setFrameOrigin(origin);
    }
}

/// A native app-modal alert; answers the index of the button pressed.
pub async fn alert(app: &AppHandle, title: &str, text: &str, buttons: &[&str]) -> Option<usize> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (title, text) = (title.to_string(), text.to_string());
    let buttons: Vec<String> = buttons.iter().map(|b| b.to_string()).collect();
    app.run_on_main_thread(move || {
        let _ = tx.send(show_alert(&title, &text, &buttons));
    })
    .ok()?;
    rx.await.ok().flatten()
}

#[cfg(target_os = "macos")]
fn show_alert(title: &str, text: &str, buttons: &[String]) -> Option<usize> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication};
    use objc2_foundation::NSString;

    let mtm = MainThreadMarker::new()?;
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(text));
    for button in buttons {
        alert.addButtonWithTitle(&NSString::from_str(button));
    }
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    usize::try_from(alert.runModal() - NSAlertFirstButtonReturn).ok()
}

#[cfg(not(target_os = "macos"))]
fn show_alert(_title: &str, _text: &str, _buttons: &[String]) -> Option<usize> {
    None
}

/// Puts plain text on the general pasteboard.
pub fn copy_text(text: &str) {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
        use objc2_foundation::NSString;
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        // SAFETY: a system constant.
        pasteboard.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = text;
}

/// The system alert sound, for a command that has nothing to act on.
pub fn beep() {
    #[cfg(target_os = "macos")]
    objc2_app_kit::NSBeep();
}

/// Palette for host identity, in Kimi's register (its accent blue first).
pub const PALETTE: [&str; 8] = [
    "#1783ff", "#8b5cf6", "#16a34a", "#f59e0b", "#f43f5e", "#0d9488", "#f97316", "#6366f1",
];

/// A small anti-aliased colored dot for menu items.
pub fn dot_icon(color: Option<&str>) -> Image<'static> {
    const SIZE: u32 = 32;
    let (r, g, b) = color.and_then(parse_hex).unwrap_or((0x9c, 0xa3, 0xaf));
    let radius = SIZE as f64 * 0.28;
    let center = SIZE as f64 / 2.0;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f64 + 0.5 - center;
            let dy = y as f64 + 0.5 - center;
            let alpha = (radius + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
            rgba.extend_from_slice(&[r, g, b, (alpha * 255.0) as u8]);
        }
    }
    Image::new_owned(rgba, SIZE, SIZE)
}

pub fn parse_hex(color: &str) -> Option<(u8, u8, u8)> {
    let hex = color.strip_prefix('#')?;
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some((channel(0)?, channel(2)?, channel(4)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_colors() {
        assert_eq!(parse_hex("#1783ff"), Some((0x17, 0x83, 0xff)));
        assert_eq!(parse_hex("1783ff"), None);
        assert_eq!(parse_hex("#12345g"), None);
        assert!(PALETTE.iter().all(|c| parse_hex(c).is_some()));
    }
}
