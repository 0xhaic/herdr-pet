//! macOS global mouse state via NSEvent.
//!
//! NSEvent mouseLocation is in points with the origin at the bottom-left of
//! the primary screen. We convert to top-left-origin physical pixels so the
//! rest of the app can compare it with winit window positions.

use std::path::PathBuf;

use objc2::runtime::{AnyObject, Imp, Sel};
use objc2::{ffi, sel};
use objc2_app_kit::{NSEvent, NSScreen, NSView, NSWindowCollectionBehavior, NSWorkspace};
use objc2_foundation::{NSRect, NSString};
use winit::platform::macos::MonitorHandleExtMacOS;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

pub use overlay::ChatOverlay;
pub use settings::SettingsPanel;

const HERDR_APP_BUNDLE_ID: &str = "dev.herdr.app";
/// Classic menu bar height in points, used when AppKit cannot map a monitor
/// to its `NSScreen`.
const MENU_BAR_FALLBACK: f64 = 25.0;

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

/// Let the window frame move above the menu bar. macOS constrains a visible
/// window's top edge to the menu bar, which would keep the pet (drawn below
/// the bubble) from ever reaching the status bar. Overrides
/// `constrainFrameRect:toScreen:` on winit's window class to return the
/// proposed frame unchanged. Returns false if the NSWindow is unavailable.
pub fn allow_window_above_menu_bar(window: &Window) -> bool {
    let Ok(handle) = window.window_handle() else {
        return false;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return false;
    };
    // SAFETY: `resumed` runs on the main thread and winit keeps the NSView
    // alive for the window's lifetime.
    let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    let Some(ns_window) = view.window() else {
        return false;
    };
    unsafe {
        let class = ffi::object_getClass(&*ns_window as *const _ as *const AnyObject);
        let selector = sel!(constrainFrameRect:toScreen:);
        let inherited = ffi::class_getInstanceMethod(class, selector);
        if inherited.is_null() {
            return false;
        }
        let types = ffi::method_getTypeEncoding(inherited);
        let signature = allow_frame
            as extern "C-unwind" fn(&AnyObject, Sel, NSRect, Option<&NSScreen>) -> NSRect;
        // SAFETY: the signature mirrors NSWindow's inherited method, whose
        // type encoding is reused above.
        let imp: Imp = std::mem::transmute(signature);
        // False when an earlier window already added the override.
        ffi::class_addMethod(class.cast_mut(), selector, imp, types);
    }
    true
}

/// Return the proposed frame unchanged instead of constraining it to the
/// screen's visible area.
extern "C-unwind" fn allow_frame(
    _: &AnyObject,
    _: Sel,
    rect: NSRect,
    _: Option<&NSScreen>,
) -> NSRect {
    rect
}

/// Menu bar height in physical pixels for a monitor's screen, so the pet can
/// stay below it. Derived from `visibleFrame`, whose top inset is the menu
/// bar; the Dock inset is ignored. Falls back to the classic menu bar height
/// when AppKit cannot identify the screen.
pub fn menu_bar_height(monitor: &winit::monitor::MonitorHandle) -> f64 {
    let Some(pointer) = monitor.ns_screen() else {
        return (MENU_BAR_FALLBACK * monitor.scale_factor()).round();
    };
    // SAFETY: winit returns the NSScreen backing this monitor; we only read
    // its geometry on the main thread inside the event loop.
    let screen = unsafe { &*pointer.cast::<NSScreen>() };
    let frame = screen.frame();
    let visible = screen.visibleFrame();
    let height = (frame.origin.y + frame.size.height) - (visible.origin.y + visible.size.height);
    (height.max(0.0) * screen.backingScaleFactor()).round()
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
    use std::cell::{Cell, RefCell};
    use std::path::{Path, PathBuf};

    use objc2::rc::Retained;
    use objc2::{AnyThread, MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{
        NSBox, NSBoxType, NSColor, NSControlSize, NSFont, NSImage, NSImageView, NSLineBreakMode,
        NSProgressIndicator, NSProgressIndicatorStyle, NSTextField, NSTitlePosition, NSView,
        NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState,
        NSVisualEffectView,
    };
    use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    use crate::chat::{
        ARROW_RESERVED, BUBBLE_HEIGHT, BUBBLE_WIDTH, BUTTON_SIZE, ChatLayout, status_symbol,
    };
    use crate::status::AgentStatus;

    const TITLE_INSET_TOP: f64 = 12.0;
    const TEXT_INSET_X: f64 = 16.0;
    const TEXT_LINE_HEIGHT: f64 = 16.0;
    const TEXT_LINE_GAP: f64 = 4.0;
    const CHEVRON_SIZE: f64 = 13.0;
    const ARROW_SYMBOL_SIZE: f64 = 12.0;
    const AGENT_ICON_SIZE: f64 = 14.0;
    const AGENT_ICON_GAP: f64 = 6.0;
    const SPINNER_SIZE: f64 = 16.0;

    /// Frame inputs from the last layout pass, kept so an icon appearing can
    /// reflow the title row without waiting for another window resize.
    #[derive(Clone, Copy)]
    struct Frames {
        layout: ChatLayout,
        height: f64,
        pager: bool,
    }

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
        agent_icon: Retained<NSImageView>,
        status_icon: Retained<NSImageView>,
        /// Native spinner shown instead of `status_icon` while working.
        status_spinner: Retained<NSProgressIndicator>,
        prev_arrow: Retained<NSImageView>,
        next_arrow: Retained<NSImageView>,
        button: Retained<NSVisualEffectView>,
        button_tint: Retained<NSBox>,
        chevron: Retained<NSImageView>,
        /// Visibilities are tracked separately so the bubble, the toggle and
        /// the pager can be hidden independently.
        bubble_visible: Cell<bool>,
        button_visible: Cell<bool>,
        pager_visible: Cell<bool>,
        icon_present: Cell<bool>,
        status_working: Cell<bool>,
        frames: Cell<Option<Frames>>,
        /// Requested icon path and tint, so a subtitle change does not reload
        /// the same SVG from disk.
        icon_key: RefCell<Option<(PathBuf, bool)>>,
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
            // `textColor` resolves to opaque black in Aqua and white in Dark
            // Aqua, so the text stays legible on the glass in both
            // appearances. Fade the subtitle via view alpha to keep the
            // dynamic color intact.
            let title = label(
                mtm,
                NSFont::boldSystemFontOfSize(13.0),
                NSColor::textColor(),
            );
            let subtitle = label(mtm, NSFont::systemFontOfSize(13.0), NSColor::textColor());
            subtitle.setAlphaValue(0.85);
            // Labels sit above the glass rather than inside it, so the
            // bubble's own opacity does not fade the text.
            container.addSubview(&bubble);
            container.addSubview(&title);
            container.addSubview(&subtitle);

            // The agent icon sits before the title text, above the glass and
            // fully opaque like the labels.
            let agent_icon = NSImageView::new(mtm);
            agent_icon.setContentTintColor(Some(&NSColor::textColor()));
            container.addSubview(&agent_icon);

            // The state glyph sits before the subtitle, in the same left column
            // as the agent icon. Working swaps the static glyph for a native
            // spinner; threaded animation keeps it off the main thread.
            let status_icon = NSImageView::new(mtm);
            container.addSubview(&status_icon);
            let status_spinner = NSProgressIndicator::new(mtm);
            status_spinner.setStyle(NSProgressIndicatorStyle::Spinning);
            status_spinner.setControlSize(NSControlSize::Small);
            status_spinner.setDisplayedWhenStopped(false);
            // SAFETY: the spinner lives on the main thread; this only selects
            // AppKit's internal animation thread.
            unsafe { status_spinner.setUsesThreadedAnimation(true) };
            container.addSubview(&status_spinner);

            // Agent pager arrows live inside the bubble's trailing end and
            // stay fully opaque like the labels.
            let prev_arrow = pager_arrow(mtm, "chevron.left");
            let next_arrow = pager_arrow(mtm, "chevron.right");
            container.addSubview(&prev_arrow);
            container.addSubview(&next_arrow);

            // The toggle button shares the bubble's glass: no tint fill, the
            // same border, and the same opacity. The chevron sits above the
            // glass like the labels so it stays legible at low opacity.
            let (button, button_tint) = glass_view(
                mtm,
                BUTTON_SIZE / 2.0,
                NSColor::clearColor(),
                NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.09),
            );
            button.setAlphaValue(opacity);
            let chevron = NSImageView::new(mtm);
            chevron.setContentTintColor(Some(&NSColor::textColor()));

            container.addSubview(&button);
            container.addSubview(&chevron);
            content.addSubview(&container);

            let overlay = Self {
                bubble,
                bubble_tint,
                title,
                subtitle,
                agent_icon,
                status_icon,
                status_spinner,
                prev_arrow,
                next_arrow,
                button,
                button_tint,
                chevron,
                bubble_visible: Cell::new(false),
                button_visible: Cell::new(false),
                pager_visible: Cell::new(false),
                icon_present: Cell::new(false),
                status_working: Cell::new(false),
                frames: Cell::new(None),
                icon_key: RefCell::new(None),
            };
            overlay.set_status(None);
            overlay.set_visible(open, true);
            Some(overlay)
        }

        /// Reflow in AppKit's bottom-left point coordinates from the top-left
        /// logical layout. `pager` reserves room for the agent arrows inside
        /// the bubble. Called on creation and whenever the window resizes or
        /// the pager appears/disappears.
        pub fn layout(&self, layout: &ChatLayout, height: f64, pager: bool) {
            self.frames.set(Some(Frames {
                layout: *layout,
                height,
                pager,
            }));
            self.pager_visible.set(pager);
            self.apply_frames();
            self.apply_visibility();
        }

        fn apply_frames(&self) {
            let Some(frames) = self.frames.get() else {
                return;
            };
            let layout = &frames.layout;
            let height = frames.height;
            let pager = frames.pager;
            let bubble = NSRect::new(
                NSPoint::new(layout.bubble.x, height - layout.bubble.y - layout.bubble.h),
                NSSize::new(layout.bubble.w, layout.bubble.h),
            );
            self.bubble.setFrame(bubble);
            self.bubble_tint.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(layout.bubble.w, layout.bubble.h),
            ));
            let text_right = if pager { ARROW_RESERVED } else { TEXT_INSET_X };
            let icon_extra = if self.icon_present.get() {
                AGENT_ICON_SIZE + AGENT_ICON_GAP
            } else {
                0.0
            };
            let text_left = TEXT_INSET_X + icon_extra;
            let text_width = BUBBLE_WIDTH - text_left - text_right;
            let title_y = bubble.origin.y + BUBBLE_HEIGHT - TITLE_INSET_TOP - TEXT_LINE_HEIGHT;
            self.title.setFrame(NSRect::new(
                NSPoint::new(bubble.origin.x + text_left, title_y),
                NSSize::new(text_width, TEXT_LINE_HEIGHT),
            ));
            // The subtitle keeps a glyph column even when the agent icon on
            // the title row is missing, so both text lines stay aligned.
            let subtitle_left = TEXT_INSET_X + AGENT_ICON_SIZE + AGENT_ICON_GAP;
            let subtitle_y = title_y - TEXT_LINE_GAP - TEXT_LINE_HEIGHT;
            self.subtitle.setFrame(NSRect::new(
                NSPoint::new(bubble.origin.x + subtitle_left, subtitle_y),
                NSSize::new(BUBBLE_WIDTH - subtitle_left - text_right, TEXT_LINE_HEIGHT),
            ));
            self.agent_icon.setFrame(NSRect::new(
                NSPoint::new(
                    bubble.origin.x + TEXT_INSET_X,
                    title_y + (TEXT_LINE_HEIGHT - AGENT_ICON_SIZE) / 2.0,
                ),
                NSSize::new(AGENT_ICON_SIZE, AGENT_ICON_SIZE),
            ));
            self.status_icon.setFrame(NSRect::new(
                NSPoint::new(
                    bubble.origin.x + TEXT_INSET_X,
                    subtitle_y + (TEXT_LINE_HEIGHT - AGENT_ICON_SIZE) / 2.0,
                ),
                NSSize::new(AGENT_ICON_SIZE, AGENT_ICON_SIZE),
            ));
            self.status_spinner.setFrame(NSRect::new(
                NSPoint::new(
                    bubble.origin.x + TEXT_INSET_X - (SPINNER_SIZE - AGENT_ICON_SIZE) / 2.0,
                    subtitle_y + (TEXT_LINE_HEIGHT - SPINNER_SIZE) / 2.0,
                ),
                NSSize::new(SPINNER_SIZE, SPINNER_SIZE),
            ));
            let button = NSRect::new(
                NSPoint::new(layout.button.x, height - layout.button.y - layout.button.h),
                NSSize::new(layout.button.w, layout.button.h),
            );
            self.button.setFrame(button);
            self.button_tint.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(layout.button.w, layout.button.h),
            ));
            self.chevron.setFrame(NSRect::new(
                NSPoint::new(
                    button.origin.x + (BUTTON_SIZE - CHEVRON_SIZE) / 2.0,
                    button.origin.y + (BUTTON_SIZE - CHEVRON_SIZE) / 2.0,
                ),
                NSSize::new(CHEVRON_SIZE, CHEVRON_SIZE),
            ));
            self.prev_arrow
                .setFrame(appkit_rect(layout.prev_arrow, height));
            self.next_arrow
                .setFrame(appkit_rect(layout.next_arrow, height));
        }

        pub fn set_text(&self, title: &str, subtitle: &str) {
            self.title.setStringValue(&NSString::from_str(title));
            self.subtitle.setStringValue(&NSString::from_str(subtitle));
        }

        /// Load the agent icon shown before the title. `None`, or a file
        /// AppKit cannot read, hides the icon and gives the title its full
        /// width back.
        pub fn set_icon(&self, icon: Option<(&Path, bool)>) {
            let key = icon.map(|(path, tint)| (path.to_path_buf(), tint));
            if *self.icon_key.borrow() == key {
                return;
            }
            *self.icon_key.borrow_mut() = key;
            let mut present = false;
            if let Some((path, tint)) = icon
                && let Some(path) = path.to_str()
                && let Some(image) =
                    NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(path))
            {
                image.setSize(NSSize::new(AGENT_ICON_SIZE, AGENT_ICON_SIZE));
                image.setTemplate(tint);
                self.agent_icon.setImage(Some(&image));
                present = true;
            }
            if !present {
                self.agent_icon.setImage(None);
            }
            self.icon_present.set(present);
            self.apply_frames();
            self.apply_visibility();
        }

        /// Set the state glyph on the subtitle row. Working switches to the
        /// native spinner; the other states keep semantic-colored static
        /// glyphs so the agent's own message stays the focus.
        pub fn set_status(&self, status: Option<AgentStatus>) {
            let working = status == Some(AgentStatus::Working);
            self.status_working.set(working);
            if !working {
                let color = match status {
                    Some(AgentStatus::Working) => NSColor::systemBlueColor(),
                    Some(AgentStatus::Blocked) => NSColor::systemOrangeColor(),
                    Some(AgentStatus::Done) => NSColor::systemGreenColor(),
                    Some(AgentStatus::Error) => NSColor::systemRedColor(),
                    Some(AgentStatus::Idle) | None => NSColor::secondaryLabelColor(),
                };
                self.status_icon.setContentTintColor(Some(&color));
                if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                    &NSString::from_str(status_symbol(status)),
                    None,
                ) {
                    image.setSize(NSSize::new(AGENT_ICON_SIZE, AGENT_ICON_SIZE));
                    self.status_icon.setImage(Some(&image));
                }
            }
            self.apply_visibility();
        }

        pub fn set_opacity(&self, opacity: f64) {
            self.bubble.setAlphaValue(opacity);
            self.button.setAlphaValue(opacity);
        }

        /// Hide the bubble and/or the button. The chevron follows the bubble:
        /// down when expanded, up when collapsed or auto-hidden. The pager
        /// arrows follow both the bubble and their own visibility.
        pub fn set_visible(&self, bubble: bool, button: bool) {
            self.bubble_visible.set(bubble);
            self.button_visible.set(button);
            self.apply_visibility();
        }

        fn apply_visibility(&self) {
            let bubble = self.bubble_visible.get();
            self.bubble.setHidden(!bubble);
            self.title.setHidden(!bubble);
            self.subtitle.setHidden(!bubble);
            self.agent_icon
                .setHidden(!(bubble && self.icon_present.get()));
            let spinning = bubble && self.status_working.get();
            self.status_icon.setHidden(!bubble || spinning);
            self.status_spinner.setHidden(!spinning);
            // SAFETY: nil is the documented sender for these actions; the
            // spinner is a main-thread view owned by this overlay.
            unsafe {
                if spinning {
                    self.status_spinner.startAnimation(None);
                } else {
                    self.status_spinner.stopAnimation(None);
                }
            }
            let pager = bubble && self.pager_visible.get();
            self.prev_arrow.setHidden(!pager);
            self.next_arrow.setHidden(!pager);
            let button = self.button_visible.get();
            self.button.setHidden(!button);
            self.chevron.setHidden(!button);
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

    /// Agent pager arrow: an opaque symbol above the bubble glass.
    fn pager_arrow(mtm: MainThreadMarker, symbol: &str) -> Retained<NSImageView> {
        let view = NSImageView::new(mtm);
        view.setContentTintColor(Some(&NSColor::textColor()));
        if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(symbol),
            None,
        ) {
            image.setSize(NSSize::new(ARROW_SYMBOL_SIZE, ARROW_SYMBOL_SIZE));
            view.setImage(Some(&image));
        }
        view
    }

    /// Frame for a top-left-origin layout rect in AppKit's bottom-left points.
    fn appkit_rect(rect: crate::chat::Rect, height: f64) -> NSRect {
        NSRect::new(
            NSPoint::new(rect.x, height - rect.y - rect.h),
            NSSize::new(rect.w, rect.h),
        )
    }
}

mod settings {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};
    use std::ptr::NonNull;
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::time::{Duration, Instant};

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationOptions, NSBackingStoreType, NSButton, NSButtonType,
        NSColor, NSControlStateValueOff, NSControlStateValueOn, NSEvent, NSEventMask,
        NSFloatingWindowLevel, NSFont, NSLineBreakMode, NSModalResponseOK, NSOpenPanel,
        NSPopUpButton, NSRunningApplication, NSSlider, NSTextField, NSView, NSWindow,
        NSWindowCollectionBehavior, NSWindowStyleMask,
    };
    use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL};

    use crate::config::{Config, MAX_SIZE, MIN_SIZE};
    use crate::platform::PanelAction;

    const WIDTH: f64 = 320.0;
    const HEIGHT: f64 = 392.0;
    const PAD: f64 = 20.0;
    const LABEL_WIDTH: f64 = 96.0;
    const CONTROL_X: f64 = 116.0;
    const CONTROL_WIDTH: f64 = WIDTH - CONTROL_X - PAD;
    const ROW_HEIGHT: f64 = 24.0;
    const PET_TOP: f64 = 20.0;
    const DIR_TOP: f64 = 58.0;
    const DIR_BUTTON_TOP: f64 = 84.0;
    const SIZE_TOP: f64 = 122.0;
    const OPACITY_TOP: f64 = 160.0;
    const TOGGLE_TOP: f64 = 204.0;
    const TOGGLE_STEP: f64 = 28.0;
    const FOOTER_TOP: f64 = 358.0;
    /// The tray/menu click that opened the panel must finish before outside
    /// clicks can close it again.
    const MONITOR_ARM_DELAY: Duration = Duration::from_millis(300);

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
        dir_label: Retained<NSTextField>,
        reset_dir: Retained<NSButton>,
        size: Retained<NSSlider>,
        opacity: Retained<NSSlider>,
        notifications: Retained<NSButton>,
        notification_sound: Retained<NSButton>,
        gaze: Retained<NSButton>,
        click_through: Retained<NSButton>,
        chat_open: Retained<NSButton>,
        /// NSControl does not retain its target, so the panel keeps it alive.
        path_chooser: Retained<PathChooser>,
        dir_events: Receiver<PanelAction>,
        monitor: Option<OutsideClickMonitor>,
        monitor_arm_at: Option<Instant>,
        last: UiState,
    }

    impl SettingsPanel {
        pub fn new(
            pets: &[String],
            selected: usize,
            config: &Config,
            pets_dir: &Path,
            custom_dir: bool,
        ) -> Option<Self> {
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
            // Settings must be visible even when another app is frontmost or
            // a full-screen Space is active; it closes on the next outside
            // click anyway.
            window.setLevel(NSFloatingWindowLevel);
            window.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::FullScreenAuxiliary,
            );
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
            place(&pet, CONTROL_X, PET_TOP, CONTROL_WIDTH, ROW_HEIGHT);
            content.addSubview(&pet);

            let (dir_sender, dir_events) = channel();
            let path_chooser = PathChooser::new(mtm, pets_dir, dir_sender);
            let dir_title = label(mtm, "宠物目录", 13.0);
            place(&dir_title, PAD, DIR_TOP + 4.0, LABEL_WIDTH, 16.0);
            content.addSubview(&dir_title);
            let dir_label = label(mtm, "", 11.0);
            dir_label.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
            dir_label.setTextColor(Some(&NSColor::secondaryLabelColor()));
            place(&dir_label, CONTROL_X, DIR_TOP + 4.0, CONTROL_WIDTH, 16.0);
            content.addSubview(&dir_label);

            let choose_dir = push_button(mtm, "选择目录…");
            place(&choose_dir, CONTROL_X, DIR_BUTTON_TOP, 100.0, ROW_HEIGHT);
            content.addSubview(&choose_dir);
            let reset_dir = push_button(mtm, "恢复默认");
            place(
                &reset_dir,
                CONTROL_X + 106.0,
                DIR_BUTTON_TOP,
                CONTROL_WIDTH - 106.0,
                ROW_HEIGHT,
            );
            content.addSubview(&reset_dir);
            // SAFETY: both selectors exist on `PathChooser`, and the panel
            // retains the chooser for as long as the buttons can fire.
            unsafe {
                let _: () = msg_send![&choose_dir, setTarget: &*path_chooser];
                let _: () = msg_send![&choose_dir, setAction: sel!(choosePetsDirectory:)];
                let _: () = msg_send![&reset_dir, setTarget: &*path_chooser];
                let _: () = msg_send![&reset_dir, setAction: sel!(resetPetsDirectory:)];
            }

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
                dir_label,
                reset_dir,
                size,
                opacity,
                notifications,
                notification_sound,
                gaze,
                click_through,
                chat_open,
                path_chooser,
                dir_events,
                monitor: None,
                monitor_arm_at: None,
                last: UiState::default(),
            };
            panel.sync_pets(pets, selected, pets_dir, custom_dir);
            panel.sync(config, selected);
            Some(panel)
        }

        pub fn show(&mut self) {
            let Some(mtm) = MainThreadMarker::new() else {
                return;
            };
            // Accessory apps are not activated by a menu-bar click; without
            // this the window can stay behind the frontmost app's windows.
            NSApplication::sharedApplication(mtm).activate();
            NSRunningApplication::currentApplication()
                .activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
            self.window.makeKeyAndOrderFront(None);
            self.window.orderFrontRegardless();
            if self.monitor.is_none() && self.monitor_arm_at.is_none() {
                self.monitor_arm_at = Some(Instant::now() + MONITOR_ARM_DELAY);
            }
        }

        /// Changes the user made since the last poll. Controls without
        /// target/action still track their own state, so reading them is
        /// enough to support live sliders and toggles.
        pub fn poll(&mut self) -> Vec<PanelAction> {
            // Closing via the title bar or an outside click must also release
            // the event monitors; the next tick does that.
            if !self.window.isVisible() {
                self.monitor = None;
                self.monitor_arm_at = None;
                return Vec::new();
            } else if self.monitor.is_none()
                && self
                    .monitor_arm_at
                    .is_some_and(|arm_at| Instant::now() >= arm_at)
            {
                self.monitor_arm_at = None;
                if let Some(mtm) = MainThreadMarker::new() {
                    self.monitor = Some(OutsideClickMonitor::install(&self.window, mtm));
                }
            }
            let mut actions = Vec::new();
            while let Ok(action) = self.dir_events.try_recv() {
                actions.push(action);
            }
            let current = self.read();
            actions.extend(diff(&self.last, &current));
            self.last = current;
            actions
        }

        /// Refresh the pet list and the effective root directory. Called when
        /// the panel opens and after the directory changes.
        pub fn sync_pets(
            &mut self,
            pets: &[String],
            selected: usize,
            pets_dir: &Path,
            custom_dir: bool,
        ) {
            self.pet.removeAllItems();
            for name in pets {
                self.pet.addItemWithTitle(&NSString::from_str(name));
            }
            if !pets.is_empty() {
                self.pet
                    .selectItemAtIndex(selected.min(pets.len() - 1) as isize);
            }
            let text = NSString::from_str(&pets_dir.to_string_lossy());
            self.dir_label.setStringValue(&text);
            self.dir_label.setToolTip(Some(&text));
            self.reset_dir.setEnabled(custom_dir);
            self.path_chooser.set_dir(pets_dir);
            self.last.pet = self.pet.indexOfSelectedItem();
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

    struct PathChooserIvars {
        dir: RefCell<PathBuf>,
        sender: Sender<PanelAction>,
    }

    // The directory buttons need a real action callback; sliders and toggles
    // stay callback-free and are polled above.
    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and the class only
        // holds plain Rust data.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[ivars = PathChooserIvars]
        struct PathChooser;

        unsafe impl NSObjectProtocol for PathChooser {}

        impl PathChooser {
            #[unsafe(method(choosePetsDirectory:))]
            fn choose_pets_directory(&self, _sender: &NSButton) {
                let dir = self.ivars().dir.borrow().clone();
                if let Some(path) = choose_directory(&dir) {
                    let _ = self.ivars().sender.send(PanelAction::SetPetsDir(path));
                }
            }

            #[unsafe(method(resetPetsDirectory:))]
            fn reset_pets_directory(&self, _sender: &NSButton) {
                let _ = self.ivars().sender.send(PanelAction::ResetPetsDir);
            }
        }
    );

    impl PathChooser {
        fn new(mtm: MainThreadMarker, dir: &Path, sender: Sender<PanelAction>) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(PathChooserIvars {
                dir: RefCell::new(dir.to_path_buf()),
                sender,
            });
            // SAFETY: NSObject's init has this signature.
            unsafe { msg_send![super(this), init] }
        }

        fn set_dir(&self, dir: &Path) {
            *self.ivars().dir.borrow_mut() = dir.to_path_buf();
        }
    }

    fn choose_directory(initial: &Path) -> Option<PathBuf> {
        let mtm = MainThreadMarker::new()?;
        let panel = NSOpenPanel::openPanel(mtm);
        panel.setCanChooseFiles(false);
        panel.setCanChooseDirectories(true);
        panel.setAllowsMultipleSelection(false);
        let url = NSURL::fileURLWithPath(&NSString::from_str(&initial.to_string_lossy()));
        panel.setDirectoryURL(Some(&url));
        if panel.runModal() != NSModalResponseOK {
            return None;
        }
        let path = panel.URL()?.path()?;
        Some(PathBuf::from(path.to_string()))
    }

    /// Hides the settings window when a click lands outside it, mirroring the
    /// About dialog. Modal panels (the directory picker, alerts) are exempt.
    struct OutsideClickMonitor {
        local: Option<Retained<AnyObject>>,
        global: Option<Retained<AnyObject>>,
    }

    impl OutsideClickMonitor {
        fn install(window: &Retained<NSWindow>, mtm: MainThreadMarker) -> Self {
            let mask = NSEventMask::LeftMouseDown
                | NSEventMask::RightMouseDown
                | NSEventMask::OtherMouseDown;

            let local_window = window.clone();
            let local_handler: RcBlock<dyn Fn(NonNull<NSEvent>) -> *mut NSEvent> =
                RcBlock::new(move |event: NonNull<NSEvent>| {
                    // SAFETY: AppKit passes a live event for the handler's duration.
                    let event_ref = unsafe { event.as_ref() };
                    let inside = event_ref.window(mtm).is_some_and(|event_window| {
                        std::ptr::eq(
                            Retained::as_ptr(&event_window),
                            Retained::as_ptr(&local_window),
                        )
                    });
                    let modal = NSApplication::sharedApplication(mtm)
                        .modalWindow()
                        .is_some();
                    if !inside && !modal {
                        local_window.orderOut(None);
                    }
                    event.as_ptr()
                });
            // SAFETY: the handler returns the event unchanged.
            let local = unsafe {
                NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &local_handler)
            };

            let global_window = window.clone();
            let global_handler: RcBlock<dyn Fn(NonNull<NSEvent>)> =
                RcBlock::new(move |_event: NonNull<NSEvent>| {
                    // Global events carry no window. When the app is inactive,
                    // clicking the floating panel also arrives here; keep it
                    // open when the pointer is physically over the panel.
                    let frame = global_window.frame();
                    let p = NSEvent::mouseLocation();
                    let inside = p.x >= frame.origin.x
                        && p.x < frame.origin.x + frame.size.width
                        && p.y >= frame.origin.y
                        && p.y < frame.origin.y + frame.size.height;
                    if !inside {
                        global_window.orderOut(None);
                    }
                });
            let global =
                NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &global_handler);

            Self { local, global }
        }
    }

    impl Drop for OutsideClickMonitor {
        fn drop(&mut self) {
            for monitor in [self.local.take(), self.global.take()]
                .into_iter()
                .flatten()
            {
                // SAFETY: each token came from the matching add call above.
                unsafe { NSEvent::removeMonitor(&monitor) };
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

    fn push_button(mtm: MainThreadMarker, title: &str) -> Retained<NSButton> {
        let button = NSButton::new(mtm);
        button.setButtonType(NSButtonType::MomentaryPushIn);
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
