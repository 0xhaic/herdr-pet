//! Platform glue. Only macOS is implemented for the first version.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{float_on_all_spaces, left_button_down, mouse_position, send_notification};

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
