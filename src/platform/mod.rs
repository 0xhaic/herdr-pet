//! Platform glue. Only macOS is implemented for the first version.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{
    ChatOverlay, SettingsPanel, allow_window_above_menu_bar, float_on_all_spaces,
    herdr_app_frontmost, herdr_app_path, left_button_down, menu_bar_height, mouse_position,
    send_notification, show_about,
};

/// Settings-panel controls the app should apply. The panel itself is
/// platform-specific; poll it once per tick and dispatch the changes.
#[derive(Debug, Clone, PartialEq)]
pub enum PanelAction {
    SelectPet(usize),
    SetSize(f64),
    SetChatOpacity(f64),
    SetNotifications(bool),
    SetNotificationSound(bool),
    SetGaze(bool),
    SetClickThrough(bool),
    SetChatOpen(bool),
    SetPetsDir(std::path::PathBuf),
    ResetPetsDir,
}

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

    pub fn layout(&self, _layout: &crate::chat::ChatLayout, _height: f64, _pager: bool) {}

    pub fn set_text(&self, _title: &str, _subtitle: &str) {}

    pub fn set_icon(&self, _icon: Option<(&std::path::Path, bool)>) {}

    pub fn set_visible(&self, _bubble: bool, _button: bool) {}

    pub fn set_opacity(&self, _opacity: f64) {}
}

#[cfg(not(target_os = "macos"))]
pub struct SettingsPanel;

#[cfg(not(target_os = "macos"))]
impl SettingsPanel {
    pub fn new(
        _pets: &[String],
        _selected: usize,
        _config: &crate::config::Config,
        _pets_dir: &std::path::Path,
        _custom_dir: bool,
    ) -> Option<Self> {
        None
    }

    pub fn show(&self) {}

    pub fn poll(&mut self) -> Vec<PanelAction> {
        Vec::new()
    }

    pub fn sync(&mut self, _config: &crate::config::Config, _selected: usize) {}

    pub fn sync_pets(
        &mut self,
        _pets: &[String],
        _selected: usize,
        _pets_dir: &std::path::Path,
        _custom_dir: bool,
    ) {
    }
}

#[cfg(not(target_os = "macos"))]
pub fn float_on_all_spaces(_window: &winit::window::Window) -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn allow_window_above_menu_bar(_window: &winit::window::Window) -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn menu_bar_height(_monitor: &winit::monitor::MonitorHandle) -> f64 {
    0.0
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
