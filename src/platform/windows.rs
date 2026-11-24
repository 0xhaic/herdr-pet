//! Windows platform glue.
//!
//! M0 covers what the pet needs to be visible and usable: a focus-free
//! window that can become click-through, global pointer state, and the
//! monitor work area. The Direct2D chat overlay and the Win32 settings panel
//! belong to later milestones; see `docs/windows-port.md`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    DeleteObject, ExtCreateRegion, GetMonitorInfoW, HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromPoint, RDH_RECTANGLES, RGNDATA, RGNDATAHEADER, SetWindowRgn,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetCursorPos, GetWindowLongPtrW, STYLESTRUCT, SWP_FRAMECHANGED, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, WM_STYLECHANGING,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};
use winit::monitor::MonitorHandle;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use crate::chat::{ChatLayout, Rect};
use crate::config::Config;
use crate::pet::{Atlas, CELL_H, CELL_W, Frame};
use crate::platform::PanelAction;
use crate::status::AgentStatus;

/// Whole-window click-through setting, mirrored onto `WS_EX_TRANSPARENT`
/// whenever winit rewrites the window styles.
static CLICK_THROUGH: AtomicBool = AtomicBool::new(false);
const SUBCLASS_ID: usize = 1;

/// Turn the window into a focus-free tool window and install the hit-test
/// subclass. `with_skip_taskbar` only removes the taskbar button, so
/// `WS_EX_TOOLWINDOW` still has to hide the window from Alt+Tab.
pub fn prepare_window(window: &Window) {
    let Some(hwnd) = hwnd(window) else {
        return;
    };
    let extra = (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0) as isize;
    // SAFETY: the HWND belongs to a live winit window on the main thread; the
    // subclass proc only reads a static flag and the style-change message.
    unsafe {
        if !SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, 0).as_bool() {
            eprintln!("herdr-pet: cannot install the window subclass; window styles may be lost");
        }
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | extra);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}

/// Per-pixel hit testing is owned by the window region on Windows, so the
/// UI's per-tick call is a no-op; the whole-window setting goes through
/// `set_click_through`.
pub fn set_cursor_hittest(_window: &Window, _hittest: bool) {}

/// Apply the whole-window click-through setting.
pub fn set_click_through(window: &Window, click_through: bool) {
    CLICK_THROUGH.store(click_through, Ordering::Relaxed);
    if let Some(hwnd) = hwnd(window) {
        set_transparent_style(hwnd, click_through);
    }
}

/// Add or remove `WS_EX_TRANSPARENT` without touching the window's other
/// styles. A style change is immediately visible to hit testing, so no
/// frame-wide `SetWindowPos` is needed.
fn set_transparent_style(hwnd: HWND, transparent: bool) {
    let bit = WS_EX_TRANSPARENT.0 as isize;
    // SAFETY: the HWND belongs to a live winit window on the main thread.
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    let updated = if transparent {
        style | bit
    } else {
        style & !bit
    };
    if updated != style {
        // SAFETY: same HWND; the new style keeps every other bit intact.
        unsafe {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, updated);
        }
    }
}

/// Rebuild the window's hit region from the visible pet frame. A Direct3D /
/// DirectComposition window is not hit-tested by its alpha, and
/// `WS_EX_LAYERED` would give up the GPU presentation, so an OS window
/// region is the cross-process way to let transparent pixels fall through.
/// Chromium uses the same approach for its Direct3D transparent windows.
/// The M3 bubble must be added to this region once it renders.
pub fn update_pet_region(
    window: &Window,
    atlas: &Atlas,
    frame: Frame,
    pet: Rect,
    scale: f64,
    alpha_threshold: u8,
) {
    let Some(hwnd) = hwnd(window) else {
        return;
    };
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    let spans = atlas.alpha_spans(frame.row, frame.col, alpha_threshold);
    if spans.is_empty() {
        // An empty region would hide the window; keep the previous shape.
        return;
    }
    let scale_x = pet.w * scale / f64::from(CELL_W);
    let scale_y = pet.h * scale / f64::from(CELL_H);
    let origin_x = pet.x * scale;
    let origin_y = pet.y * scale;
    let rects: Vec<RECT> = spans
        .iter()
        .map(|&(y, x0, x1)| RECT {
            left: (origin_x + f64::from(x0) * scale_x).floor() as i32,
            top: (origin_y + f64::from(y) * scale_y).floor() as i32,
            right: (origin_x + f64::from(x1) * scale_x).ceil() as i32,
            bottom: (origin_y + f64::from(y + 1) * scale_y).ceil() as i32,
        })
        .collect();
    apply_window_region(hwnd, &rects);
}

/// Replace the window region with `rects`, in window coordinates. The region
/// clips painting and drives hit testing; `ExtCreateRegion` takes the same
/// RGNDATA layout Chromium builds for this purpose.
fn apply_window_region(hwnd: HWND, rects: &[RECT]) {
    if rects.is_empty() {
        return;
    }
    let header_size = std::mem::size_of::<RGNDATAHEADER>();
    let rect_size = std::mem::size_of::<RECT>();
    let rect_bytes = std::mem::size_of_val(rects);
    let size = header_size + rect_bytes;
    // A RECT buffer keeps RGNDATA's alignment and holds the header followed
    // by every rectangle contiguously.
    let mut storage: Vec<RECT> = Vec::with_capacity(size.div_ceil(rect_size));
    let buffer = storage.as_mut_ptr().cast::<u8>();
    // SAFETY: `storage` has room for the header and all rectangles, and no
    // uninitialized memory is read.
    unsafe {
        let data = buffer.cast::<RGNDATA>();
        (*data).rdh = RGNDATAHEADER {
            dwSize: header_size as u32,
            iType: RDH_RECTANGLES,
            nCount: rects.len() as u32,
            nRgnSize: rect_bytes as u32,
            rcBound: region_bounds(rects),
        };
        std::ptr::copy_nonoverlapping(
            rects.as_ptr().cast::<u8>(),
            buffer.add(header_size),
            rect_bytes,
        );
        let region = ExtCreateRegion(None, size as u32, data);
        if region.0.is_null() {
            return;
        }
        let _ = SetWindowRgn(hwnd, Some(region), true);
        let _ = DeleteObject(HGDIOBJ(region.0));
    }
}

fn region_bounds(rects: &[RECT]) -> RECT {
    let mut bound = rects[0];
    for rect in &rects[1..] {
        bound.left = bound.left.min(rect.left);
        bound.top = bound.top.min(rect.top);
        bound.right = bound.right.max(rect.right);
        bound.bottom = bound.bottom.max(rect.bottom);
    }
    bound
}

/// Keep the focus-free styles when winit rewrites the whole `WS_EX` bitfield
/// for its own flag changes (visibility, topmost, ...), and mirror the
/// click-through setting onto `WS_EX_TRANSPARENT`.
unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match message {
        WM_STYLECHANGING if wparam.0 as i32 == GWL_EXSTYLE.0 => {
            // SAFETY: the system passes a valid STYLESTRUCT for this message.
            unsafe {
                let style = &mut *(lparam.0 as *mut STYLESTRUCT);
                style.styleNew |= WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0;
                if CLICK_THROUGH.load(Ordering::Relaxed) {
                    style.styleNew |= WS_EX_TRANSPARENT.0;
                } else {
                    style.styleNew &= !WS_EX_TRANSPARENT.0;
                }
            }
            // SAFETY: forward the (possibly adjusted) style change to winit.
            unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
        }
        // SAFETY: every other message belongs to winit's own window proc.
        _ => unsafe { DefSubclassProc(hwnd, message, wparam, lparam) },
    }
}

fn hwnd(window: &Window) -> Option<HWND> {
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return None;
    };
    Some(HWND(handle.hwnd.get() as *mut std::ffi::c_void))
}

/// Global pointer position in physical pixels, top-left origin. Unlike macOS
/// no primary-screen conversion is needed; `GetCursorPos` already returns
/// virtual-screen physical coordinates.
pub fn mouse_position(_screen_height: f64, _scale: f64) -> Option<(f64, f64)> {
    let mut point = POINT::default();
    // SAFETY: `point` is a valid output buffer on this thread.
    unsafe { GetCursorPos(&mut point) }.ok()?;
    Some((f64::from(point.x), f64::from(point.y)))
}

/// Left mouse button state for drag release detection.
pub fn left_button_down() -> bool {
    // SAFETY: reading a global key state has no preconditions.
    (unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) } & i16::MIN) != 0
}

/// The pet is clamped by `work_area` instead, so there is no menu bar inset
/// to report.
pub fn menu_bar_height(_monitor: &MonitorHandle) -> f64 {
    0.0
}

/// Work area of the monitor backing `monitor`, in physical pixels. Windows
/// excludes the taskbar there whether it sits at the bottom, top, left or
/// right, and `rcWork` also accounts for the primary monitor's mixed DPI.
pub fn work_area(monitor: &MonitorHandle) -> Option<Rect> {
    let origin = monitor.position();
    let size = monitor.size();
    // `MonitorFromPoint` needs a point inside the monitor; its centre is the
    // most robust choice for adjacent or mirrored layouts.
    let center = POINT {
        x: origin.x + (size.width / 2) as i32,
        y: origin.y + (size.height / 2) as i32,
    };
    // SAFETY: both calls only read monitor geometry.
    let handle = unsafe { MonitorFromPoint(center, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(handle, &mut info) }.as_bool() {
        return None;
    }
    let work = info.rcWork;
    Some(Rect {
        x: f64::from(work.left),
        y: f64::from(work.top),
        w: f64::from(work.right - work.left),
        h: f64::from(work.bottom - work.top),
    })
}

/// Windows has no Spaces equivalent; the window is simply always on top.
pub fn float_on_all_spaces(_window: &Window) -> bool {
    true
}

/// There is no menu bar constraint to lift on Windows.
pub fn allow_window_above_menu_bar(_window: &Window) -> bool {
    true
}

/// Herdr frontmost detection arrives with the Herdr.app milestone (M2).
pub fn herdr_app_frontmost() -> bool {
    false
}

/// Herdr executable discovery arrives with the Herdr.app milestone (M2).
pub fn herdr_app_path() -> Option<PathBuf> {
    None
}

/// WinRT Toast support arrives with the notification milestone (M2).
pub fn send_notification(_message: &str) -> bool {
    false
}

/// The TaskDialog arrives with the settings milestone (M3).
pub fn show_about() {}

/// Show `menu` as a context menu for the window. Must run on the main thread,
/// which winit event callbacks guarantee; `None` places it at the pointer.
pub fn show_context_menu(window: &Window, menu: &muda::Menu) {
    use muda::ContextMenu;

    let Some(hwnd) = hwnd(window) else {
        return;
    };
    // SAFETY: the HWND belongs to a live winit window, and the popup runs on
    // the main thread inside an event callback.
    unsafe { menu.show_context_menu_for_hwnd(hwnd.0 as isize, None) };
}

// The Direct2D bubble bitmap (M3) replaces these stubs.
pub struct ChatOverlay;

impl ChatOverlay {
    pub fn new(_window: &Window, _open: bool, _opacity: f64) -> Option<Self> {
        None
    }

    pub fn layout(&self, _layout: &ChatLayout, _height: f64, _pager: bool) {}

    pub fn set_text(&self, _title: &str, _subtitle: &str) {}

    pub fn set_icon(&self, _icon: Option<(&Path, bool)>) {}

    pub fn set_status(&self, _status: Option<AgentStatus>) {}

    pub fn set_visible(&self, _bubble: bool, _button: bool) {}

    pub fn set_opacity(&self, _opacity: f64) {}
}

// The native Win32 settings panel (M3) replaces this stub.
pub struct SettingsPanel;

impl SettingsPanel {
    pub fn new(
        _pets: &[String],
        _selected: usize,
        _config: &Config,
        _pets_dir: &Path,
        _custom_dir: bool,
    ) -> Option<Self> {
        None
    }

    pub fn show(&self) {}

    pub fn poll(&mut self) -> Vec<PanelAction> {
        Vec::new()
    }

    pub fn sync(&mut self, _config: &Config, _selected: usize) {}

    pub fn sync_pets(
        &mut self,
        _pets: &[String],
        _selected: usize,
        _pets_dir: &Path,
        _custom_dir: bool,
    ) {
    }
}
