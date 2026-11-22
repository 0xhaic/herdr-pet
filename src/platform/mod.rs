//! Platform glue. Only macOS is implemented for the first version.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{
    ChatOverlay, float_on_all_spaces, herdr_app_frontmost, herdr_app_path, left_button_down,
    mouse_position, send_notification, show_about,
};

#[cfg(not(target_os = "macos"))]
pub fn herdr_app_frontmost() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn herdr_app_path() -> Option<std::path::PathBuf> {
    None
}

#[cfg(not(target_os = "macos"))]
pub struct ChatOverlay;

#[cfg(not(target_os = "macos"))]
impl ChatOverlay {
    pub fn new(_window: &winit::window::Window, _open: bool, _opacity: f64) -> Option<Self> {
        None
    }

    pub fn layout(&self, _layout: &crate::chat::ChatLayout, _height: f64) {}

    pub fn set_text(&self, _title: &str, _subtitle: &str) {}

    pub fn set_visible(&self, _bubble: bool, _button: bool) {}
}

#[cfg(not(target_os = "macos"))]
pub fn float_on_all_spaces(_window: &winit::window::Window) -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn mouse_position(_screen_height: f64, _scale: f64) -> Option<(f64, f64)> {
    None
}

#[cfg(not(target_os = "macos"))]
pub fn left_button_down() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn send_notification(_message: &str) -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn show_about() {}
