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
/// A rasterized chat overlay for platforms that draw it in software
/// (Windows). Pixels are premultiplied BGRA with tightly packed, top-down
/// rows; macOS uses native views and never produces one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayBitmap {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

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

/// Rebuild the window's hit region from the visible pet frame and any
/// interactive overlay rectangles (the bubble and its toggle button). macOS
/// hit-tests per pixel in `main.rs`; Windows needs an OS-level region so
/// transparent pixels pass clicks to other applications while the drawn UI
/// stays clickable.
#[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
pub fn update_pet_region(
    window: &winit::window::Window,
    atlas: &crate::pet::Atlas,
    frame: crate::pet::Frame,
    pet: crate::chat::Rect,
    extras: &[crate::chat::Rect],
    scale: f64,
    alpha_threshold: u8,
) {
    #[cfg(target_os = "windows")]
    windows::update_pet_region(window, atlas, frame, pet, extras, scale, alpha_threshold);
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

/// Snapshot of the settings panel's controls, shared by the AppKit and Win32
/// panels. Polling diffs against the last snapshot, so neither panel needs
/// per-control change callbacks for sliders or switches.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UiState {
    pub pet: isize,
    pub size: f64,
    pub opacity: f64,
    pub notifications: bool,
    pub notification_sound: bool,
    pub gaze: bool,
    pub click_through: bool,
    pub hide_tray_icon: bool,
    pub chat_open: bool,
}

impl UiState {
    /// Changes the user made since `previous`, in the panel's fixed order.
    pub fn actions_since(&self, previous: &UiState) -> Vec<PanelAction> {
        let mut actions = Vec::new();
        if self.pet != previous.pet && self.pet >= 0 {
            actions.push(PanelAction::SelectPet(self.pet as usize));
        }
        if self.size != previous.size {
            actions.push(PanelAction::SetSize(self.size));
        }
        if self.opacity != previous.opacity {
            actions.push(PanelAction::SetChatOpacity(self.opacity));
        }
        if self.notifications != previous.notifications {
            actions.push(PanelAction::SetNotifications(self.notifications));
        }
        if self.notification_sound != previous.notification_sound {
            actions.push(PanelAction::SetNotificationSound(self.notification_sound));
        }
        if self.gaze != previous.gaze {
            actions.push(PanelAction::SetGaze(self.gaze));
        }
        if self.click_through != previous.click_through {
            actions.push(PanelAction::SetClickThrough(self.click_through));
        }
        if self.hide_tray_icon != previous.hide_tray_icon {
            actions.push(PanelAction::SetHideTrayIcon(self.hide_tray_icon));
        }
        if self.chat_open != previous.chat_open {
            actions.push(PanelAction::SetChatOpen(self.chat_open));
        }
        actions
    }
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

    pub fn take_bitmap(&mut self) -> Option<OverlayBitmap> {
        None
    }
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

#[cfg(test)]
mod tests {
    use super::{PanelAction, UiState};

    fn base() -> UiState {
        UiState {
            pet: 0,
            size: 104.0,
            opacity: 0.7,
            notifications: true,
            notification_sound: true,
            gaze: true,
            click_through: false,
            hide_tray_icon: false,
            chat_open: true,
        }
    }

    #[test]
    fn identical_state_produces_no_actions() {
        assert!(base().actions_since(&base()).is_empty());
    }

    #[test]
    fn each_control_maps_to_its_action() {
        let mut next = base();
        next.pet = 2;
        assert_eq!(next.actions_since(&base()), vec![PanelAction::SelectPet(2)]);
        let mut next = base();
        next.size = 156.0;
        assert_eq!(
            next.actions_since(&base()),
            vec![PanelAction::SetSize(156.0)]
        );
        let mut next = base();
        next.opacity = 0.25;
        assert_eq!(
            next.actions_since(&base()),
            vec![PanelAction::SetChatOpacity(0.25)]
        );
        let mut next = base();
        next.notifications = false;
        assert_eq!(
            next.actions_since(&base()),
            vec![PanelAction::SetNotifications(false)]
        );
        let mut next = base();
        next.notification_sound = false;
        assert_eq!(
            next.actions_since(&base()),
            vec![PanelAction::SetNotificationSound(false)]
        );
        let mut next = base();
        next.gaze = false;
        assert_eq!(
            next.actions_since(&base()),
            vec![PanelAction::SetGaze(false)]
        );
        let mut next = base();
        next.click_through = true;
        assert_eq!(
            next.actions_since(&base()),
            vec![PanelAction::SetClickThrough(true)]
        );
        let mut next = base();
        next.hide_tray_icon = true;
        assert_eq!(
            next.actions_since(&base()),
            vec![PanelAction::SetHideTrayIcon(true)]
        );
        let mut next = base();
        next.chat_open = false;
        assert_eq!(
            next.actions_since(&base()),
            vec![PanelAction::SetChatOpen(false)]
        );
    }

    #[test]
    fn an_empty_pet_list_does_not_select_anything() {
        let mut next = base();
        next.pet = -1;
        assert!(next.actions_since(&base()).is_empty());
    }
}
