//! Direct2D chat overlay.
//!
//! macOS draws the bubble with native AppKit views. Windows rasterizes the
//! same layout into a premultiplied BGRA bitmap with Direct2D/DirectWrite and
//! hands it to the wgpu renderer, which draws it over the pet. There is no
//! background blur; the glass is a theme-aware translucent fill.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use resvg::{tiny_skia, usvg};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES, D2D1_CAP_STYLE_ROUND,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1Brush, ID2D1DCRenderTarget,
    ID2D1Factory, ID2D1SolidColorBrush, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
    DWriteCreateFactory, IDWriteFactory, IDWriteInlineObject, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, HBITMAP, HDC, HGDIOBJ, SelectObject,
};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, RRF_RT_REG_DWORD, RegCloseKey, RegGetValueW, RegOpenKeyExW,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::w;
use windows_numerics::Vector2;
use winit::window::Window;

use crate::chat::{ARROW_RESERVED, BUBBLE_HEIGHT, BUTTON_SIZE, ChatLayout, Rect};
use crate::platform::OverlayBitmap;
use crate::status::AgentStatus;

const TITLE_INSET_TOP: f32 = 12.0;
const TEXT_INSET_X: f32 = 16.0;
const TEXT_LINE_HEIGHT: f32 = 16.0;
const TEXT_LINE_GAP: f32 = 4.0;
const CHEVRON_SIZE: f32 = 13.0;
const ARROW_SYMBOL_SIZE: f32 = 12.0;
const AGENT_ICON_SIZE: f32 = 14.0;
const AGENT_ICON_GAP: f32 = 6.0;
const TEXT_SIZE: f32 = 13.0;
const BORDER_WIDTH: f32 = 1.0;
/// The spinner is redrawn at most this often; the app ticks at 60 Hz but a
/// full-window bitmap upload does not need that rate.
const SPINNER_INTERVAL: Duration = Duration::from_millis(33);
/// Theme changes are rare; a two-second poll is cheap and keeps the bubble in
/// step with Windows' light/dark switch.
const THEME_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Theme {
    Light,
    Dark,
}

/// Colors for one theme. `opacity` is already folded into the glass and
/// border alphas; text and icons stay opaque.
#[derive(Clone, Copy)]
struct Palette {
    text: D2D1_COLOR_F,
    subtitle: D2D1_COLOR_F,
    glass: D2D1_COLOR_F,
    border: D2D1_COLOR_F,
    muted: D2D1_COLOR_F,
}

fn color(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

fn status_color(status: Option<AgentStatus>, theme: Theme) -> D2D1_COLOR_F {
    let (dark, light) = match status {
        Some(AgentStatus::Working) => (color(0.36, 0.65, 1.0, 0.95), color(0.10, 0.43, 0.85, 0.95)),
        Some(AgentStatus::Blocked) => (color(1.0, 0.67, 0.24, 0.95), color(0.80, 0.47, 0.02, 0.95)),
        Some(AgentStatus::Done) => (color(0.36, 0.85, 0.47, 0.95), color(0.14, 0.60, 0.26, 0.95)),
        Some(AgentStatus::Error) => (color(1.0, 0.38, 0.38, 0.95), color(0.80, 0.16, 0.16, 0.95)),
        Some(AgentStatus::Idle) | None => {
            (color(0.78, 0.78, 0.80, 0.9), color(0.36, 0.36, 0.40, 0.9))
        }
    };
    match theme {
        Theme::Dark => dark,
        Theme::Light => light,
    }
}

impl Palette {
    fn new(theme: Theme, opacity: f64) -> Self {
        let opacity = opacity.clamp(0.0, 1.0) as f32;
        match theme {
            Theme::Dark => Self {
                text: color(1.0, 1.0, 1.0, 0.95),
                subtitle: color(1.0, 1.0, 1.0, 0.82),
                glass: color(0.09, 0.09, 0.11, 0.88 * opacity),
                border: color(1.0, 1.0, 1.0, 0.18 * opacity),
                muted: color(1.0, 1.0, 1.0, 0.72),
            },
            Theme::Light => Self {
                text: color(0.07, 0.07, 0.09, 0.95),
                subtitle: color(0.24, 0.24, 0.28, 0.85),
                glass: color(0.99, 0.99, 1.0, 0.92 * opacity),
                border: color(0.1, 0.1, 0.12, 0.16 * opacity),
                muted: color(0.2, 0.2, 0.24, 0.72),
            },
        }
    }
}

/// Frame inputs from the last layout pass. The bubble can flip below the pet
/// or slide sideways, so the rasterized bitmap follows the logical layout.
#[derive(Clone, Copy)]
struct Frames {
    layout: ChatLayout,
    pager: bool,
}

/// Rasterized icon and the inputs it was built from.
///
/// The pixels are kept, but not the `ID2D1Bitmap`: Direct2D resources only
/// work with the render target that created them, and the target is rebuilt
/// whenever the window (or its DPI) changes size. Uploading ~28×28 pixels per
/// paint is cheap and avoids a stale handle failing `EndDraw` with
/// `D2DERR_WRONG_RENDER_TARGET` (0x88990015).
struct IconBitmap {
    key: (PathBuf, bool, [u8; 3]),
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

/// A Direct2D device-context-free target that renders into a top-down DIB.
struct Target {
    render_target: ID2D1DCRenderTarget,
    hdc: HDC,
    hbitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
    bits: *mut c_void,
    width: u32,
    height: u32,
    dpi: u32,
}

impl Drop for Target {
    fn drop(&mut self) {
        // SAFETY: every handle came from the matching creation call above and
        // is released exactly once, on the thread that made it.
        unsafe {
            SelectObject(self.hdc, self.old_bitmap);
            let _ = DeleteDC(self.hdc);
            let _ = DeleteObject(HGDIOBJ(self.hbitmap.0));
        }
    }
}

impl Target {
    fn new(render_target: &ID2D1DCRenderTarget, width: u32, height: u32, dpi: u32) -> Option<Self> {
        let header_size = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: header_size,
                biWidth: width as i32,
                // Negative height gives top-down rows, matching wgpu's upload
                // order without a flip.
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        // SAFETY: `info` is fully initialized and `bits` is a valid out
        // pointer; the DIB lives as long as this target.
        let hbitmap =
            unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()? };
        if bits.is_null() {
            // SAFETY: the bitmap was created above and is not used elsewhere.
            unsafe {
                let _ = DeleteObject(HGDIOBJ(hbitmap.0));
            }
            return None;
        }
        // SAFETY: a memory DC with a selected top-down DIB is a valid target
        // for ID2D1DCRenderTarget::BindDC.
        unsafe {
            let hdc = CreateCompatibleDC(None);
            if hdc.0.is_null() {
                let _ = DeleteObject(HGDIOBJ(hbitmap.0));
                return None;
            }
            let old_bitmap = SelectObject(hdc, HGDIOBJ(hbitmap.0));
            let rect = RECT {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            };
            if let Err(error) = render_target.BindDC(hdc, &rect) {
                eprintln!("herdr-pet: cannot bind the overlay device context: {error}");
                SelectObject(hdc, old_bitmap);
                let _ = DeleteDC(hdc);
                let _ = DeleteObject(HGDIOBJ(hbitmap.0));
                return None;
            }
            render_target.SetDpi(dpi as f32, dpi as f32);
            Some(Self {
                render_target: render_target.clone(),
                hdc,
                hbitmap,
                old_bitmap,
                bits,
                width,
                height,
                dpi,
            })
        }
    }

    fn set_dpi(&mut self, dpi: u32) {
        if self.dpi == dpi {
            return;
        }
        self.dpi = dpi;
        // SAFETY: the target belongs to this thread and is not drawing.
        unsafe {
            self.render_target.SetDpi(dpi as f32, dpi as f32);
        }
    }

    fn pixels(&self) -> Vec<u8> {
        let len = (self.width as usize) * (self.height as usize) * 4;
        // SAFETY: `bits` points to `width * height * 4` readable bytes owned
        // by the DIB for the lifetime of this target.
        unsafe { std::slice::from_raw_parts(self.bits.cast::<u8>(), len).to_vec() }
    }
}

pub struct ChatOverlay {
    hwnd: HWND,
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    title_format: IDWriteTextFormat,
    body_format: IDWriteTextFormat,
    ellipsis: IDWriteInlineObject,
    round_caps: ID2D1StrokeStyle,
    target: Option<Target>,
    frames: Option<Frames>,
    title: String,
    subtitle: String,
    status: Option<AgentStatus>,
    icon: Option<(PathBuf, bool)>,
    icon_bitmap: Option<IconBitmap>,
    bubble_visible: bool,
    button_visible: bool,
    opacity: f64,
    theme: Theme,
    next_theme_check: Instant,
    dirty: bool,
    last_paint: Instant,
    started: Instant,
}

impl ChatOverlay {
    pub fn new(window: &Window, open: bool, opacity: f64) -> Option<Self> {
        let hwnd = super::hwnd(window)?;
        // SAFETY: Direct2D and DirectWrite factories are apartment-agnostic
        // and are used only on this (the main) thread.
        let (factory, dwrite) = unsafe {
            let factory: ID2D1Factory =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok()?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
            (factory, dwrite)
        };
        let title_format = make_text_format(&dwrite, DWRITE_FONT_WEIGHT_BOLD)?;
        let body_format = make_text_format(&dwrite, DWRITE_FONT_WEIGHT_NORMAL)?;
        // SAFETY: the trimming sign belongs to the DWrite factory.
        let ellipsis = unsafe { dwrite.CreateEllipsisTrimmingSign(&body_format).ok()? };
        let round_caps = unsafe {
            factory
                .CreateStrokeStyle(
                    &D2D1_STROKE_STYLE_PROPERTIES {
                        startCap: D2D1_CAP_STYLE_ROUND,
                        endCap: D2D1_CAP_STYLE_ROUND,
                        dashCap: D2D1_CAP_STYLE_ROUND,
                        ..Default::default()
                    },
                    None,
                )
                .ok()?
        };
        let now = Instant::now();
        Some(Self {
            hwnd,
            factory,
            dwrite,
            title_format,
            body_format,
            ellipsis,
            round_caps,
            target: None,
            frames: None,
            title: String::new(),
            subtitle: String::new(),
            status: None,
            icon: None,
            icon_bitmap: None,
            bubble_visible: false,
            button_visible: open,
            opacity: opacity.clamp(0.0, 1.0),
            theme: current_theme(),
            next_theme_check: now + THEME_INTERVAL,
            dirty: true,
            last_paint: now - SPINNER_INTERVAL,
            started: now,
        })
    }

    /// Remember the logical layout and the pager state. The next
    /// `take_bitmap` re-rasterizes; the window size itself is read from the
    /// HWND so a DPI change needs no extra input.
    pub fn layout(&mut self, layout: &ChatLayout, _height: f64, pager: bool) {
        self.frames = Some(Frames {
            layout: *layout,
            pager,
        });
        self.dirty = true;
    }

    pub fn set_text(&mut self, title: &str, subtitle: &str) {
        if self.title == title && self.subtitle == subtitle {
            return;
        }
        self.title.clear();
        self.title.push_str(title);
        self.subtitle.clear();
        self.subtitle.push_str(subtitle);
        self.dirty = true;
    }

    pub fn set_icon(&mut self, icon: Option<(&Path, bool)>) {
        let next = icon.map(|(path, tint)| (path.to_path_buf(), tint));
        if self.icon == next {
            return;
        }
        self.icon = next;
        self.icon_bitmap = None;
        self.dirty = true;
    }

    pub fn set_status(&mut self, status: Option<AgentStatus>) {
        if self.status == status {
            return;
        }
        self.status = status;
        self.dirty = true;
    }

    pub fn set_visible(&mut self, bubble: bool, button: bool) {
        if self.bubble_visible == bubble && self.button_visible == button {
            return;
        }
        self.bubble_visible = bubble;
        self.button_visible = button;
        self.dirty = true;
    }

    pub fn set_opacity(&mut self, opacity: f64) {
        let opacity = opacity.clamp(0.0, 1.0);
        if (self.opacity - opacity).abs() < f64::EPSILON {
            return;
        }
        self.opacity = opacity;
        self.dirty = true;
    }

    /// Rasterize when the inputs changed, or on the spinner clock while a
    /// working agent is on screen. Returns the whole-window bitmap for wgpu.
    pub fn take_bitmap(&mut self) -> Option<OverlayBitmap> {
        let now = Instant::now();
        if now >= self.next_theme_check {
            self.next_theme_check = now + THEME_INTERVAL;
            let theme = current_theme();
            if theme != self.theme {
                self.theme = theme;
                self.icon_bitmap = None;
                self.dirty = true;
            }
        }
        let working = self.bubble_visible && self.status == Some(AgentStatus::Working);
        if working && !self.dirty && now.duration_since(self.last_paint) >= SPINNER_INTERVAL {
            self.dirty = true;
        }
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        self.last_paint = now;
        match self.paint(now) {
            Ok(bitmap) => bitmap,
            Err(error) => {
                eprintln!("herdr-pet: cannot draw the chat overlay: {error}");
                None
            }
        }
    }

    fn paint(&mut self, now: Instant) -> windows::core::Result<Option<OverlayBitmap>> {
        let Some(frames) = self.frames else {
            return Ok(None);
        };
        let mut rect = RECT::default();
        // SAFETY: the HWND belongs to the live pet window.
        unsafe { GetClientRect(self.hwnd, &mut rect)? };
        let width = (rect.right - rect.left).max(0) as u32;
        let height = (rect.bottom - rect.top).max(0) as u32;
        if width == 0 || height == 0 {
            return Ok(None);
        }
        // SAFETY: reading the window DPI has no preconditions.
        let dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(96);
        if self.target.is_none()
            || self
                .target
                .as_ref()
                .is_some_and(|target| target.width != width || target.height != height)
        {
            let properties = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: dpi as f32,
                dpiY: dpi as f32,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            // SAFETY: the properties are fully initialized; the DC target owns
            // no window and is used only here.
            let render_target = unsafe {
                self.factory
                    .CreateDCRenderTarget(&properties)
                    .inspect_err(|error| {
                        eprintln!("herdr-pet: cannot create the overlay render target: {error}");
                    })?
            };
            let Some(target) = Target::new(&render_target, width, height, dpi) else {
                return Ok(None);
            };
            self.target = Some(target);
            // The icon raster was sized for the old target's DPI; let the next
            // draw re-read it at the new scale.
            self.icon_bitmap = None;
        }
        let render_target = {
            let Some(target) = self.target.as_mut() else {
                return Ok(None);
            };
            target.set_dpi(dpi);
            target.render_target.clone()
        };
        let palette = Palette::new(self.theme, self.opacity);
        // SAFETY: drawing happens on the owning thread; every returned COM
        // object is released at the end of this scope.
        unsafe {
            render_target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            render_target.BeginDraw();
            let transparent = color(0.0, 0.0, 0.0, 0.0);
            render_target.Clear(Some(&transparent));
            let result = self.draw(&render_target, &frames, &palette, dpi, now);
            let ended = render_target.EndDraw(None, None);
            result?;
            ended?;
        }
        let Some(target) = self.target.as_ref() else {
            return Ok(None);
        };
        Ok(Some(OverlayBitmap {
            width,
            height,
            pixels: target.pixels(),
        }))
    }

    fn draw(
        &mut self,
        rt: &ID2D1DCRenderTarget,
        frames: &Frames,
        palette: &Palette,
        dpi: u32,
        now: Instant,
    ) -> windows::core::Result<()> {
        let layout = &frames.layout;
        let text = brush(rt, palette.text)?;
        let subtitle = brush(rt, palette.subtitle)?;
        let glass = brush(rt, palette.glass)?;
        let border = brush(rt, palette.border)?;
        let muted = brush(rt, palette.muted)?;
        let status = brush(rt, status_color(self.status, self.theme))?;

        if self.bubble_visible {
            let rect = rect_f(layout.bubble);
            let rounded = D2D1_ROUNDED_RECT {
                rect,
                radiusX: BUBBLE_HEIGHT as f32 / 2.0,
                radiusY: BUBBLE_HEIGHT as f32 / 2.0,
            };
            // SAFETY: all arguments are plain values on the current stack.
            unsafe {
                rt.FillRoundedRectangle(&rounded, brush_ref(&glass));
                rt.DrawRoundedRectangle(
                    &rounded,
                    brush_ref(&border),
                    BORDER_WIDTH,
                    None::<&ID2D1StrokeStyle>,
                );
            }
            self.prepare_icon(dpi);
            let icon_extra = if self.icon_bitmap.is_some() {
                AGENT_ICON_SIZE + AGENT_ICON_GAP
            } else {
                0.0
            };
            self.draw_icon(rt, layout);
            let title_y = layout.bubble.y as f32 + TITLE_INSET_TOP;
            let subtitle_y = title_y + TEXT_LINE_HEIGHT + TEXT_LINE_GAP;
            let text_right = layout.bubble.x as f32 + layout.bubble.w as f32
                - if frames.pager {
                    ARROW_RESERVED as f32
                } else {
                    TEXT_INSET_X
                };
            let title_left = layout.bubble.x as f32 + TEXT_INSET_X + icon_extra;
            self.draw_text(
                rt,
                &self.title,
                &self.title_format,
                (title_left, title_y),
                text_right - title_left,
                &text,
            );
            let subtitle_left =
                layout.bubble.x as f32 + TEXT_INSET_X + AGENT_ICON_SIZE + AGENT_ICON_GAP;
            self.draw_text(
                rt,
                &self.subtitle,
                &self.body_format,
                (subtitle_left, subtitle_y),
                text_right - subtitle_left,
                &subtitle,
            );
            let status_center = (
                layout.bubble.x as f32 + TEXT_INSET_X + AGENT_ICON_SIZE / 2.0,
                subtitle_y + TEXT_LINE_HEIGHT / 2.0,
            );
            draw_status(
                rt,
                status_center,
                self.status,
                &status,
                &muted,
                &self.round_caps,
                now.duration_since(self.started),
            );
            if frames.pager {
                draw_chevron(
                    rt,
                    layout.prev_arrow.center(),
                    -1.0,
                    &muted,
                    &self.round_caps,
                );
                draw_chevron(
                    rt,
                    layout.next_arrow.center(),
                    1.0,
                    &muted,
                    &self.round_caps,
                );
            }
        }
        if self.button_visible {
            let rect = rect_f(layout.button);
            let radius = BUTTON_SIZE as f32 / 2.0;
            let rounded = D2D1_ROUNDED_RECT {
                rect,
                radiusX: radius,
                radiusY: radius,
            };
            // SAFETY: plain values, same render target as above.
            unsafe {
                rt.FillRoundedRectangle(&rounded, brush_ref(&glass));
                rt.DrawRoundedRectangle(
                    &rounded,
                    brush_ref(&border),
                    BORDER_WIDTH,
                    None::<&ID2D1StrokeStyle>,
                );
            }
            draw_toggle_chevron(
                rt,
                layout.button.center(),
                self.bubble_visible,
                &text,
                &self.round_caps,
            );
        }
        Ok(())
    }

    /// Rasterize the agent icon at twice the on-screen size for crisp edges
    /// at every DPI. The SVG is read once per path/tint/theme combination.
    fn prepare_icon(&mut self, dpi: u32) {
        let Some((path, tint)) = &self.icon else {
            self.icon_bitmap = None;
            return;
        };
        let tint_rgb = if !*tint {
            [0, 0, 0]
        } else if self.theme == Theme::Dark {
            [255, 255, 255]
        } else {
            [18, 18, 23]
        };
        let key = (path.clone(), *tint, tint_rgb);
        if self
            .icon_bitmap
            .as_ref()
            .is_some_and(|icon| icon.key == key)
        {
            return;
        }
        let side = (AGENT_ICON_SIZE * dpi as f32 / 96.0 * 2.0).round().max(2.0) as u32;
        let Some((width, height, pixels)) = rasterize_icon(path, *tint, tint_rgb, side) else {
            self.icon_bitmap = None;
            return;
        };
        self.icon_bitmap = Some(IconBitmap {
            key,
            width,
            height,
            pixels,
        });
    }

    fn draw_icon(&self, rt: &ID2D1DCRenderTarget, layout: &ChatLayout) {
        let Some(icon) = self.icon_bitmap.as_ref() else {
            return;
        };
        let properties = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        let size = windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U {
            width: icon.width,
            height: icon.height,
        };
        // SAFETY: the pixel vec holds `width * height * 4` bytes and D2D copies
        // them during CreateBitmap. The bitmap belongs to this paint's render
        // target and is released with it before the next target can replace it.
        let bitmap = unsafe {
            match rt.CreateBitmap(
                size,
                Some(icon.pixels.as_ptr().cast()),
                icon.width * 4,
                &properties,
            ) {
                Ok(bitmap) => bitmap,
                Err(error) => {
                    eprintln!("herdr-pet: cannot upload the agent icon: {error}");
                    return;
                }
            }
        };
        let title_y = layout.bubble.y as f32 + TITLE_INSET_TOP;
        let left = layout.bubble.x as f32 + TEXT_INSET_X;
        let top = title_y + (TEXT_LINE_HEIGHT - AGENT_ICON_SIZE) / 2.0;
        let dest = D2D_RECT_F {
            left,
            top,
            right: left + AGENT_ICON_SIZE,
            bottom: top + AGENT_ICON_SIZE,
        };
        // SAFETY: both objects belong to this render target's thread.
        unsafe {
            rt.DrawBitmap(
                &bitmap,
                Some(&dest as *const D2D_RECT_F),
                1.0,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            );
        }
    }

    fn draw_text(
        &self,
        rt: &ID2D1DCRenderTarget,
        text: &str,
        format: &IDWriteTextFormat,
        origin: (f32, f32),
        width: f32,
        brush: &ID2D1SolidColorBrush,
    ) {
        if text.is_empty() || width <= 1.0 {
            return;
        }
        let utf16: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: the DWrite objects live on this thread; the layout is
        // released when `layout` drops.
        unsafe {
            let Ok(layout) = self
                .dwrite
                .CreateTextLayout(&utf16, format, width, TEXT_LINE_HEIGHT)
            else {
                return;
            };
            let _ = layout.SetTrimming(
                &DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    delimiter: 0,
                    delimiterCount: 0,
                },
                &self.ellipsis,
            );
            rt.DrawTextLayout(
                Vector2 {
                    X: origin.0,
                    Y: origin.1,
                },
                &layout,
                brush_ref(brush),
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }
}

fn make_text_format(
    dwrite: &IDWriteFactory,
    weight: windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT,
) -> Option<IDWriteTextFormat> {
    // SAFETY: the family name is a static wide string and the factory is
    // owned by the overlay.
    unsafe {
        let format = dwrite
            .CreateTextFormat(
                w!("Microsoft YaHei UI"),
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                TEXT_SIZE,
                w!("zh-cn"),
            )
            .ok()?;
        let _ = format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
        let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        Some(format)
    }
}

fn brush(
    rt: &ID2D1DCRenderTarget,
    color: D2D1_COLOR_F,
) -> windows::core::Result<ID2D1SolidColorBrush> {
    // SAFETY: plain-color brush creation on the target's thread.
    unsafe { rt.CreateSolidColorBrush(&color, None) }
}

fn brush_ref(brush: &ID2D1SolidColorBrush) -> &ID2D1Brush {
    brush
}

fn rect_f(rect: Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: rect.x as f32,
        top: rect.y as f32,
        right: (rect.x + rect.w) as f32,
        bottom: (rect.y + rect.h) as f32,
    }
}

fn draw_chevron(
    rt: &ID2D1DCRenderTarget,
    center: (f64, f64),
    direction: f32,
    brush: &ID2D1SolidColorBrush,
    style: &ID2D1StrokeStyle,
) {
    let (cx, cy) = (center.0 as f32, center.1 as f32);
    let reach = ARROW_SYMBOL_SIZE / 2.0;
    let half = reach * 0.55;
    let tip = (cx + direction * reach * 0.4, cy);
    // SAFETY: plain geometry on the current render target.
    unsafe {
        rt.DrawLine(
            Vector2 {
                X: cx - direction * reach * 0.2,
                Y: cy - half,
            },
            Vector2 { X: tip.0, Y: tip.1 },
            brush_ref(brush),
            1.6,
            style,
        );
        rt.DrawLine(
            Vector2 { X: tip.0, Y: tip.1 },
            Vector2 {
                X: cx - direction * reach * 0.2,
                Y: cy + half,
            },
            brush_ref(brush),
            1.6,
            style,
        );
    }
}

fn draw_toggle_chevron(
    rt: &ID2D1DCRenderTarget,
    center: (f64, f64),
    down: bool,
    brush: &ID2D1SolidColorBrush,
    style: &ID2D1StrokeStyle,
) {
    let (cx, cy) = (center.0 as f32, center.1 as f32);
    let half = CHEVRON_SIZE / 2.0;
    let offset = if down { -half * 0.35 } else { half * 0.35 };
    let tip_y = cy - offset;
    let wing_y = cy + offset;
    // SAFETY: plain geometry on the current render target.
    unsafe {
        rt.DrawLine(
            Vector2 {
                X: cx - half * 0.55,
                Y: wing_y,
            },
            Vector2 { X: cx, Y: tip_y },
            brush_ref(brush),
            1.7,
            style,
        );
        rt.DrawLine(
            Vector2 { X: cx, Y: tip_y },
            Vector2 {
                X: cx + half * 0.55,
                Y: wing_y,
            },
            brush_ref(brush),
            1.7,
            style,
        );
    }
}

/// State glyph in the subtitle column. Working draws a rotating dotted
/// spinner; other states use simple vector marks that need no icon font.
fn draw_status(
    rt: &ID2D1DCRenderTarget,
    center: (f32, f32),
    status: Option<AgentStatus>,
    color_brush: &ID2D1SolidColorBrush,
    muted: &ID2D1SolidColorBrush,
    style: &ID2D1StrokeStyle,
    elapsed: Duration,
) {
    let (cx, cy) = center;
    // SAFETY: drawing primitives on the current target.
    unsafe {
        match status {
            Some(AgentStatus::Working) => {
                let turns = elapsed.as_secs_f64() * 0.9;
                let phase = (turns.fract() * std::f64::consts::TAU) as f32;
                let radius = 5.2;
                for index in 0..10 {
                    let angle = phase + index as f32 * std::f32::consts::TAU / 10.0;
                    // A growing dot is the spinner head; alpha would need a
                    // second brush per step for the same effect.
                    let dot_radius = 0.55 + 0.75 * ((index as f32 + 1.0) / 10.0).powi(2);
                    let dot = D2D1_ELLIPSE {
                        point: Vector2 {
                            X: cx + angle.cos() * radius,
                            Y: cy + angle.sin() * radius,
                        },
                        radiusX: dot_radius,
                        radiusY: dot_radius,
                    };
                    rt.FillEllipse(&dot, brush_ref(color_brush));
                }
            }
            Some(AgentStatus::Done) => {
                rt.DrawLine(
                    Vector2 { X: cx - 5.0, Y: cy },
                    Vector2 {
                        X: cx - 1.0,
                        Y: cy + 4.0,
                    },
                    brush_ref(color_brush),
                    1.8,
                    style,
                );
                rt.DrawLine(
                    Vector2 {
                        X: cx - 1.0,
                        Y: cy + 4.0,
                    },
                    Vector2 {
                        X: cx + 5.0,
                        Y: cy - 4.0,
                    },
                    brush_ref(color_brush),
                    1.8,
                    style,
                );
            }
            Some(AgentStatus::Error) => {
                rt.DrawLine(
                    Vector2 {
                        X: cx - 4.0,
                        Y: cy - 4.0,
                    },
                    Vector2 {
                        X: cx + 4.0,
                        Y: cy + 4.0,
                    },
                    brush_ref(color_brush),
                    1.8,
                    style,
                );
                rt.DrawLine(
                    Vector2 {
                        X: cx + 4.0,
                        Y: cy - 4.0,
                    },
                    Vector2 {
                        X: cx - 4.0,
                        Y: cy + 4.0,
                    },
                    brush_ref(color_brush),
                    1.8,
                    style,
                );
            }
            Some(AgentStatus::Blocked) => {
                rt.DrawLine(
                    Vector2 { X: cx, Y: cy - 5.5 },
                    Vector2 { X: cx, Y: cy + 1.5 },
                    brush_ref(color_brush),
                    2.0,
                    style,
                );
                let dot = D2D1_ELLIPSE {
                    point: Vector2 { X: cx, Y: cy + 4.8 },
                    radiusX: 1.2,
                    radiusY: 1.2,
                };
                rt.FillEllipse(&dot, brush_ref(color_brush));
            }
            Some(AgentStatus::Idle) => {
                // A small "Z" reads like the moon glyph without needing a
                // symbol font or a cut-out mask.
                rt.DrawLine(
                    Vector2 {
                        X: cx - 4.0,
                        Y: cy - 4.5,
                    },
                    Vector2 {
                        X: cx + 4.0,
                        Y: cy - 4.5,
                    },
                    brush_ref(color_brush),
                    1.5,
                    style,
                );
                rt.DrawLine(
                    Vector2 {
                        X: cx + 4.0,
                        Y: cy - 4.5,
                    },
                    Vector2 {
                        X: cx - 4.0,
                        Y: cy + 4.5,
                    },
                    brush_ref(color_brush),
                    1.5,
                    style,
                );
                rt.DrawLine(
                    Vector2 {
                        X: cx - 4.0,
                        Y: cy + 4.5,
                    },
                    Vector2 {
                        X: cx + 4.0,
                        Y: cy + 4.5,
                    },
                    brush_ref(color_brush),
                    1.5,
                    style,
                );
            }
            None => {
                for x in [-4.5f32, 0.0, 4.5] {
                    let dot = D2D1_ELLIPSE {
                        point: Vector2 { X: cx + x, Y: cy },
                        radiusX: 1.1,
                        radiusY: 1.1,
                    };
                    rt.FillEllipse(&dot, brush_ref(muted));
                }
            }
        }
    }
}

/// Rasterize a Herdr SVG icon. Tinted icons take the theme's text color with
/// the SVG's own alpha, matching Herdr's one-color glyphs.
fn rasterize_icon(
    path: &Path,
    tint: bool,
    tint_rgb: [u8; 3],
    side: u32,
) -> Option<(u32, u32, Vec<u8>)> {
    let data = std::fs::read(path).ok()?;
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).ok()?;
    let size = tree.size();
    if size.width() <= 0.0 || size.height() <= 0.0 {
        return None;
    }
    let mut pixmap = tiny_skia::Pixmap::new(side, side)?;
    let scale = (side as f32 / size.width()).min(side as f32 / size.height());
    let dx = (side as f32 - size.width() * scale) / 2.0;
    let dy = (side as f32 - size.height() * scale) / 2.0;
    let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(dx, dy);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let mut pixels = pixmap.take();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        if tint {
            // tiny-skia stores premultiplied RGBA; replace the color while
            // keeping the coverage-derived alpha.
            pixel[0] = (u32::from(tint_rgb[0]) * alpha / 255) as u8;
            pixel[1] = (u32::from(tint_rgb[1]) * alpha / 255) as u8;
            pixel[2] = (u32::from(tint_rgb[2]) * alpha / 255) as u8;
        }
        pixel.swap(0, 2);
    }
    Some((side, side, pixels))
}

fn current_theme() -> Theme {
    let subkey = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    let value = w!("AppsUseLightTheme");
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: the key is opened read-only and closed below; `data` has room
    // for the DWORD the value stores.
    unsafe {
        let mut key = HKEY_CURRENT_USER;
        if RegOpenKeyExW(HKEY_CURRENT_USER, subkey, None, KEY_QUERY_VALUE, &mut key).0 != 0 {
            return Theme::Dark;
        }
        let status = RegGetValueW(
            HKEY_CURRENT_USER,
            subkey,
            value,
            RRF_RT_REG_DWORD,
            None,
            Some((&mut data as *mut u32).cast()),
            Some(&mut size),
        );
        let _ = RegCloseKey(key);
        if status.0 == 0 && data != 0 {
            Theme::Light
        } else {
            Theme::Dark
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_keeps_text_opaque_and_scales_glass() {
        let opaque = Palette::new(Theme::Dark, 1.0);
        let clear = Palette::new(Theme::Dark, 0.0);
        assert_eq!(opaque.text.a, 0.95);
        assert_eq!(clear.text.a, 0.95);
        assert_eq!(clear.glass.a, 0.0);
        assert_eq!(clear.border.a, 0.0);
        assert!(opaque.glass.a > 0.5);
    }

    #[test]
    fn icon_rasterization_covers_the_requested_side() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect width="24" height="24" fill="#ff0000"/></svg>"##;
        let dir = std::env::temp_dir().join(format!("herdr-pet-icon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.svg");
        std::fs::write(&path, svg).unwrap();
        let (width, height, pixels) = rasterize_icon(&path, true, [10, 20, 30], 14).unwrap();
        assert_eq!((width, height), (14, 14));
        assert_eq!(pixels.len(), 14 * 14 * 4);
        // The tint replaces the red source color and BGRA stores blue first.
        let center = (7 * 14 + 7) * 4;
        assert_eq!(&pixels[center..center + 4], &[30, 20, 10, 255]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
