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
