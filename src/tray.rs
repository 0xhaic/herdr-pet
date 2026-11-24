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

/// macOS: 22 pt @2x monochrome square plate with a cat-shaped cut-out, drawn
/// as a template image. Windows: a colored icon stays visible on both light
/// and dark taskbars.
#[cfg(target_os = "macos")]
const ICON_BYTES: &[u8] = include_bytes!("../assets/tray-icon.png");
#[cfg(not(target_os = "macos"))]
const ICON_BYTES: &[u8] = include_bytes!("../assets/app-icon.png");
const ID_SHOW: &str = "show";
const ID_SETTINGS: &str = "settings";
const ID_ABOUT: &str = "about";
const ID_QUIT: &str = "quit";
#[cfg(target_os = "windows")]
const ID_CLICK_THROUGH: &str = "click-through";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    ToggleVisible,
    Settings,
    About,
    Quit,
    #[cfg(target_os = "windows")]
    ToggleClickThrough,
}

pub struct Tray {
    tray: TrayIcon,
    /// Pet right-click menu, shown while the menu-bar icon is hidden.
    pet_menu: Menu,
    /// Windows exposes the whole-window pass-through switch in both menus
    /// until the settings panel lands (M3).
    #[cfg(target_os = "windows")]
    click_through_items: Vec<muda::CheckMenuItem>,
}

impl Tray {
    pub fn new() -> Result<Self> {
        let show = MenuItem::with_id(ID_SHOW, "显示/隐藏", true, None);
        let settings = MenuItem::with_id(ID_SETTINGS, "设置…", true, None);
        let about = MenuItem::with_id(ID_ABOUT, "关于", true, None);
        let quit = MenuItem::with_id(ID_QUIT, "退出", true, None);
        let separator = PredefinedMenuItem::separator();
        // The settings panel arrives with M3; until then Windows puts the
        // click-through switch directly in both menus so it stays reachable.
        #[cfg(target_os = "windows")]
        let tray_click_through =
            muda::CheckMenuItem::with_id(ID_CLICK_THROUGH, "点击穿透", true, false, None);
        #[cfg(target_os = "windows")]
        let pet_click_through =
            muda::CheckMenuItem::with_id(ID_CLICK_THROUGH, "点击穿透", true, false, None);

        let menu = Menu::new();
        menu.append_items(&[&show, &settings])
            .context("building the tray menu")?;
        #[cfg(target_os = "windows")]
        menu.append(&tray_click_through)
            .context("adding the click-through switch")?;
        menu.append_items(&[&separator, &about, &quit])
            .context("building the tray menu")?;

        // The pet menu leaves out show/hide: hiding the pet would strand the
        // user with neither the menu-bar icon nor a window to right-click.
        let pet_settings = MenuItem::with_id(ID_SETTINGS, "设置…", true, None);
        let pet_about = MenuItem::with_id(ID_ABOUT, "关于", true, None);
        let pet_quit = MenuItem::with_id(ID_QUIT, "退出", true, None);
        let pet_separator = PredefinedMenuItem::separator();
        let pet_menu = Menu::new();
        pet_menu
            .append(&pet_settings)
            .context("building the pet menu")?;
        #[cfg(target_os = "windows")]
        pet_menu
            .append(&pet_click_through)
            .context("adding the click-through switch")?;
        pet_menu
            .append_items(&[&pet_separator, &pet_about, &pet_quit])
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

        Ok(Self {
            tray,
            pet_menu,
            #[cfg(target_os = "windows")]
            click_through_items: vec![tray_click_through, pet_click_through],
        })
    }

    /// Reflect the whole-window click-through setting in both menus.
    pub fn set_click_through(&self, enabled: bool) {
        #[cfg(target_os = "windows")]
        for item in &self.click_through_items {
            item.set_checked(enabled);
        }
        #[cfg(not(target_os = "windows"))]
        let _ = enabled;
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
        #[cfg(target_os = "windows")]
        ID_CLICK_THROUGH => Some(Action::ToggleClickThrough),
        _ => None,
    }
}

fn decode_icon() -> Result<Icon> {
    let image = image::load_from_memory(ICON_BYTES)
        .context("decoding the tray icon")?
        .to_rgba8();
    // Windows scales tray icons with the taskbar; 32 px covers 200% DPI.
    #[cfg(not(target_os = "macos"))]
    let image = image::imageops::resize(&image, 32, 32, image::imageops::FilterType::Lanczos3);
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
        #[cfg(target_os = "windows")]
        assert_eq!(
            action_for("click-through"),
            Some(Action::ToggleClickThrough)
        );
        assert_eq!(action_for("nope"), None);
    }
}
