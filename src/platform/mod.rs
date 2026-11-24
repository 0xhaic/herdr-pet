//! Platform glue. macOS and Windows each provide their own implementation;
//! other targets keep inert stubs so the shared logic still compiles.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
pub use macos::{
    ChatOverlay, SettingsPanel, allow_window_above_menu_bar, float_on_all_spaces,
    herdr_app_frontmost, herdr_app_path, left_button_down, menu_bar_height, mouse_position,
    send_notification, show_about, show_context_menu,
};
#[cfg(target_os = "windows")]
pub use windows::{
    ChatOverlay, SettingsPanel, allow_window_above_menu_bar, float_on_all_spaces,
    herdr_app_frontmost, herdr_app_path, left_button_down, menu_bar_height, mouse_position,
    send_notification, show_about, show_context_menu,
};

/// Toggle whether the window receives the mouse at the current pointer
/// position. macOS delegates to winit; Windows builds a window region from
/// the pet's alpha mask instead, so this per-tick call is a no-op there.
pub fn set_cursor_hittest(window: &winit::window::Window, hittest: bool) {
    #[cfg(target_os = "windows")]
    windows::set_cursor_hittest(window, hittest);
    #[cfg(not(target_os = "windows"))]
    {
        let _ = window.set_cursor_hittest(hittest);
    }
}

/// Apply the whole-window "click through" setting. macOS disables mouse
/// events on the window; Windows toggles `WS_EX_TRANSPARENT`.
pub fn set_click_through(window: &winit::window::Window, click_through: bool) {
    #[cfg(target_os = "windows")]
    windows::set_click_through(window, click_through);
    #[cfg(not(target_os = "windows"))]
    {
        let _ = window.set_cursor_hittest(!click_through);
    }
}

/// One-time per-window platform setup. Windows adds focus-free styles and the
/// hit-test subclass; other platforms need none.
#[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
pub fn prepare_window(window: &winit::window::Window) {
    #[cfg(target_os = "windows")]
    windows::prepare_window(window);
}

/// One-time notification setup. Windows registers the toast app id before the
/// first reminder; other platforms need none.
pub fn prepare_notifications() {
    #[cfg(target_os = "windows")]
    windows::prepare_notifications();
}

/// Play the platform alert sound. macOS plays it through `afplay` in
/// `notify.rs`; only Windows routes through here.
#[cfg(target_os = "windows")]
pub fn play_notification_sound(blocked: bool) {
    windows::play_notification_sound(blocked);
}

/// Rebuild the window's hit region from the visible pet frame. macOS hit-tests
/// per pixel in `main.rs`; Windows needs an OS-level region so transparent
/// pixels pass clicks to other applications.
#[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
pub fn update_pet_region(
    window: &winit::window::Window,
    atlas: &crate::pet::Atlas,
    frame: crate::pet::Frame,
    pet: crate::chat::Rect,
    scale: f64,
    alpha_threshold: u8,
) {
    #[cfg(target_os = "windows")]
    windows::update_pet_region(window, atlas, frame, pet, scale, alpha_threshold);
}

/// Usable screen area in physical pixels. Windows reports the monitor's work
/// area (taskbar excluded wherever it sits); macOS keeps its menu-bar-aware
/// computation in `main.rs`.
#[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
pub fn work_area(monitor: &winit::monitor::MonitorHandle) -> Option<crate::chat::Rect> {
    #[cfg(target_os = "windows")]
    {
        windows::work_area(monitor)
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// Settings-panel controls the app should apply. The panel itself is
/// platform-specific; poll it once per tick and dispatch the changes. The
/// Windows panel that constructs these actions lands in M3.
#[cfg_attr(target_os = "windows", allow(dead_code))]
#[derive(Debug, Clone, PartialEq)]
pub enum PanelAction {
    SelectPet(usize),
    SetSize(f64),
    SetChatOpacity(f64),
    SetNotifications(bool),
    SetNotificationSound(bool),
    SetGaze(bool),
    SetClickThrough(bool),
    SetHideTrayIcon(bool),
    SetChatOpen(bool),
    SetPetsDir(std::path::PathBuf),
    ResetPetsDir,
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn herdr_app_frontmost() -> bool {
    false
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn herdr_app_path() -> Option<std::path::PathBuf> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub struct ChatOverlay;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl ChatOverlay {
    pub fn new(_window: &winit::window::Window, _open: bool, _opacity: f64) -> Option<Self> {
        None
    }

    pub fn layout(&self, _layout: &crate::chat::ChatLayout, _height: f64, _pager: bool) {}

    pub fn set_text(&self, _title: &str, _subtitle: &str) {}

    pub fn set_icon(&self, _icon: Option<(&std::path::Path, bool)>) {}

    pub fn set_status(&self, _status: Option<crate::status::AgentStatus>) {}

    pub fn set_visible(&self, _bubble: bool, _button: bool) {}

    pub fn set_opacity(&self, _opacity: f64) {}
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub struct SettingsPanel;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
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

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn float_on_all_spaces(_window: &winit::window::Window) -> bool {
    false
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn allow_window_above_menu_bar(_window: &winit::window::Window) -> bool {
    false
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn menu_bar_height(_monitor: &winit::monitor::MonitorHandle) -> f64 {
    0.0
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn mouse_position(_screen_height: f64, _scale: f64) -> Option<(f64, f64)> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn left_button_down() -> bool {
    false
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn send_notification(_message: &str) -> bool {
    false
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn show_about() {}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn show_context_menu(_window: &winit::window::Window, _menu: &muda::Menu) {}
