//! Menu-bar tray icon and menu.

use anyhow::{Context, Result};
use muda::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// 22 pt @2x monochrome cat-head glyph; drawn as a macOS template image.
const ICON_BYTES: &[u8] = include_bytes!("../assets/tray-icon.png");
const ID_SHOW: &str = "show";
const ID_NEXT_PET: &str = "next-pet";
const ID_BIGGER: &str = "bigger";
const ID_SMALLER: &str = "smaller";
const ID_CLICK_THROUGH: &str = "click-through";
const ID_NOTIFICATIONS: &str = "notifications";
const ID_NOTIFICATION_SOUND: &str = "notification-sound";
const ID_QUIT: &str = "quit";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    ToggleVisible,
    NextPet,
    Bigger,
    Smaller,
    ToggleClickThrough,
    ToggleNotifications,
    ToggleNotificationSound,
    Quit,
}

pub struct Tray {
    _tray: TrayIcon,
    click_through: CheckMenuItem,
    notifications: CheckMenuItem,
    notification_sound: CheckMenuItem,
}

impl Tray {
    pub fn new(notifications: bool, notification_sound: bool, click_through: bool) -> Result<Self> {
        let show = MenuItem::with_id(ID_SHOW, "显示/隐藏", true, None);
        let next = MenuItem::with_id(ID_NEXT_PET, "下一只宠物", true, None);
        let bigger = MenuItem::with_id(ID_BIGGER, "放大", true, None);
        let smaller = MenuItem::with_id(ID_SMALLER, "缩小", true, None);
        let click_through_item =
            CheckMenuItem::with_id(ID_CLICK_THROUGH, "点击穿透", true, click_through, None);
        let notifications_item =
            CheckMenuItem::with_id(ID_NOTIFICATIONS, "提醒", true, notifications, None);
        let notification_sound_item = CheckMenuItem::with_id(
            ID_NOTIFICATION_SOUND,
            "通知音效",
            true,
            notification_sound,
            None,
        );
        let quit = MenuItem::with_id(ID_QUIT, "退出", true, None);
        let separator_a = PredefinedMenuItem::separator();
        let separator_b = PredefinedMenuItem::separator();

        let menu = Menu::new();
        menu.append_items(&[
            &show,
            &next,
            &separator_a,
            &bigger,
            &smaller,
            &click_through_item,
            &notifications_item,
            &notification_sound_item,
            &separator_b,
            &quit,
        ])
        .context("building the tray menu")?;

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
            _tray: tray,
            click_through: click_through_item,
            notifications: notifications_item,
            notification_sound: notification_sound_item,
        })
    }

    pub fn set_click_through(&self, checked: bool) {
        self.click_through.set_checked(checked);
    }

    pub fn set_notifications(&self, checked: bool) {
        self.notifications.set_checked(checked);
    }

    pub fn set_notification_sound(&self, checked: bool) {
        self.notification_sound.set_checked(checked);
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
        ID_NEXT_PET => Some(Action::NextPet),
        ID_BIGGER => Some(Action::Bigger),
        ID_SMALLER => Some(Action::Smaller),
        ID_CLICK_THROUGH => Some(Action::ToggleClickThrough),
        ID_NOTIFICATIONS => Some(Action::ToggleNotifications),
        ID_NOTIFICATION_SOUND => Some(Action::ToggleNotificationSound),
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
        assert_eq!(action_for("next-pet"), Some(Action::NextPet));
        assert_eq!(action_for("bigger"), Some(Action::Bigger));
        assert_eq!(action_for("smaller"), Some(Action::Smaller));
        assert_eq!(
            action_for("click-through"),
            Some(Action::ToggleClickThrough)
        );
        assert_eq!(
            action_for("notifications"),
            Some(Action::ToggleNotifications)
        );
        assert_eq!(
            action_for("notification-sound"),
            Some(Action::ToggleNotificationSound)
        );
        assert_eq!(action_for("quit"), Some(Action::Quit));
        assert_eq!(action_for("nope"), None);
    }
}
