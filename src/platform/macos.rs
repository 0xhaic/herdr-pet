//! macOS global mouse state via NSEvent.
//!
//! NSEvent mouseLocation is in points with the origin at the bottom-left of
//! the primary screen. We convert to top-left-origin physical pixels so the
//! rest of the app can compare it with winit window positions.

use std::path::PathBuf;

use objc2_app_kit::{NSEvent, NSView, NSWindowCollectionBehavior, NSWorkspace};
use objc2_foundation::NSString;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

pub use overlay::ChatOverlay;
pub use settings::SettingsPanel;

const HERDR_APP_BUNDLE_ID: &str = "dev.herdr.app";

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

/// Whether Herdr.app is the frontmost application.
pub fn herdr_app_frontmost() -> bool {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .and_then(|application| application.bundleIdentifier())
        .is_some_and(|id| id.to_string() == HERDR_APP_BUNDLE_ID)
}

/// Herdr.app's executable, which doubles as the `herdr-app` CLI. Asking
/// LaunchServices keeps this working when the app is not in /Applications.
pub fn herdr_app_path() -> Option<PathBuf> {
    let url = NSWorkspace::sharedWorkspace()
        .URLForApplicationWithBundleIdentifier(&NSString::from_str(HERDR_APP_BUNDLE_ID))?;
    let path = url.path()?;
    Some(PathBuf::from(path.to_string()).join("Contents/MacOS/Herdr"))
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
/// the modal alert blocks the event loop until dismissed, and a click
/// anywhere outside the dialog dismisses it.
pub fn show_about() {
    about::show();
}

mod overlay {
    use objc2::rc::Retained;
    use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{
        NSBox, NSBoxType, NSColor, NSFont, NSImage, NSImageView, NSLineBreakMode, NSTextField,
        NSTitlePosition, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
        NSVisualEffectState, NSVisualEffectView,
    };
    use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    use crate::chat::{BUBBLE_HEIGHT, BUBBLE_WIDTH, BUTTON_SIZE, ChatLayout};

    const TITLE_INSET_TOP: f64 = 12.0;
    const TEXT_INSET_X: f64 = 16.0;
    const TEXT_LINE_HEIGHT: f64 = 16.0;
    const TEXT_LINE_GAP: f64 = 4.0;
    const CHEVRON_SIZE: f64 = 13.0;

    // The glass views are display-only: returning nil from the container's hit
    // test lets clicks fall through to winit's backing view, which owns the
    // drag/toggle behaviour. Without this the effect views would swallow the
    // arrow clicks before winit sees them.
    define_class!(
        // SAFETY: NSView has no subclassing requirements and this class has no
        // ivars or Drop implementation.
        #[unsafe(super(NSView))]
        #[thread_kind = MainThreadOnly]
        struct ClickThroughView;

        impl ClickThroughView {
            #[unsafe(method(hitTest:))]
            fn hit_test(&self, _point: NSPoint) -> *mut NSView {
                std::ptr::null_mut()
            }
        }

        unsafe impl NSObjectProtocol for ClickThroughView {}
    );

    pub struct ChatOverlay {
        bubble: Retained<NSVisualEffectView>,
        bubble_tint: Retained<NSBox>,
        title: Retained<NSTextField>,
        subtitle: Retained<NSTextField>,
        button: Retained<NSVisualEffectView>,
        button_tint: Retained<NSBox>,
        chevron: Retained<NSImageView>,
    }

    impl ChatOverlay {
        /// Attach the glass bubble and the toggle button to the winit view.
        /// Returns None when AppKit handles are unavailable.
        pub fn new(window: &Window, open: bool, opacity: f64) -> Option<Self> {
            let mtm = MainThreadMarker::new()?;
            let content = content_view(window)?;
            let container: Retained<ClickThroughView> = unsafe {
                msg_send![
                    mtm.alloc::<ClickThroughView>(),
                    initWithFrame: content.bounds()
                ]
            };
            container.setAutoresizingMask(
                objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
                    | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
            );

            let (bubble, bubble_tint) = glass_view(
                mtm,
                BUBBLE_HEIGHT / 2.0,
                NSColor::clearColor(),
                NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.09),
            );
            bubble.setAlphaValue(opacity);
            let title = label(
                mtm,
                NSFont::boldSystemFontOfSize(13.0),
                NSColor::colorWithCalibratedWhite_alpha(1.0, 0.95),
            );
            let subtitle = label(
                mtm,
                NSFont::systemFontOfSize(13.0),
                NSColor::colorWithCalibratedWhite_alpha(0.78, 0.88),
            );
            // Labels sit above the glass rather than inside it, so the
            // bubble's own opacity does not fade the text.
            container.addSubview(&bubble);
            container.addSubview(&title);
            container.addSubview(&subtitle);

            let (button, button_tint) = glass_view(
                mtm,
                BUTTON_SIZE / 2.0,
                NSColor::colorWithSRGBRed_green_blue_alpha(0.24, 0.27, 0.34, 0.58),
                NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.12),
            );
            let chevron = NSImageView::new(mtm);
            chevron.setContentTintColor(Some(&NSColor::colorWithCalibratedWhite_alpha(1.0, 0.92)));
            button.addSubview(&chevron);

            container.addSubview(&button);
            content.addSubview(&container);

            let overlay = Self {
                bubble,
                bubble_tint,
                title,
                subtitle,
                button,
                button_tint,
                chevron,
            };
            overlay.set_visible(open, true);
            Some(overlay)
        }

        /// Reflow in AppKit's bottom-left point coordinates from the top-left
        /// logical layout. Called on creation and whenever the window resizes.
        pub fn layout(&self, layout: &ChatLayout, height: f64) {
            let bubble = NSRect::new(
                NSPoint::new(layout.bubble.x, height - layout.bubble.y - layout.bubble.h),
                NSSize::new(layout.bubble.w, layout.bubble.h),
            );
            self.bubble.setFrame(bubble);
            self.bubble_tint.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(layout.bubble.w, layout.bubble.h),
            ));
            let text_width = BUBBLE_WIDTH - TEXT_INSET_X * 2.0;
            self.title.setFrame(NSRect::new(
                NSPoint::new(
                    bubble.origin.x + TEXT_INSET_X,
                    bubble.origin.y + BUBBLE_HEIGHT - TITLE_INSET_TOP - TEXT_LINE_HEIGHT,
                ),
                NSSize::new(text_width, TEXT_LINE_HEIGHT),
            ));
            self.subtitle.setFrame(NSRect::new(
                NSPoint::new(
                    bubble.origin.x + TEXT_INSET_X,
                    bubble.origin.y + BUBBLE_HEIGHT
                        - TITLE_INSET_TOP
                        - TEXT_LINE_HEIGHT
                        - TEXT_LINE_GAP
                        - TEXT_LINE_HEIGHT,
                ),
                NSSize::new(text_width, TEXT_LINE_HEIGHT),
            ));
            self.button.setFrame(NSRect::new(
                NSPoint::new(layout.button.x, height - layout.button.y - layout.button.h),
                NSSize::new(layout.button.w, layout.button.h),
            ));
            self.button_tint.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(layout.button.w, layout.button.h),
            ));
            self.chevron.setFrame(NSRect::new(
                NSPoint::new(
                    (BUTTON_SIZE - CHEVRON_SIZE) / 2.0,
                    (BUTTON_SIZE - CHEVRON_SIZE) / 2.0,
                ),
                NSSize::new(CHEVRON_SIZE, CHEVRON_SIZE),
            ));
        }

        pub fn set_text(&self, title: &str, subtitle: &str) {
            self.title.setStringValue(&NSString::from_str(title));
            self.subtitle.setStringValue(&NSString::from_str(subtitle));
        }

        pub fn set_opacity(&self, opacity: f64) {
            self.bubble.setAlphaValue(opacity);
        }

        /// Hide the bubble and/or the button. The chevron follows the bubble:
        /// down when expanded, up when collapsed or auto-hidden.
        pub fn set_visible(&self, bubble: bool, button: bool) {
            self.bubble.setHidden(!bubble);
            self.title.setHidden(!bubble);
            self.subtitle.setHidden(!bubble);
            self.button.setHidden(!button);
            let symbol = if bubble { "chevron.down" } else { "chevron.up" };
            if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str(symbol),
                None,
            ) {
                image.setSize(NSSize::new(CHEVRON_SIZE, CHEVRON_SIZE));
                self.chevron.setImage(Some(&image));
            }
        }
    }

    fn content_view(window: &Window) -> Option<Retained<NSView>> {
        let handle = window.window_handle().ok()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return None;
        };
        // SAFETY: `ChatOverlay::new` runs on the main thread while winit keeps
        // the NSView alive for the window's lifetime.
        let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
        view.window()?.contentView()
    }

    fn glass_view(
        mtm: MainThreadMarker,
        radius: f64,
        fill: Retained<NSColor>,
        border: Retained<NSColor>,
    ) -> (Retained<NSVisualEffectView>, Retained<NSBox>) {
        let view = NSVisualEffectView::new(mtm);
        view.setMaterial(NSVisualEffectMaterial::HUDWindow);
        view.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        view.setState(NSVisualEffectState::Active);
        view.setWantsLayer(true);
        if let Some(layer) = view.layer() {
            layer.setCornerRadius(radius);
            layer.setMasksToBounds(true);
        }

        let tint = NSBox::new(mtm);
        tint.setBoxType(NSBoxType::Custom);
        tint.setTitlePosition(NSTitlePosition::NoTitle);
        tint.setTransparent(false);
        tint.setBorderWidth(1.0);
        tint.setCornerRadius(radius);
        tint.setFillColor(&fill);
        tint.setBorderColor(&border);
        view.addSubview(&tint);
        (view, tint)
    }

    fn label(
        mtm: MainThreadMarker,
        font: Retained<NSFont>,
        color: Retained<NSColor>,
    ) -> Retained<NSTextField> {
        let label = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        label.setFont(Some(&font));
        label.setTextColor(Some(&color));
        label.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        label.setUsesSingleLineMode(true);
        label.setMaximumNumberOfLines(1);
        label
    }
}

mod settings {
    use objc2::rc::Retained;
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSApplication, NSBackingStoreType, NSButton, NSButtonType, NSColor, NSControlStateValueOff,
        NSControlStateValueOn, NSFont, NSPopUpButton, NSSlider, NSTextField, NSView, NSWindow,
        NSWindowStyleMask,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    use crate::config::{Config, MAX_SIZE, MIN_SIZE};
    use crate::platform::PanelAction;

    const WIDTH: f64 = 320.0;
    const HEIGHT: f64 = 330.0;
    const PAD: f64 = 20.0;
    const LABEL_WIDTH: f64 = 96.0;
    const CONTROL_X: f64 = 116.0;
    const CONTROL_WIDTH: f64 = WIDTH - CONTROL_X - PAD;
    const ROW_HEIGHT: f64 = 24.0;
    const PET_TOP: f64 = 20.0;
    const SIZE_TOP: f64 = 58.0;
    const OPACITY_TOP: f64 = 96.0;
    const TOGGLE_TOP: f64 = 140.0;
    const TOGGLE_STEP: f64 = 28.0;
    const FOOTER_TOP: f64 = 296.0;

    /// Snapshot of the controls as they were last handed to or read from the
    /// app; polling diffs against it, so no AppKit callbacks are needed.
    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    struct UiState {
        pet: isize,
        size: f64,
        opacity: f64,
        notifications: bool,
        notification_sound: bool,
        gaze: bool,
        click_through: bool,
        chat_open: bool,
    }

    pub struct SettingsPanel {
        window: Retained<NSWindow>,
        pet: Retained<NSPopUpButton>,
        size: Retained<NSSlider>,
        opacity: Retained<NSSlider>,
        notifications: Retained<NSButton>,
        notification_sound: Retained<NSButton>,
        gaze: Retained<NSButton>,
        click_through: Retained<NSButton>,
        chat_open: Retained<NSButton>,
        last: UiState,
    }

    impl SettingsPanel {
        pub fn new(pets: &[String], selected: usize, config: &Config) -> Option<Self> {
            let mtm = MainThreadMarker::new()?;
            // SAFETY: `initWithContentRect...` is the designated initializer;
            // the frame is static and the window is reused after closing.
            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, HEIGHT)),
                    NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            window.setTitle(&NSString::from_str("herdr-pet 设置"));
            // The panel is owned by `App`; closing it must only order it out.
            // SAFETY: the setter is otherwise safe to call on the main thread.
            unsafe { window.setReleasedWhenClosed(false) };
            window.center();
            let content = window.contentView()?;

            let pet_label = label(mtm, "宠物", 13.0);
            place(&pet_label, PAD, PET_TOP + 4.0, LABEL_WIDTH, 16.0);
            content.addSubview(&pet_label);

            let pet = NSPopUpButton::initWithFrame_pullsDown(
                NSPopUpButton::alloc(mtm),
                NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(CONTROL_WIDTH, ROW_HEIGHT),
                ),
                false,
            );
            for name in pets {
                pet.addItemWithTitle(&NSString::from_str(name));
            }
            place(&pet, CONTROL_X, PET_TOP, CONTROL_WIDTH, ROW_HEIGHT);
            content.addSubview(&pet);

            let size_label = label(mtm, "大小", 13.0);
            place(&size_label, PAD, SIZE_TOP + 4.0, LABEL_WIDTH, 16.0);
            content.addSubview(&size_label);
            let size = slider(mtm, MIN_SIZE, MAX_SIZE);
            place(&size, CONTROL_X, SIZE_TOP, CONTROL_WIDTH, ROW_HEIGHT);
            content.addSubview(&size);

            let opacity_label = label(mtm, "气泡不透明度", 13.0);
            place(&opacity_label, PAD, OPACITY_TOP + 4.0, LABEL_WIDTH, 16.0);
            content.addSubview(&opacity_label);
            let opacity = slider(mtm, 0.0, 1.0);
            place(&opacity, CONTROL_X, OPACITY_TOP, CONTROL_WIDTH, ROW_HEIGHT);
            content.addSubview(&opacity);

            let toggles = [
                ("提醒", TOGGLE_TOP),
                ("通知音效", TOGGLE_TOP + TOGGLE_STEP),
                ("凝视", TOGGLE_TOP + TOGGLE_STEP * 2.0),
                ("点击穿透", TOGGLE_TOP + TOGGLE_STEP * 3.0),
                ("聊天气泡展开", TOGGLE_TOP + TOGGLE_STEP * 4.0),
            ];
            let mut buttons = Vec::with_capacity(toggles.len());
            for (title, top) in toggles {
                let button = checkbox(mtm, title);
                place(&button, PAD, top, WIDTH - PAD * 2.0, 20.0);
                content.addSubview(&button);
                buttons.push(button);
            }
            let [
                notifications,
                notification_sound,
                gaze,
                click_through,
                chat_open,
            ] = <[Retained<NSButton>; 5]>::try_from(buttons).ok()?;

            let footer = label(mtm, "修改会立即生效并保存", 11.0);
            footer.setTextColor(Some(&NSColor::secondaryLabelColor()));
            place(&footer, PAD, FOOTER_TOP, WIDTH - PAD * 2.0, 16.0);
            content.addSubview(&footer);

            let mut panel = Self {
                window,
                pet,
                size,
                opacity,
                notifications,
                notification_sound,
                gaze,
                click_through,
                chat_open,
                last: UiState::default(),
            };
            panel.sync(config, selected);
            Some(panel)
        }

        pub fn show(&self) {
            let Some(mtm) = MainThreadMarker::new() else {
                return;
            };
            // Accessory apps are not activated by a menu-bar click; without
            // this the window cannot become key and the controls stay inert.
            NSApplication::sharedApplication(mtm).activate();
            self.window.makeKeyAndOrderFront(None);
        }

        /// Changes the user made since the last poll. Controls without
        /// target/action still track their own state, so reading them is
        /// enough to support live sliders and toggles.
        pub fn poll(&mut self) -> Vec<PanelAction> {
            let current = self.read();
            let actions = diff(&self.last, &current);
            self.last = current;
            actions
        }

        /// Push the app's config into the controls, e.g. after a tray action
        /// changed it while the panel was open.
        pub fn sync(&mut self, config: &Config, selected: usize) {
            let count = self.pet.numberOfItems();
            if count > 0 {
                self.pet
                    .selectItemAtIndex(selected.min(count as usize - 1) as isize);
            }
            self.size.setDoubleValue(config.size);
            self.opacity.setDoubleValue(config.chat_opacity);
            set_checked(&self.notifications, config.notifications);
            set_checked(&self.notification_sound, config.notification_sound);
            set_checked(&self.gaze, config.gaze);
            set_checked(&self.click_through, config.click_through);
            set_checked(&self.chat_open, config.chat_open);
            self.last = self.read();
        }

        fn read(&self) -> UiState {
            UiState {
                pet: self.pet.indexOfSelectedItem(),
                size: self.size.doubleValue(),
                opacity: self.opacity.doubleValue(),
                notifications: is_checked(&self.notifications),
                notification_sound: is_checked(&self.notification_sound),
                gaze: is_checked(&self.gaze),
                click_through: is_checked(&self.click_through),
                chat_open: is_checked(&self.chat_open),
            }
        }
    }

    fn diff(previous: &UiState, current: &UiState) -> Vec<PanelAction> {
        let mut actions = Vec::new();
        if current.pet != previous.pet && current.pet >= 0 {
            actions.push(PanelAction::SelectPet(current.pet as usize));
        }
        if current.size != previous.size {
            actions.push(PanelAction::SetSize(current.size));
        }
        if current.opacity != previous.opacity {
            actions.push(PanelAction::SetChatOpacity(current.opacity));
        }
        if current.notifications != previous.notifications {
            actions.push(PanelAction::SetNotifications(current.notifications));
        }
        if current.notification_sound != previous.notification_sound {
            actions.push(PanelAction::SetNotificationSound(
                current.notification_sound,
            ));
        }
        if current.gaze != previous.gaze {
            actions.push(PanelAction::SetGaze(current.gaze));
        }
        if current.click_through != previous.click_through {
            actions.push(PanelAction::SetClickThrough(current.click_through));
        }
        if current.chat_open != previous.chat_open {
            actions.push(PanelAction::SetChatOpen(current.chat_open));
        }
        actions
    }

    /// Frame from top-left logical coordinates; AppKit views are placed from
    /// the content view's bottom-left corner.
    fn place(view: &NSView, x: f64, top: f64, width: f64, height: f64) {
        view.setFrame(NSRect::new(
            NSPoint::new(x, HEIGHT - top - height),
            NSSize::new(width, height),
        ));
    }

    fn label(mtm: MainThreadMarker, text: &str, size: f64) -> Retained<NSTextField> {
        let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
        label.setFont(Some(&NSFont::systemFontOfSize(size)));
        label
    }

    fn slider(mtm: MainThreadMarker, min: f64, max: f64) -> Retained<NSSlider> {
        let slider = NSSlider::new(mtm);
        slider.setMinValue(min);
        slider.setMaxValue(max);
        slider.setContinuous(true);
        slider
    }

    fn checkbox(mtm: MainThreadMarker, title: &str) -> Retained<NSButton> {
        let button = NSButton::new(mtm);
        button.setButtonType(NSButtonType::Switch);
        button.setTitle(&NSString::from_str(title));
        button
    }

    fn is_checked(button: &NSButton) -> bool {
        button.state() == NSControlStateValueOn
    }

    fn set_checked(button: &NSButton, checked: bool) {
        button.setState(if checked {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn base() -> UiState {
            UiState {
                pet: 0,
                size: 104.0,
                opacity: 0.7,
                notifications: true,
                notification_sound: true,
                gaze: true,
                click_through: false,
                chat_open: true,
            }
        }

        #[test]
        fn identical_state_produces_no_actions() {
            assert!(diff(&base(), &base()).is_empty());
        }

        #[test]
        fn each_control_maps_to_its_action() {
            let mut next = base();
            next.pet = 2;
            assert_eq!(diff(&base(), &next), vec![PanelAction::SelectPet(2)]);

            let mut next = base();
            next.size = 156.0;
            assert_eq!(diff(&base(), &next), vec![PanelAction::SetSize(156.0)]);

            let mut next = base();
            next.opacity = 0.25;
            assert_eq!(
                diff(&base(), &next),
                vec![PanelAction::SetChatOpacity(0.25)]
            );

            let mut next = base();
            next.notifications = false;
            assert_eq!(
                diff(&base(), &next),
                vec![PanelAction::SetNotifications(false)]
            );

            let mut next = base();
            next.notification_sound = false;
            assert_eq!(
                diff(&base(), &next),
                vec![PanelAction::SetNotificationSound(false)]
            );

            let mut next = base();
            next.gaze = false;
            assert_eq!(diff(&base(), &next), vec![PanelAction::SetGaze(false)]);

            let mut next = base();
            next.click_through = true;
            assert_eq!(
                diff(&base(), &next),
                vec![PanelAction::SetClickThrough(true)]
            );

            let mut next = base();
            next.chat_open = false;
            assert_eq!(diff(&base(), &next), vec![PanelAction::SetChatOpen(false)]);
        }

        #[test]
        fn empty_pet_popup_is_ignored() {
            let previous = base();
            let mut current = base();
            current.pet = -1;
            assert!(diff(&previous, &current).is_empty());
        }
    }
}

mod about {
    use std::ptr::NonNull;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{
        NSAlert, NSAlertFirstButtonReturn, NSApplication, NSEvent, NSEventMask, NSImage,
        NSWorkspace,
    };
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

        // Dismiss on any click outside the dialog. Clicks delivered to other
        // applications (desktop, other windows) only reach a global monitor;
        // clicks on our own windows (pet, menu bar) only reach a local one,
        // so both are installed for the modal session.
        let outside_click =
            NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::OtherMouseDown;
        let dialog = alert.window();
        let local_dialog = dialog.clone();
        let local_handler: RcBlock<dyn Fn(NonNull<NSEvent>) -> *mut NSEvent> =
            RcBlock::new(move |event: NonNull<NSEvent>| {
                // SAFETY: AppKit passes a live event for the duration of the handler.
                let event_ref = unsafe { event.as_ref() };
                let inside_dialog = event_ref.window(mtm).is_some_and(|window| {
                    std::ptr::eq(Retained::as_ptr(&window), Retained::as_ptr(&local_dialog))
                });
                if inside_dialog {
                    event.as_ptr()
                } else {
                    NSApplication::sharedApplication(mtm).abortModal();
                    std::ptr::null_mut()
                }
            });
        // SAFETY: the handler returns `event` unchanged or null.
        let local = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(outside_click, &local_handler)
        };
        let global_handler: RcBlock<dyn Fn(NonNull<NSEvent>)> =
            RcBlock::new(move |_event: NonNull<NSEvent>| {
                NSApplication::sharedApplication(mtm).abortModal();
            });
        let global =
            NSEvent::addGlobalMonitorForEventsMatchingMask_handler(outside_click, &global_handler);

        let response = alert.runModal();
        for monitor in [local, global].into_iter().flatten() {
            // SAFETY: each token came from the matching add call above.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
        if response == NSAlertFirstButtonReturn
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
