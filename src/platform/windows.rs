//! Windows platform glue.
//!
//! M0 covers what the pet needs to be visible and usable: a focus-free
//! window that can become click-through, global pointer state, and the
//! monitor work area. The Direct2D chat overlay and the Win32 settings panel
//! belong to later milestones; see `docs/windows-port.md`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
use windows::Win32::Foundation::{
    COLORREF, CloseHandle, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    DeleteObject, ExtCreateRegion, GetMonitorInfoW, HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromPoint, RDH_RECTANGLES, RGNDATA, RGNDATAHEADER, SetWindowRgn,
};
use windows::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC, SND_NODEFAULT};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegSetValueExW,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TD_INFORMATION_ICON, TDCBF_OK_BUTTON,
    TDF_ALLOW_DIALOG_CANCELLATION, TaskDialogIndirect,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetCursorPos, GetForegroundWindow, GetWindowLongPtrW, GetWindowThreadProcessId,
    LWA_ALPHA, STYLESTRUCT, SW_SHOWNORMAL, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_NOZORDER, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos,
    WM_STYLECHANGED, WM_STYLECHANGING, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TRANSPARENT,
};
use windows::core::{HSTRING, PCWSTR, PWSTR, w};
use winit::monitor::MonitorHandle;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use crate::chat::Rect;
use crate::pet::{Atlas, CELL_H, CELL_W, Frame};

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
///
/// A non-layered `WS_EX_TRANSPARENT` window still receives mouse input, so
/// the window also becomes layered: the OS then ignores the window shape for
/// hit testing and clicks reach whatever sits below. A full-alpha layered
/// surface leaves the DirectComposition content unchanged.
pub fn set_click_through(window: &Window, click_through: bool) {
    CLICK_THROUGH.store(click_through, Ordering::Relaxed);
    if let Some(hwnd) = hwnd(window) {
        apply_click_through(hwnd);
    }
}

/// Mirror the click-through setting onto the window. The subclass calls this
/// again after every style rewrite so winit cannot drop the bits.
fn apply_click_through(hwnd: HWND) {
    let click_through = CLICK_THROUGH.load(Ordering::Relaxed);
    let flags = (WS_EX_TRANSPARENT.0 | WS_EX_LAYERED.0) as isize;
    // SAFETY: the HWND belongs to a live winit window on the main thread.
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let updated = if click_through {
            style | flags
        } else {
            style & !flags
        };
        let changed = updated != style;
        if changed {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, updated);
        }
        if click_through && changed {
            // A layered window without attributes is not displayed; full
            // alpha keeps every pixel the composition already draws.
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
        }
        if changed {
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
}

/// Rebuild the window's hit region from the visible pet frame plus the drawn
/// overlay rectangles. A Direct3D / DirectComposition window is not hit-tested
/// by its alpha, and `WS_EX_LAYERED` would give up the GPU presentation, so an
/// OS window region is the cross-process way to let transparent pixels fall
/// through. Chromium uses the same approach for its transparent windows.
pub fn update_pet_region(
    window: &Window,
    atlas: &Atlas,
    frame: Frame,
    pet: Rect,
    extras: &[Rect],
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
    if spans.is_empty() && extras.is_empty() {
        // An empty region would hide the window; keep the previous shape.
        return;
    }
    let scale_x = pet.w * scale / f64::from(CELL_W);
    let scale_y = pet.h * scale / f64::from(CELL_H);
    let origin_x = pet.x * scale;
    let origin_y = pet.y * scale;
    let mut rects: Vec<RECT> = spans
        .iter()
        .map(|&(y, x0, x1)| RECT {
            left: (origin_x + f64::from(x0) * scale_x).floor() as i32,
            top: (origin_y + f64::from(y) * scale_y).floor() as i32,
            right: (origin_x + f64::from(x1) * scale_x).ceil() as i32,
            bottom: (origin_y + f64::from(y + 1) * scale_y).ceil() as i32,
        })
        .collect();
    for extra in extras {
        rects.push(RECT {
            left: (extra.x * scale).floor() as i32,
            top: (extra.y * scale).floor() as i32,
            right: ((extra.x + extra.w) * scale).ceil() as i32,
            bottom: ((extra.y + extra.h) * scale).ceil() as i32,
        });
    }
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
/// click-through setting onto `WS_EX_TRANSPARENT | WS_EX_LAYERED`.
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
                    style.styleNew |= WS_EX_TRANSPARENT.0 | WS_EX_LAYERED.0;
                } else {
                    style.styleNew &= !(WS_EX_TRANSPARENT.0 | WS_EX_LAYERED.0);
                }
            }
            // SAFETY: forward the (possibly adjusted) style change to winit.
            unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
        }
        WM_STYLECHANGED if wparam.0 as i32 == GWL_EXSTYLE.0 => {
            // Re-adding `WS_EX_LAYERED` resets its attributes; full alpha
            // keeps the DirectComposition content visible.
            if CLICK_THROUGH.load(Ordering::Relaxed) {
                // SAFETY: the HWND belongs to a live winit window on the main
                // thread and the style change above kept it layered.
                unsafe {
                    let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
                }
            }
            // SAFETY: forward the style change to winit.
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

/// Toast identity registered under `HKCU\Software\Classes\AppUserModelId`.
/// Without it Windows silently drops toasts from a bare executable.
const APP_ID: &str = "dev.herdr.pet";

/// Register the notification identity before the first reminder, so the
/// shell never drops one while it learns about the app.
pub fn prepare_notifications() {
    ensure_app_id();
}

/// Show a Windows toast with the given text.
pub fn send_notification(message: &str) -> bool {
    ensure_app_id();
    match show_toast(message) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("herdr-pet: cannot show a notification: {error}");
            false
        }
    }
}

fn show_toast(message: &str) -> windows::core::Result<()> {
    // SAFETY: a WinRT call needs an initialized apartment on this thread;
    // RPC_E_CHANGED_MODE only means another mode already initialized it.
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    let document = XmlDocument::new()?;
    document.LoadXml(&HSTRING::from(toast_xml(message)))?;
    let toast = ToastNotification::CreateToastNotification(&document)?;
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))?;
    notifier.Show(&toast)
}

fn ensure_app_id() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        if let Err(error) = register_app_id() {
            eprintln!("herdr-pet: cannot register the notification app id: {error}");
        }
    });
}

fn register_app_id() -> windows::core::Result<()> {
    const DISPLAY_NAME: &str = "herdr-pet";
    let subkey = HSTRING::from(format!("Software\\Classes\\AppUserModelId\\{APP_ID}"));
    let name = HSTRING::from("DisplayName");
    // REG_SZ is UTF-16 with a terminating NUL.
    let mut bytes = Vec::with_capacity((DISPLAY_NAME.encode_utf16().count() + 1) * 2);
    for unit in DISPLAY_NAME.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes.extend_from_slice(&0u16.to_le_bytes());
    let mut key = HKEY(std::ptr::null_mut());
    // SAFETY: the key is created under HKCU, written once, and closed.
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &subkey,
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
        .ok()?;
        let status = RegSetValueExW(key, &name, None, REG_SZ, Some(&bytes));
        let _ = RegCloseKey(key);
        status.ok()?;
    }
    Ok(())
}

/// Play the system sound associated with a blocked or finished agent.
pub fn play_notification_sound(blocked: bool) {
    let alias = if blocked {
        windows::core::w!("SystemHand")
    } else {
        windows::core::w!("SystemAsterisk")
    };
    // SAFETY: SND_ASYNC hands the system alias to the sound player and
    // returns immediately; no resources are owned here.
    unsafe {
        let _ = PlaySoundW(alias, None, SND_ALIAS | SND_ASYNC | SND_NODEFAULT);
    }
}

fn toast_xml(message: &str) -> String {
    format!(
        concat!(
            "<toast><visual><binding template=\"ToastGeneric\">",
            "<text>herdr-pet</text><text>{}</text>",
            "</binding></visual><audio silent=\"true\"/></toast>"
        ),
        xml_escape(message)
    )
}

/// Escape text going into the toast XML; terminal titles may contain `&`,
/// `<` or quotes.
fn xml_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Herdr executable found once per process: PATH first, then the default
/// install under `%LOCALAPPDATA%\Programs`.
pub fn herdr_app_path() -> Option<PathBuf> {
    cached_app_path().map(Path::to_path_buf)
}

fn cached_app_path() -> Option<&'static Path> {
    static APP_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    APP_PATH.get_or_init(discover_app_path).as_deref()
}

fn discover_app_path() -> Option<PathBuf> {
    const NAMES: [&str; 2] = ["herdr-app.exe", "Herdr.exe"];
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in NAMES {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    let programs = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Programs");
    for folder in ["herdr-app", "Herdr"] {
        for name in NAMES {
            let candidate = programs.join(folder).join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Whether the foreground window belongs to Herdr. The process image is
/// compared with the discovered executable and falls back to its file name,
/// so a custom install still hides the chat UI.
pub fn herdr_app_frontmost() -> bool {
    // SAFETY: these calls only read the foreground window and process state.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return false;
        }
        let mut pid = 0u32;
        if GetWindowThreadProcessId(hwnd, Some(&mut pid)) == 0 || pid == 0 {
            return false;
        }
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;
        let queried = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(process);
        if queried.is_err() {
            return false;
        }
        let image = String::from_utf16_lossy(&buffer[..size as usize]);
        is_herdr_app_image(Path::new(&image))
    }
}

fn is_herdr_app_image(image: &Path) -> bool {
    if let Some(app) = cached_app_path()
        && image
            .to_string_lossy()
            .eq_ignore_ascii_case(&app.to_string_lossy())
    {
        return true;
    }
    image
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| {
            name.eq_ignore_ascii_case("herdr-app.exe") || name.eq_ignore_ascii_case("Herdr.exe")
        })
}

/// About dialog. The application manifest activates comctl32 v6, so the
/// native task dialog is available; it is owned by the pet window and offers
/// a button that opens the repository.
pub fn show_about(window: Option<&Window>) {
    const REPOSITORY: &str = "https://github.com/0xhaic/herdr-pet";
    const OPEN_REPOSITORY: i32 = 1001;
    let body = format!(
        "由 herdr-core 中 agent 状态驱动的桌面宠物。\n\n版本 {}\n{REPOSITORY}",
        env!("CARGO_PKG_VERSION")
    );
    let title: Vec<u16> = "关于 herdr-pet".encode_utf16().chain(Some(0)).collect();
    let instruction: Vec<u16> = "herdr-pet".encode_utf16().chain(Some(0)).collect();
    let content: Vec<u16> = body.encode_utf16().chain(Some(0)).collect();
    let open_label: Vec<u16> = "打开仓库".encode_utf16().chain(Some(0)).collect();
    let buttons = [TASKDIALOG_BUTTON {
        nButtonID: OPEN_REPOSITORY,
        pszButtonText: PCWSTR(open_label.as_ptr()),
    }];
    let config = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: window.and_then(hwnd).unwrap_or_default(),
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION,
        dwCommonButtons: TDCBF_OK_BUTTON,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        Anonymous1: TASKDIALOGCONFIG_0 {
            pszMainIcon: TD_INFORMATION_ICON,
        },
        pszMainInstruction: PCWSTR(instruction.as_ptr()),
        pszContent: PCWSTR(content.as_ptr()),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        // Return opens nothing; the repository is an explicit choice.
        nDefaultButton: 1,
        ..Default::default()
    };
    let mut pressed = 0i32;
    // SAFETY: every string and the button array outlive the call; the dialog
    // runs modally on the main thread with the pet window as its owner.
    match unsafe { TaskDialogIndirect(&config, Some(&mut pressed), None, None) } {
        Ok(()) if pressed == OPEN_REPOSITORY => open_repository(REPOSITORY),
        Ok(()) => {}
        Err(error) => eprintln!("herdr-pet: cannot show the About dialog: {error}"),
    }
}

fn open_repository(repository: &str) {
    let url: Vec<u16> = repository.encode_utf16().chain(Some(0)).collect();
    // SAFETY: the URL is a live NUL-terminated wide string for the call.
    unsafe {
        let _ = ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(url.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

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

mod overlay;
mod settings;

pub use overlay::ChatOverlay;
pub use settings::SettingsPanel;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_toast_text() {
        let xml = toast_xml("a&b<c>d\"e'f");
        assert!(xml.contains("a&amp;b&lt;c&gt;d&quot;e&apos;f"));
        assert!(xml.contains("<audio silent=\"true\"/>"));
    }

    #[test]
    fn recognizes_herdr_executable_names() {
        assert!(is_herdr_app_image(Path::new(
            r"C:\Program Files\Herdr\Herdr.exe"
        )));
        assert!(is_herdr_app_image(Path::new(
            r"C:\Users\me\AppData\Local\Programs\herdr-app\herdr-app.exe"
        )));
        assert!(!is_herdr_app_image(Path::new(r"C:\Windows\notepad.exe")));
    }
}
