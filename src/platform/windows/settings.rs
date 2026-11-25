//! Native Win32 settings panel.
//!
//! The panel is a modeless top-level window with standard controls. Like the
//! AppKit panel, it does not react to control notifications: the app polls
//! the controls once per tick and diffs them against the last snapshot.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_WINDOW, CreateFontW, DEFAULT_CHARSET,
    DEFAULT_PITCH, DeleteObject, FF_DONTCARE, FW_NORMAL, HBRUSH, HFONT, HGDIOBJ,
    OUT_DEFAULT_PRECIS,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    BST_CHECKED, ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, TBM_SETPOS,
    TBM_SETRANGE, TBS_HORZ, TBS_NOTICKS,
};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForSystem};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, VK_ESCAPE};
use windows::Win32::UI::Shell::{
    FILEOPENDIALOGOPTIONS, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog,
    IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL,
    CBS_DROPDOWNLIST, CreateWindowExW, DefWindowProcW, DestroyWindow, GetSystemMetrics,
    GetWindowRect, HICON, HMENU, HWND_TOPMOST, IDC_ARROW, IsWindowVisible, LoadCursorW, MoveWindow,
    RegisterClassExW, SET_WINDOW_POS_FLAGS, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOW,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SendMessageW, SetForegroundWindow,
    SetWindowPos, SetWindowTextW, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND,
    WM_DPICHANGED, WM_KEYDOWN, WM_SETFONT, WNDCLASS_STYLES, WNDCLASSEXW, WS_CAPTION, WS_CHILD,
    WS_EX_TOOLWINDOW, WS_SYSMENU, WS_VISIBLE,
};
use windows::core::{PCWSTR, w};

use crate::config::{Config, MAX_SIZE, MIN_SIZE};
use crate::platform::{PanelAction, UiState};

const WIDTH: f64 = 320.0;
const HEIGHT: f64 = 420.0;
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
const FOOTER_TOP: f64 = 386.0;
const COMBO_DROP_HEIGHT: f64 = 200.0;
/// The tray/menu click that opened the panel must finish before outside
/// clicks can close it again.
const MONITOR_ARM_DELAY: Duration = Duration::from_millis(300);
const CLASS_NAME: PCWSTR = w!("herdr_pet_settings");
const WINDOW_TITLE: &str = "herdr-pet 设置";

const ID_PET: i32 = 1001;
const ID_DIR_TEXT: i32 = 1002;
const ID_CHOOSE_DIR: i32 = 1003;
const ID_RESET_DIR: i32 = 1004;
const ID_SIZE: i32 = 1005;
const ID_OPACITY: i32 = 1006;
const ID_NOTIFICATIONS: i32 = 1010;
const ID_NOTIFICATION_SOUND: i32 = 1011;
const ID_GAZE: i32 = 1012;
const ID_CLICK_THROUGH: i32 = 1013;
const ID_HIDE_TRAY_ICON: i32 = 1014;
const ID_CHAT_OPEN: i32 = 1015;

/// Static control style: trim the path from the left with an ellipsis.
const SS_ENDELLIPSIS: u32 = 0x0000_8000;
/// Plain button style used for the auto-toggling checkboxes.
const BS_AUTOCHECKBOX: u32 = 0x0000_0003;
/// `TBM_GETPOS` is the first trackbar message (`WM_USER`).
const TBM_GETPOS: u32 = 0x0400;
/// TBM_SETRANGE packs a 16-bit min/max pair into LPARAM.
const RANGE_SHIFT: i32 = 16;
/// Trackbar positions are integers; keep one decimal for size and two for
/// opacity.
const SIZE_SCALE: f64 = 10.0;
const OPACITY_SCALE: f64 = 100.0;

/// A directory picker runs on its own STA thread; while it is open, the
/// outside-click monitor must not close the panel.
static DIALOG_OPEN: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// What the window proc needs: the action sender for button clicks.
    static LINK: RefCell<Option<PanelLink>> = const { RefCell::new(None) };
    /// Where the directory picker starts.
    static PICKER_DIR: RefCell<PathBuf> = const { RefCell::new(PathBuf::new()) };
    /// WM_DPICHANGED stores the new DPI here; the next poll relayouts.
    static PENDING_DPI: Cell<u32> = const { Cell::new(0) };
}

struct PanelLink {
    sender: Sender<PanelAction>,
}

#[derive(Clone, Copy)]
struct Child {
    hwnd: HWND,
    rect: (f64, f64, f64, f64),
}

pub struct SettingsPanel {
    hwnd: HWND,
    font: Option<HFONT>,
    pet: HWND,
    dir_label: HWND,
    reset_dir: HWND,
    size: HWND,
    opacity: HWND,
    notifications: HWND,
    notification_sound: HWND,
    gaze: HWND,
    click_through: HWND,
    hide_tray_icon: HWND,
    chat_open: HWND,
    children: Vec<Child>,
    dir_events: Receiver<PanelAction>,
    last: UiState,
    dpi: u32,
    arm_at: Option<Instant>,
    last_down: bool,
}

impl SettingsPanel {
    pub fn new(
        pets: &[String],
        selected: usize,
        config: &Config,
        pets_dir: &Path,
        custom_dir: bool,
    ) -> Option<Self> {
        // SAFETY: every Win32 object created here is owned by the panel, lives
        // on the main thread, and is released by `Drop`.
        unsafe {
            let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
                dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_BAR_CLASSES,
            });
            let instance = HINSTANCE(GetModuleHandleW(None).ok()?.0);
            register_class(instance);
            let dpi = GetDpiForSystem().max(96);
            let style = WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0);
            let ex_style = WS_EX_TOOLWINDOW;
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: (WIDTH * dpi as f64 / 96.0).round() as i32,
                bottom: (HEIGHT * dpi as f64 / 96.0).round() as i32,
            };
            AdjustWindowRectExForDpi(&mut rect, style, false, ex_style, dpi).ok()?;
            let width = rect.right - rect.left;
            let height = rect.bottom - rect.top;
            let x = ((GetSystemMetrics(SM_CXSCREEN) - width) / 2).max(0);
            let y = ((GetSystemMetrics(SM_CYSCREEN) - height) / 2).max(0);
            let title: Vec<u16> = WINDOW_TITLE.encode_utf16().chain(Some(0)).collect();
            let hwnd = match CreateWindowExW(
                ex_style,
                CLASS_NAME,
                PCWSTR(title.as_ptr()),
                style,
                x,
                y,
                width,
                height,
                None,
                None,
                Some(instance),
                None,
            ) {
                Ok(hwnd) => hwnd,
                Err(error) => {
                    eprintln!("herdr-pet: cannot create the settings window: {error}");
                    return None;
                }
            };
            let font = create_font(dpi);
            let (sender, dir_events) = channel();
            LINK.with(|link| {
                *link.borrow_mut() = Some(PanelLink { sender });
            });
            let built = build_children(hwnd, dpi, font);
            let Some((children, controls)) = built else {
                let _ = DestroyWindow(hwnd);
                return None;
            };
            let mut panel = Self {
                hwnd,
                font: (!font.0.is_null()).then_some(font),
                pet: controls.pet,
                dir_label: controls.dir_label,
                reset_dir: controls.reset_dir,
                size: controls.size,
                opacity: controls.opacity,
                notifications: controls.notifications,
                notification_sound: controls.notification_sound,
                gaze: controls.gaze,
                click_through: controls.click_through,
                hide_tray_icon: controls.hide_tray_icon,
                chat_open: controls.chat_open,
                children,
                dir_events,
                last: UiState::default(),
                dpi,
                arm_at: None,
                last_down: false,
            };
            panel.sync_pets(pets, selected, pets_dir, custom_dir);
            panel.sync(config, selected);
            Some(panel)
        }
    }

    pub fn show(&mut self) {
        // SAFETY: the HWND belongs to this panel on the main thread.
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SET_WINDOW_POS_FLAGS(SWP_NOMOVE.0 | SWP_NOSIZE.0 | SWP_NOACTIVATE.0),
            );
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            // The tray menu is owned by this process, so it may still hold
            // foreground rights; if not, the window is visible but unfocused
            // until clicked, which is harmless.
            let _ = SetForegroundWindow(self.hwnd);
        }
        self.arm_at = Some(Instant::now() + MONITOR_ARM_DELAY);
        self.last_down = super::left_button_down();
    }

    /// Changes the user made since the last poll, plus directory-picker
    /// results. Controls are read by value, so sliders and toggles need no
    /// callbacks.
    pub fn poll(&mut self) -> Vec<PanelAction> {
        // SAFETY: querying this panel's visibility and rect has no
        // preconditions.
        if !unsafe { IsWindowVisible(self.hwnd) }.as_bool() {
            self.arm_at = None;
            self.last_down = false;
            return Vec::new();
        }
        self.apply_pending_dpi();
        let mut actions = Vec::new();
        while let Ok(action) = self.dir_events.try_recv() {
            actions.push(action);
        }
        if let Some(arm_at) = self.arm_at
            && Instant::now() >= arm_at
        {
            let down = super::left_button_down();
            if down && !self.last_down && !DIALOG_OPEN.load(Ordering::Relaxed) {
                self.close_if_pointer_outside();
            }
            self.last_down = down;
        }
        let current = self.read();
        actions.extend(current.actions_since(&self.last));
        self.last = current;
        actions
    }

    fn close_if_pointer_outside(&self) {
        let Some((x, y)) = super::mouse_position(0.0, 1.0) else {
            return;
        };
        let mut rect = RECT::default();
        // SAFETY: `rect` is a valid out buffer for this window's screen rect.
        if unsafe { GetWindowRect(self.hwnd, &mut rect) }.is_err() {
            return;
        }
        let inside = x >= f64::from(rect.left)
            && x < f64::from(rect.right)
            && y >= f64::from(rect.top)
            && y < f64::from(rect.bottom);
        if !inside {
            // SAFETY: hiding a window owned by this thread is always allowed.
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }

    /// Push the app's config into the controls, e.g. after a tray action
    /// changed it while the panel was open.
    pub fn sync(&mut self, config: &Config, selected: usize) {
        set_combo_selection(self.pet, selected);
        set_checked(self.notifications, config.notifications);
        set_checked(self.notification_sound, config.notification_sound);
        set_checked(self.gaze, config.gaze);
        set_checked(self.click_through, config.click_through);
        set_checked(self.hide_tray_icon, config.hide_tray_icon);
        set_checked(self.chat_open, config.chat_open);
        set_trackbar(
            self.size,
            (config.size * SIZE_SCALE).round() as i32,
            (MIN_SIZE * SIZE_SCALE).round() as i32,
            (MAX_SIZE * SIZE_SCALE).round() as i32,
        );
        set_trackbar(
            self.opacity,
            (config.chat_opacity * OPACITY_SCALE).round() as i32,
            0,
            OPACITY_SCALE as i32,
        );
        // The toggle has no effect while the pet ignores the mouse.
        // SAFETY: enabling a child control is allowed on the owning thread.
        unsafe {
            let _ = EnableWindow(self.hide_tray_icon, !config.click_through);
        }
        self.last = self.read();
    }

    /// Refresh the pet list and the effective root directory.
    pub fn sync_pets(
        &mut self,
        pets: &[String],
        selected: usize,
        pets_dir: &Path,
        custom_dir: bool,
    ) {
        send_message(self.pet, CB_RESETCONTENT, 0, 0);
        for name in pets {
            let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            send_message(self.pet, CB_ADDSTRING, 0, wide.as_ptr() as isize);
        }
        if !pets.is_empty() {
            set_combo_selection(self.pet, selected.min(pets.len() - 1));
        }
        set_text(self.dir_label, &pets_dir.to_string_lossy());
        // SAFETY: enabling a child control is allowed on the owning thread.
        unsafe {
            let _ = EnableWindow(self.reset_dir, custom_dir);
        }
        PICKER_DIR.with(|dir| *dir.borrow_mut() = pets_dir.to_path_buf());
        self.last.pet = selection(self.pet);
    }

    fn read(&self) -> UiState {
        UiState {
            pet: selection(self.pet),
            size: f64::from(send_message(self.size, TBM_GETPOS, 0, 0) as i32) / SIZE_SCALE,
            opacity: f64::from(send_message(self.opacity, TBM_GETPOS, 0, 0) as i32) / OPACITY_SCALE,
            notifications: checked(self.notifications),
            notification_sound: checked(self.notification_sound),
            gaze: checked(self.gaze),
            click_through: checked(self.click_through),
            hide_tray_icon: checked(self.hide_tray_icon),
            chat_open: checked(self.chat_open),
        }
    }

    fn apply_pending_dpi(&mut self) {
        let dpi = PENDING_DPI.with(|pending| pending.replace(0));
        if dpi == 0 || dpi == self.dpi {
            return;
        }
        self.dpi = dpi;
        let font = create_font(dpi);
        let replaced = if font.0.is_null() {
            None
        } else {
            self.font.replace(font)
        };
        if let Some(old) = replaced {
            // SAFETY: the old font is no longer selected by any child after
            // the WM_SETFONT messages below; it was created by us.
            unsafe {
                let _ = DeleteObject(HGDIOBJ(old.0));
            }
        }
        let font = self.font;
        for child in &self.children {
            let (x, y, width, height) = pixels(child.rect, dpi);
            // SAFETY: moving and re-fonting our own child controls is safe.
            unsafe {
                let _ = MoveWindow(child.hwnd, x, y, width, height, true);
                if let Some(font) = font {
                    let _ = SendMessageW(
                        child.hwnd,
                        WM_SETFONT,
                        Some(WPARAM(font.0 as usize)),
                        Some(LPARAM(1)),
                    );
                }
            }
        }
    }
}

impl Drop for SettingsPanel {
    fn drop(&mut self) {
        LINK.with(|link| *link.borrow_mut() = None);
        PICKER_DIR.with(|dir| dir.borrow_mut().clear());
        // SAFETY: the window and font belong to this panel and are released
        // once. Children are destroyed with the window before the font.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
        if let Some(font) = self.font.take() {
            // SAFETY: the font was created by `create_font` and is unused now.
            unsafe {
                let _ = DeleteObject(HGDIOBJ(font.0));
            }
        }
    }
}

struct Controls {
    pet: HWND,
    dir_label: HWND,
    reset_dir: HWND,
    size: HWND,
    opacity: HWND,
    notifications: HWND,
    notification_sound: HWND,
    gaze: HWND,
    click_through: HWND,
    hide_tray_icon: HWND,
    chat_open: HWND,
}

/// Create every control and return the list used for DPI relayout plus the
/// handles the panel reads and writes. Returns None if any control fails.
fn build_children(parent: HWND, dpi: u32, font: HFONT) -> Option<(Vec<Child>, Controls)> {
    let mut children = Vec::new();
    let mut add = |class: PCWSTR,
                   text: &str,
                   id: i32,
                   style: u32,
                   rect: (f64, f64, f64, f64)|
     -> Option<HWND> {
        let child = create_child(parent, class, text, id, style, rect, dpi)?;
        if !font.0.is_null() {
            // SAFETY: the child belongs to this thread and the font outlives
            // the control.
            unsafe {
                let _ = SendMessageW(
                    child.hwnd,
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
        }
        children.push(child);
        Some(child.hwnd)
    };
    add(
        w!("STATIC"),
        "宠物",
        0,
        0,
        (PAD, PET_TOP + 4.0, LABEL_WIDTH, 16.0),
    )?;
    let pet = add(
        w!("COMBOBOX"),
        "",
        ID_PET,
        CBS_DROPDOWNLIST as u32,
        (CONTROL_X, PET_TOP, CONTROL_WIDTH, COMBO_DROP_HEIGHT),
    )?;
    add(
        w!("STATIC"),
        "宠物目录",
        0,
        0,
        (PAD, DIR_TOP + 4.0, LABEL_WIDTH, 16.0),
    )?;
    let dir_label = add(
        w!("STATIC"),
        "",
        ID_DIR_TEXT,
        SS_ENDELLIPSIS,
        (CONTROL_X, DIR_TOP + 2.0, CONTROL_WIDTH, 18.0),
    )?;
    add(
        w!("BUTTON"),
        "选择目录…",
        ID_CHOOSE_DIR,
        0,
        (CONTROL_X, DIR_BUTTON_TOP, 100.0, ROW_HEIGHT),
    )?;
    let reset_dir = add(
        w!("BUTTON"),
        "恢复默认",
        ID_RESET_DIR,
        0,
        (
            CONTROL_X + 106.0,
            DIR_BUTTON_TOP,
            CONTROL_WIDTH - 106.0,
            ROW_HEIGHT,
        ),
    )?;
    add(
        w!("STATIC"),
        "大小",
        0,
        0,
        (PAD, SIZE_TOP + 4.0, LABEL_WIDTH, 16.0),
    )?;
    let size = add(
        w!("msctls_trackbar32"),
        "",
        ID_SIZE,
        TBS_HORZ | TBS_NOTICKS,
        (CONTROL_X, SIZE_TOP, CONTROL_WIDTH, ROW_HEIGHT),
    )?;
    add(
        w!("STATIC"),
        "气泡不透明度",
        0,
        0,
        (PAD, OPACITY_TOP + 4.0, LABEL_WIDTH, 16.0),
    )?;
    let opacity = add(
        w!("msctls_trackbar32"),
        "",
        ID_OPACITY,
        TBS_HORZ | TBS_NOTICKS,
        (CONTROL_X, OPACITY_TOP, CONTROL_WIDTH, ROW_HEIGHT),
    )?;
    let toggles = [
        ("提醒", TOGGLE_TOP, ID_NOTIFICATIONS),
        ("通知音效", TOGGLE_TOP + TOGGLE_STEP, ID_NOTIFICATION_SOUND),
        ("凝视", TOGGLE_TOP + TOGGLE_STEP * 2.0, ID_GAZE),
        ("点击穿透", TOGGLE_TOP + TOGGLE_STEP * 3.0, ID_CLICK_THROUGH),
        (
            "隐藏托盘图标",
            TOGGLE_TOP + TOGGLE_STEP * 4.0,
            ID_HIDE_TRAY_ICON,
        ),
        ("聊天气泡展开", TOGGLE_TOP + TOGGLE_STEP * 5.0, ID_CHAT_OPEN),
    ];
    let mut toggles_created = Vec::with_capacity(toggles.len());
    for (title, top, id) in toggles {
        toggles_created.push(add(
            w!("BUTTON"),
            title,
            id,
            BS_AUTOCHECKBOX,
            (PAD, top, WIDTH - PAD * 2.0, 20.0),
        )?);
    }
    add(
        w!("STATIC"),
        "修改会立即生效并保存",
        0,
        0,
        (PAD, FOOTER_TOP, WIDTH - PAD * 2.0, 16.0),
    )?;
    let [
        notifications,
        notification_sound,
        gaze,
        click_through,
        hide_tray_icon,
        chat_open,
    ] = <[HWND; 6]>::try_from(toggles_created).ok()?;
    Some((
        children,
        Controls {
            pet,
            dir_label,
            reset_dir,
            size,
            opacity,
            notifications,
            notification_sound,
            gaze,
            click_through,
            hide_tray_icon,
            chat_open,
        },
    ))
}

fn create_child(
    parent: HWND,
    class: PCWSTR,
    text: &str,
    id: i32,
    style: u32,
    rect: (f64, f64, f64, f64),
    dpi: u32,
) -> Option<Child> {
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let (x, y, width, height) = pixels(rect, dpi);
    // SAFETY: the class name and text are live wide strings for the call; the
    // parent is owned by the panel.
    unsafe {
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            PCWSTR(wide.as_ptr()),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | style),
            x,
            y,
            width,
            height,
            Some(parent),
            Some(HMENU(id as isize as *mut c_void)),
            None,
            None,
        )
        .ok()?;
        Some(Child { hwnd, rect })
    }
}

fn register_class(instance: HINSTANCE) {
    // SAFETY: the class name is a static wide string and the window proc has
    // the required signature. Registering an already-registered class is
    // harmless; the atom is unused.
    unsafe {
        let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: HICON::default(),
            hCursor: cursor,
            hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as isize as *mut c_void),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: CLASS_NAME,
            hIconSm: HICON::default(),
        };
        let _ = RegisterClassExW(&class);
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            match id {
                ID_CHOOSE_DIR => {
                    spawn_directory_dialog(hwnd);
                    LRESULT(0)
                }
                ID_RESET_DIR => {
                    LINK.with(|link| {
                        if let Some(link) = &*link.borrow() {
                            let _ = link.sender.send(PanelAction::ResetPetsDir);
                        }
                    });
                    LRESULT(0)
                }
                _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
            }
        }
        WM_CLOSE => {
            // SAFETY: hiding the panel is the documented close behaviour; the
            // instance stays alive for the next open.
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 == usize::from(VK_ESCAPE.0) => {
            // SAFETY: same as WM_CLOSE.
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let dpi = ((wparam.0 >> 16) & 0xFFFF) as u32;
            PENDING_DPI.with(|pending| pending.set(dpi.max(96)));
            // SAFETY: the system passes a valid suggested RECT in LPARAM.
            unsafe {
                let suggested = &*(lparam.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SET_WINDOW_POS_FLAGS(SWP_NOZORDER.0 | SWP_NOACTIVATE.0),
                );
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn spawn_directory_dialog(parent: HWND) {
    let Some(sender) = LINK.with(|link| link.borrow().as_ref().map(|link| link.sender.clone()))
    else {
        return;
    };
    let dir = PICKER_DIR.with(|dir| dir.borrow().clone());
    let parent = parent.0 as isize;
    DIALOG_OPEN.store(true, Ordering::SeqCst);
    std::thread::spawn(move || {
        let chosen = choose_directory(&dir, parent);
        DIALOG_OPEN.store(false, Ordering::SeqCst);
        if let Some(path) = chosen {
            let _ = sender.send(PanelAction::SetPetsDir(path));
        }
    });
}

/// Run the folder picker on a dedicated STA thread, since the main thread is
/// already initialized as MTA by the WinRT toast setup.
fn choose_directory(initial: &Path, parent: isize) -> Option<PathBuf> {
    // SAFETY: COM is initialized for this thread and every interface is
    // released before returning.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = FILEOPENDIALOGOPTIONS(FOS_PICKFOLDERS.0 | FOS_FORCEFILESYSTEM.0);
        let _ = dialog.SetOptions(options);
        let wide: Vec<u16> = initial.as_os_str().encode_wide().chain(Some(0)).collect();
        if let Ok(folder) =
            SHCreateItemFromParsingName::<PCWSTR, _, IShellItem>(PCWSTR(wide.as_ptr()), None)
        {
            let _ = dialog.SetFolder(&folder);
        }
        if dialog.Show(Some(HWND(parent as *mut c_void))).is_err() {
            return None;
        }
        let item = dialog.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        if name.0.is_null() {
            return None;
        }
        let mut len = 0usize;
        while *name.0.add(len) != 0 {
            len += 1;
        }
        let path = String::from_utf16_lossy(std::slice::from_raw_parts(name.0, len));
        CoTaskMemFree(Some(name.0.cast()));
        Some(PathBuf::from(path))
    }
}

fn create_font(dpi: u32) -> HFONT {
    let height = -((9 * dpi as i32) / 72);
    // SAFETY: the face name is a static wide string; the font is owned by
    // the panel and released in its Drop.
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            u32::from(DEFAULT_PITCH.0) | u32::from(FF_DONTCARE.0),
            w!("Microsoft YaHei UI"),
        )
    }
}

/// Scale a logical rectangle by the window DPI.
fn pixels(rect: (f64, f64, f64, f64), dpi: u32) -> (i32, i32, i32, i32) {
    let scale = dpi as f64 / 96.0;
    (
        (rect.0 * scale).round() as i32,
        (rect.1 * scale).round() as i32,
        (rect.2 * scale).round() as i32,
        (rect.3 * scale).round() as i32,
    )
}

fn set_text(hwnd: HWND, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    // SAFETY: `wide` is a live NUL-terminated UTF-16 buffer for this call.
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(wide.as_ptr()));
    }
}

fn send_message(hwnd: HWND, message: u32, wparam: usize, lparam: isize) -> isize {
    // SAFETY: the message and argument packing match the control's contract.
    unsafe { SendMessageW(hwnd, message, Some(WPARAM(wparam)), Some(LPARAM(lparam))).0 }
}

fn checked(hwnd: HWND) -> bool {
    (send_message(hwnd, BM_GETCHECK, 0, 0) as u32) == BST_CHECKED.0
}

fn set_checked(hwnd: HWND, value: bool) {
    let state = if value { BST_CHECKED.0 } else { 0 };
    let _ = send_message(hwnd, BM_SETCHECK, state as usize, 0);
}

fn selection(combo: HWND) -> isize {
    send_message(combo, CB_GETCURSEL, 0, 0)
}

fn set_combo_selection(combo: HWND, index: usize) {
    let _ = send_message(combo, CB_SETCURSEL, index, 0);
}

fn set_trackbar(trackbar: HWND, position: i32, min: i32, max: i32) {
    // TBM_SETRANGE expects LPARAM to contain the min and max as 16-bit
    // values in the low and high words.
    let packed = (min & 0xFFFF) | ((max & 0xFFFF) << RANGE_SHIFT);
    let _ = send_message(trackbar, TBM_SETRANGE, 1, packed as isize);
    let _ = send_message(trackbar, TBM_SETPOS, 1, position as isize);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_logical_rectangles_for_the_dpi() {
        assert_eq!(pixels((20.0, 20.0, 96.0, 24.0), 96), (20, 20, 96, 24));
        assert_eq!(pixels((20.0, 20.0, 96.0, 24.0), 144), (30, 30, 144, 36));
        assert_eq!(pixels((20.0, 20.0, 96.0, 24.0), 192), (40, 40, 192, 48));
    }

    #[test]
    fn checkbox_messages_use_the_button_style_values() {
        assert_eq!(BST_CHECKED.0, 1);
        assert_eq!(BM_GETCHECK, 240);
        assert_eq!(BM_SETCHECK, 241);
    }
}
