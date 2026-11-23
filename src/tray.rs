//! Menu-bar tray icon and menus.
//!
//! The menus only expose global entry points (show/hide, settings, about,
//! quit); individual settings belong in the settings panel. When the tray
//! icon is hidden, the pet's right-click menu takes over the entries that
//! remain reachable without a hidden pet.

use anyhow::{Context, Result};
use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use winit::window::Window;

/// 22 pt @2x monochrome square plate with a cat-shaped cut-out; drawn as a macOS template image.
const ICON_BYTES: &[u8] = include_bytes!("../assets/tray-icon.png");
const ID_SHOW: &str = "show";
const ID_SETTINGS: &str = "settings";
const ID_ABOUT: &str = "about";
const ID_QUIT: &str = "quit";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    ToggleVisible,
    Settings,
    About,
    Quit,
}

pub struct Tray {
    tray: TrayIcon,
    /// Pet right-click menu, shown while the menu-bar icon is hidden.
    pet_menu: Menu,
}

impl Tray {
    pub fn new() -> Result<Self> {
        let show = MenuItem::with_id(ID_SHOW, "显示/隐藏", true, None);
        let settings = MenuItem::with_id(ID_SETTINGS, "设置…", true, None);
        let about = MenuItem::with_id(ID_ABOUT, "关于", true, None);
        let quit = MenuItem::with_id(ID_QUIT, "退出", true, None);
        let separator = PredefinedMenuItem::separator();

        let menu = Menu::new();
        menu.append_items(&[&show, &settings, &separator, &about, &quit])
            .context("building the tray menu")?;

        // The pet menu leaves out show/hide: hiding the pet would strand the
        // user with neither the menu-bar icon nor a window to right-click.
        let pet_settings = MenuItem::with_id(ID_SETTINGS, "设置…", true, None);
        let pet_about = MenuItem::with_id(ID_ABOUT, "关于", true, None);
        let pet_quit = MenuItem::with_id(ID_QUIT, "退出", true, None);
        let pet_separator = PredefinedMenuItem::separator();
        let pet_menu = Menu::new();
        pet_menu
            .append_items(&[&pet_settings, &pet_about, &pet_separator, &pet_quit])
            .context("building the pet menu")?;

        let icon = decode_icon()?;
        #[cfg(target_os = "macos")]
        let builder = TrayIconBuilder::new().with_icon_templated(icon);
        #[cfg(not(target_os = "macos"))]
        let builder = TrayIconBuilder::new().with_icon(icon);
        let tray = builder
            .with_tooltip("herdr-pet")
            .with_menu(Box::new(menu))
            .build()
            .context("creating the tray icon")?;

        Ok(Self { tray, pet_menu })
    }

    /// Show or hide the menu-bar icon.
    pub fn set_visible(&self, visible: bool) -> Result<()> {
        self.tray
            .set_visible(visible)
            .context("updating the tray icon")
    }

    /// Pop the pet's right-click menu at the pointer.
    pub fn show_pet_menu(&self, window: &Window) {
        crate::platform::show_context_menu(window, &self.pet_menu);
    }
}

/// Drain queued menu clicks without blocking the UI thread.
pub fn poll() -> Vec<Action> {
    let receiver = MenuEvent::receiver();
    let mut actions = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        if let Some(action) = action_for(event.id().0.as_str()) {
            actions.push(action);
        }
    }
    actions
}

fn action_for(id: &str) -> Option<Action> {
    match id {
        ID_SHOW => Some(Action::ToggleVisible),
        ID_SETTINGS => Some(Action::Settings),
        ID_ABOUT => Some(Action::About),
        ID_QUIT => Some(Action::Quit),
        _ => None,
    }
}

fn decode_icon() -> Result<Icon> {
    let image = image::load_from_memory(ICON_BYTES)
        .context("decoding the tray icon")?
        .to_rgba8();
    let (width, height) = image.dimensions();
    Icon::from_rgba(image.into_raw(), width, height).context("invalid tray icon pixels")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_menu_ids_to_actions() {
        assert_eq!(action_for("show"), Some(Action::ToggleVisible));
        assert_eq!(action_for("settings"), Some(Action::Settings));
        assert_eq!(action_for("about"), Some(Action::About));
        assert_eq!(action_for("quit"), Some(Action::Quit));
        assert_eq!(action_for("nope"), None);
    }
}
