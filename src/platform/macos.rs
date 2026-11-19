//! macOS global mouse state via NSEvent.
//!
//! NSEvent mouseLocation is in points with the origin at the bottom-left of
//! the primary screen. We convert to top-left-origin physical pixels so the
//! rest of the app can compare it with winit window positions.

use objc2_app_kit::{NSEvent, NSView, NSWindowCollectionBehavior};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

/// Keep the pet visible in every Space, including other applications'
/// full-screen Spaces, so switching Spaces never strands the pet elsewhere.
/// Returns false if winit does not expose the backing NSView/NSWindow.
pub fn float_on_all_spaces(window: &Window) -> bool {
    let Ok(handle) = window.window_handle() else {
        return false;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return false;
    };
    // SAFETY: `resumed` runs on the main thread and winit keeps the NSView
    // alive for the window's lifetime; the handle points at that view, whose
    // `window` is the pet's NSWindow.
    let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    let Some(ns_window) = view.window() else {
        return false;
    };
    ns_window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary,
    );
    true
}

/// Global mouse position in physical pixels, top-left origin, using the
/// primary screen's scale and height in points.
pub fn mouse_position(screen_height: f64, scale: f64) -> Option<(f64, f64)> {
    let point = NSEvent::mouseLocation();
    Some(points_to_pixels(point.x, point.y, screen_height, scale))
}

/// Left mouse button state for drag release detection.
pub fn left_button_down() -> bool {
    NSEvent::pressedMouseButtons() & 1 != 0
}

/// Post a desktop notification through the app's own bundle, so the banner
/// carries the app icon and name instead of the Script Editor icon that
/// osascript uses. Returns false when the process has no bundle identity
/// (a bare `cargo run` binary); the caller then falls back to osascript.
///
/// `NSUserNotificationCenter` is deprecated but still functional and is the
/// only notification API reachable without adding a new crate.
pub fn send_notification(message: &str) -> bool {
    notification::send(message)
}

/// Show the About dialog for the tray menu. Must run on the main thread;
/// the modal alert blocks the event loop until dismissed.
pub fn show_about() {
    about::show();
}

mod about {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication, NSImage, NSWorkspace};
    use objc2_foundation::{NSData, NSString, NSURL};

    const REPOSITORY: &str = "github.com/0xhaic/herdr-pet";
    /// Same artwork as `packaging/AppIcon.icns`; embedded so the logo also
    /// shows when running the bare binary.
    const ICON_BYTES: &[u8] = include_bytes!("../../assets/app-icon.png");

    pub fn show() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        // The app runs as an Accessory without a Dock icon, so it may not be
        // frontmost when the menu item is clicked.
        NSApplication::sharedApplication(mtm).activate();
        let body = format!(
            "由 herdr-core 中 agent 状态驱动的 macOS 桌面宠物。\n\n版本 {}\n{REPOSITORY}",
            env!("CARGO_PKG_VERSION")
        );
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str("herdr-pet"));
        alert.setInformativeText(&NSString::from_str(&body));
        if let Some(icon) = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(ICON_BYTES))
        {
            // SAFETY: the alert retains the image; `Some` is the supported
            // custom-icon case.
            unsafe { alert.setIcon(Some(&icon)) };
        }
        alert.addButtonWithTitle(&NSString::from_str("打开仓库"));
        alert.addButtonWithTitle(&NSString::from_str("关闭"));
        if alert.runModal() == NSAlertFirstButtonReturn
            && let Some(url) =
                NSURL::URLWithString(&NSString::from_str(&format!("https://{REPOSITORY}")))
        {
            NSWorkspace::sharedWorkspace().openURL(&url);
        }
    }
}

mod notification {
    #![allow(deprecated)]

    use objc2_foundation::{NSBundle, NSString, NSUserNotification, NSUserNotificationCenter};

    pub fn send(message: &str) -> bool {
        if NSBundle::mainBundle().bundleIdentifier().is_none() {
            return false;
        }
        // Called from the winit main thread during a frame tick.
        let notification = NSUserNotification::new();
        notification.setTitle(Some(&NSString::from_str("herdr-pet")));
        notification.setInformativeText(Some(&NSString::from_str(message)));
        NSUserNotificationCenter::defaultUserNotificationCenter()
            .deliverNotification(&notification);
        true
    }
}

fn points_to_pixels(x: f64, y: f64, screen_height: f64, scale: f64) -> (f64, f64) {
    (x * scale, (screen_height - y) * scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_bottom_left_points_to_top_left_pixels() {
        assert_eq!(points_to_pixels(0.0, 0.0, 900.0, 2.0), (0.0, 1800.0));
        assert_eq!(points_to_pixels(100.0, 900.0, 900.0, 2.0), (200.0, 0.0));
        assert_eq!(points_to_pixels(10.0, 890.0, 900.0, 1.0), (10.0, 10.0));
    }
}
