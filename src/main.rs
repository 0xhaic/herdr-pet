// Release builds are GUI apps with no console; debug builds keep stderr so
// errors stay visible while developing on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod chat;
mod config;
mod herdr;
mod herdr_app;
mod notify;
mod pet;
mod platform;
mod render;
mod status;
mod tray;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::monitor::MonitorHandle;
use winit::window::{Window, WindowId, WindowLevel};

use crate::chat::{ChatLayout, ChatText, Rect, chat_text};
use crate::config::Config;
use crate::herdr::HerdrMessage;
use crate::herdr_app::Attention;
use crate::pet::{AnimInput, Animator, DragDir, Frame, GazeCell, Mood, Pet, gaze_cell};
use crate::platform::{PanelAction, SettingsPanel};
use crate::status::{AgentStatus, StatusTable, TerminalAgent};
use crate::tray::Action;

const USAGE: &str = "usage: herdr-pet [--pet <name|path>] [--list-pets]";
const PET_HEIGHT: f64 = 104.0;
const EDGE_MARGIN: f64 = 24.0;
const TICK_INTERVAL: Duration = Duration::from_millis(16);
/// Live slider changes apply immediately but hit the disk only after the
/// user stops moving, so dragging does not rewrite the config every tick.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);
const HIDDEN_INTERVAL: Duration = Duration::from_millis(250);
const CLICK_ALPHA: u8 = 16;
/// Movement under this many logical points counts as a click, not a drag.
const CLICK_SLOP: f64 = 3.0;
/// Extra room above the pet needed before a flipped bubble returns to its
/// default position, so hovering at the threshold does not flap it.
const BUBBLE_RESTORE_MARGIN: f64 = 4.0;
/// NSWorkspace is cheap, but 60 Hz is unnecessary for an app-level state.
const FRONT_CHECK_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, PartialEq, Eq)]
enum Cli {
    Run { pet: Option<String> },
    ListPets,
}

fn main() -> Result<()> {
    let config = config::load();
    match parse_cli(std::env::args().skip(1))? {
        Cli::ListPets => list_pets(&config),
        Cli::Run { pet } => run(pet, config),
    }
}

fn parse_cli(mut args: impl Iterator<Item = String>) -> Result<Cli> {
    let mut pet = None;
    let mut list = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--pet" => {
                if pet.is_some() {
                    bail!("--pet given more than once\n{USAGE}");
                }
                pet = Some(args.next().context("--pet requires a name or path")?);
            }
            "--list-pets" => list = true,
            other => bail!("unknown argument {other:?}\n{USAGE}"),
        }
    }
    if list && pet.is_some() {
        bail!("--list-pets cannot be combined with --pet\n{USAGE}");
    }
    Ok(if list {
        Cli::ListPets
    } else {
        Cli::Run { pet }
    })
}

fn list_pets(config: &Config) -> Result<()> {
    let base = config.pets_dir.as_deref();
    let dirs = pet::pet_dirs_in(base)?;
    if dirs.is_empty() {
        println!("no pets in {}", pet::pets_base(base)?.display());
        return Ok(());
    }
    for dir in dirs {
        let name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<non-utf8>");
        match pet::load_manifest(&dir) {
            Ok(manifest) => println!("{name}\t{}", manifest.display_name),
            Err(error) => println!("{name}\t(unreadable: {error:#})"),
        }
    }
    Ok(())
}

fn run(pet_spec: Option<String>, config: Config) -> Result<()> {
    let pet = open_pet(pet_spec.as_deref(), &config)?;
    eprintln!(
        "herdr-pet: loaded {} ({}) from {}",
        pet.manifest.display_name,
        pet.manifest.id,
        pet.dir.display()
    );

    let (sender, receiver) = channel();
    herdr::spawn(sender);
    let (attention_sender, attention) = channel();
    herdr_app::spawn_attention(attention_sender);

    let mut builder = EventLoop::builder();
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder.with_activation_policy(ActivationPolicy::Accessory);
        // A desktop companion must not steal focus when it starts. Stage
        // Manager would otherwise consume the first desktop click to dismiss
        // the pet instead of revealing the most recently used app.
        builder.with_activate_ignoring_other_apps(false);
    }
    let event_loop = builder.build().context("creating the event loop")?;
    let mut app = App::new(pet, receiver, attention, config);
    event_loop
        .run_app(&mut app)
        .context("running the event loop")
}

fn open_pet(cli_pet: Option<&str>, config: &Config) -> Result<Pet> {
    let base = config.pets_dir.as_deref();
    if let Some(spec) = cli_pet {
        return open_pet_spec(spec, base);
    }
    if let Some(spec) = config.pet.as_deref().filter(|spec| !spec.is_empty()) {
        match open_pet_spec(spec, base) {
            Ok(pet) => return Ok(pet),
            Err(error) => {
                eprintln!(
                    "herdr-pet: configured pet {spec:?} unavailable ({error:#}); falling back"
                );
            }
        }
    }
    pet::load_first_pet_in(base)
}

fn open_pet_spec(spec: &str, base: Option<&Path>) -> Result<Pet> {
    let direct = PathBuf::from(spec);
    if direct.join("pet.json").is_file() {
        return pet::load_pet(&direct);
    }
    let candidate = pet::pets_base(base)?.join(spec);
    if candidate.join("pet.json").is_file() {
        return pet::load_pet(&candidate);
    }
    bail!(
        "pet {spec:?} not found (tried {} and {})",
        direct.display(),
        candidate.display()
    )
}

fn pet_display_name(dir: &Path) -> String {
    pet::load_manifest(dir)
        .map(|manifest| manifest.display_name)
        .unwrap_or_else(|_| {
            dir.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("?")
                .to_owned()
        })
}

fn pet_spec(pet: &Pet, base: Option<&Path>) -> String {
    if let Ok(pets_dir) = pet::pets_base(base)
        && pet.dir.parent() == Some(pets_dir.as_path())
        && let Some(name) = pet.dir.file_name().and_then(|name| name.to_str())
    {
        return name.to_owned();
    }
    pet.dir.display().to_string()
}

fn clamp_size(size: f64) -> f64 {
    if size.is_finite() {
        size.clamp(config::MIN_SIZE, config::MAX_SIZE)
    } else {
        PET_HEIGHT
    }
}

fn clamp_chat_opacity(opacity: f64) -> f64 {
    if opacity.is_finite() {
        opacity.clamp(0.0, 1.0)
    } else {
        config::DEFAULT_CHAT_OPACITY
    }
}

/// `unseen_done` reports whether any live done agent is still unviewed in
/// Herdr; unseen Done stands in the review loop, seen Done sits.
fn mood_for(status: Option<AgentStatus>, unseen_done: bool) -> Mood {
    match status {
        Some(AgentStatus::Blocked) => Mood::Blocked,
        Some(AgentStatus::Error) => Mood::Failed,
        Some(AgentStatus::Done) if unseen_done => Mood::Review,
        Some(AgentStatus::Done) => Mood::Done,
        Some(AgentStatus::Working) => Mood::Working,
        Some(AgentStatus::Idle) | None => Mood::Idle,
    }
}

#[derive(Debug, Clone)]
struct Drag {
    start_mouse: (f64, f64),
    last_mouse: (f64, f64),
    /// Pet top-left on screen when the drag started, in physical pixels. The
    /// pet, not the window, follows the pointer so bubble flips and slides
    /// leave it under the cursor.
    start_pet: (f64, f64),
    dir: DragDir,
    /// Usable screen areas captured at drag start; the pet may cross them.
    /// They are physical pixels, so a DPI change mid-drag does not move them.
    areas: Vec<Rect>,
}

/// Usable area of `monitor`: the screen with its top edge below the menu bar,
/// or the platform-reported work area (Windows excludes the taskbar). The pet
/// is clamped to it; the bubble is placed inside it where possible.
fn screen_area(monitor: &MonitorHandle) -> Rect {
    if let Some(area) = platform::work_area(monitor) {
        return area;
    }
    let origin = monitor.position();
    let size = monitor.size();
    let top = f64::from(origin.y) + platform::menu_bar_height(monitor);
    Rect {
        x: f64::from(origin.x),
        y: top,
        w: f64::from(size.width),
        h: (f64::from(origin.y) + f64::from(size.height) - top).max(0.0),
    }
}

/// Clamp a pet sprite rectangle, in physical pixels, so it stays on screen. A
/// sprite fully covered by the screen areas keeps its position, so dragging
/// across adjacent screens is smooth; otherwise it is clamped into the screen
/// holding its centre.
fn clamp_pet_to_screens(pet: Rect, areas: &[Rect]) -> Rect {
    if areas.is_empty() || covered_by_any(pet, areas) {
        return pet;
    }
    let Some(area) = screen_for(pet, areas) else {
        return pet;
    };
    Rect {
        x: clamp_axis(pet.x, area.x, area.x + area.w - pet.w),
        y: clamp_axis(pet.y, area.y, area.y + area.h - pet.h),
        ..pet
    }
}

/// Where to put the bubble around `pet` so it stays on screen. The bubble
/// prefers its default above the pet; when the top cannot fit it, it flips
/// below the button, and it slides sideways just enough to clear the screen
/// edges. `current_below` adds a small margin before flipping back, so a drag
/// hovering at the top threshold does not flap between the two rows.
fn bubble_placement(
    pet: Rect,
    scale: f64,
    areas: &[Rect],
    current_below: bool,
) -> chat::BubblePlacement {
    if areas.is_empty() || !scale.is_finite() || scale <= 0.0 {
        return chat::BubblePlacement::default();
    }
    let Some(area) = screen_for(pet, areas) else {
        return chat::BubblePlacement::default();
    };
    let above_need = chat::BUBBLE_ABOVE_SPACE * scale;
    let below_need = chat::BUBBLE_BELOW_SPACE * scale;
    let above_space = pet.y - area.y;
    let below_space = area.y + area.h - (pet.y + pet.h);
    let restore = if current_below {
        BUBBLE_RESTORE_MARGIN * scale
    } else {
        0.0
    };
    let below = if above_space >= above_need + restore {
        false
    } else if below_space >= below_need {
        true
    } else {
        // Neither row fits; take the one that overflows least.
        below_space - below_need > above_space - above_need
    };

    let bubble_w = chat::BUBBLE_WIDTH * scale;
    let bubble_h = chat::BUBBLE_HEIGHT * scale;
    let bubble_y = if below {
        pet.y + pet.h + chat::BUBBLE_BELOW_SPACE * scale - bubble_h
    } else {
        pet.y - chat::BUBBLE_ABOVE_SPACE * scale
    };
    let centered = pet.x + (pet.w - bubble_w) / 2.0;
    let candidate = Rect {
        x: centered,
        y: bubble_y,
        w: bubble_w,
        h: bubble_h,
    };
    let shift = if covered_by_any(candidate, areas) {
        0.0
    } else {
        clamp_axis(centered, area.x, area.x + area.w - bubble_w) - centered
    };
    chat::BubblePlacement {
        below,
        shift_x: shift / scale,
    }
}

/// Whether every point of `rect` lies inside the union of `areas`.
fn covered_by_any(rect: Rect, areas: &[Rect]) -> bool {
    let mut uncovered = vec![rect];
    for area in areas {
        uncovered = uncovered
            .into_iter()
            .flat_map(|piece| subtract_rect(piece, *area))
            .collect();
        if uncovered.is_empty() {
            return true;
        }
    }
    false
}

/// `rect` minus `hole`, split into at most four rectangles.
fn subtract_rect(rect: Rect, hole: Rect) -> Vec<Rect> {
    let x0 = rect.x.max(hole.x);
    let y0 = rect.y.max(hole.y);
    let x1 = (rect.x + rect.w).min(hole.x + hole.w);
    let y1 = (rect.y + rect.h).min(hole.y + hole.h);
    if x0 >= x1 || y0 >= y1 {
        return vec![rect];
    }
    let mut parts = Vec::new();
    if rect.y < y0 {
        parts.push(Rect {
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: y0 - rect.y,
        });
    }
    if y1 < rect.y + rect.h {
        parts.push(Rect {
            x: rect.x,
            y: y1,
            w: rect.w,
            h: rect.y + rect.h - y1,
        });
    }
    if rect.x < x0 {
        parts.push(Rect {
            x: rect.x,
            y: y0,
            w: x0 - rect.x,
            h: y1 - y0,
        });
    }
    if x1 < rect.x + rect.w {
        parts.push(Rect {
            x: x1,
            y: y0,
            w: rect.x + rect.w - x1,
            h: y1 - y0,
        });
    }
    parts
}

/// Screen area holding the pet's centre, else the one it overlaps most,
/// else the nearest.
fn screen_for(pet: Rect, areas: &[Rect]) -> Option<Rect> {
    let center = pet.center();
    if let Some(area) = areas.iter().find(|area| area.contains(center.0, center.1)) {
        return Some(*area);
    }
    let best = areas
        .iter()
        .copied()
        .max_by(|a, b| overlap_area(pet, *a).total_cmp(&overlap_area(pet, *b)))?;
    if overlap_area(pet, best) > 0.0 {
        return Some(best);
    }
    areas
        .iter()
        .copied()
        .min_by(|a, b| distance_sq(pet, *a).total_cmp(&distance_sq(pet, *b)))
}

/// Overlapping area of two rectangles.
fn overlap_area(a: Rect, b: Rect) -> f64 {
    let w = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
    let h = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
    if w > 0.0 && h > 0.0 { w * h } else { 0.0 }
}

/// Squared distance between two rectangles (0 when they touch or overlap).
fn distance_sq(a: Rect, b: Rect) -> f64 {
    let dx = (b.x - (a.x + a.w)).max(a.x - (b.x + b.w)).max(0.0);
    let dy = (b.y - (a.y + a.h)).max(a.y - (b.y + b.h)).max(0.0);
    dx * dx + dy * dy
}

/// Clamp to an inclusive range, pinning to `min` when the range is empty.
fn clamp_axis(value: f64, min: f64, max: f64) -> f64 {
    if min <= max {
        value.clamp(min, max)
    } else {
        min
    }
}

fn step_dir(current: DragDir, dx: f64) -> DragDir {
    if dx > 0.0 {
        DragDir::Right
    } else if dx < 0.0 {
        DragDir::Left
    } else {
        current
    }
}

fn within_click_slop(start: (f64, f64), end: (f64, f64)) -> bool {
    (end.0 - start.0).hypot(end.1 - start.1) <= CLICK_SLOP
}

fn chat_ui_available(is_active: bool, herdr_frontmost: bool) -> bool {
    is_active && !herdr_frontmost
}

/// The menu-bar icon hides only while the setting is on, click-through is
/// off, and the pet is visible, so a mouse-reachable entry point remains.
fn tray_icon_hidden(hide_tray_icon: bool, click_through: bool, pet_visible: bool) -> bool {
    hide_tray_icon && !click_through && pet_visible
}

/// Blocked/error agents and unseen done force the bubble open regardless of
/// Herdr being frontmost or the user having collapsed it.
fn chat_bubble_visible(chat_enabled: bool, chat_open: bool, forced: bool) -> bool {
    forced || (chat_enabled && chat_open)
}

/// Index of the manually picked agent among the bubble candidates, or 0 (the
/// automatic focus) when the pick is absent.
fn selected_index(candidates: &[&TerminalAgent], selected: Option<&str>) -> usize {
    selected
        .and_then(|address| candidates.iter().position(|agent| agent.address == address))
        .unwrap_or(0)
}

/// Step through a non-empty list, wrapping at both ends.
fn wrap_index(current: usize, step: isize, len: usize) -> usize {
    (current as isize + step).rem_euclid(len as isize) as usize
}

#[derive(Debug, Clone, Copy)]
struct WindowSnapshot {
    origin: PhysicalPosition<i32>,
    size: winit::dpi::PhysicalSize<u32>,
    scale: f64,
}

/// Inputs the Windows hit region was last built from. Rebuilding scans the
/// atlas cell, so it only happens when the frame, pet rect, overlay rects or
/// scale changes.
#[derive(Debug, Clone, Copy, PartialEq)]
struct HitRegionKey {
    frame: Frame,
    scale: u64,
    pet: (u64, u64, u64, u64),
    bubble: Option<(u64, u64, u64, u64)>,
    button: Option<(u64, u64, u64, u64)>,
}

fn rect_bits(rect: Rect) -> (u64, u64, u64, u64) {
    (
        rect.x.to_bits(),
        rect.y.to_bits(),
        rect.w.to_bits(),
        rect.h.to_bits(),
    )
}

impl HitRegionKey {
    fn new(
        frame: Frame,
        pet: Rect,
        bubble: Option<Rect>,
        button: Option<Rect>,
        scale: f64,
    ) -> Self {
        Self {
            frame,
            scale: scale.to_bits(),
            pet: rect_bits(pet),
            bubble: bubble.map(rect_bits),
            button: button.map(rect_bits),
        }
    }
}

impl WindowSnapshot {
    fn new(window: &Window) -> Option<Self> {
        Some(Self {
            origin: window.outer_position().ok()?,
            size: window.inner_size(),
            scale: window.scale_factor(),
        })
    }

    fn local_point(self, pointer: (f64, f64)) -> (f64, f64) {
        (
            (pointer.0 - f64::from(self.origin.x)) / self.scale,
            (pointer.1 - f64::from(self.origin.y)) / self.scale,
        )
    }
}

struct App {
    pet: Pet,
    window: Option<Arc<Window>>,
    renderer: Option<render::Renderer>,
    table: StatusTable,
    animator: Animator,
    messages: Receiver<HerdrMessage>,
    attention: Receiver<Attention>,
    herdr_attention: Attention,
    /// Cached NSWorkspace frontmost result; checked a few times per second.
    herdr_frontmost: bool,
    next_front_check: Instant,
    /// Whether the chat UI should exist: an active agent and Herdr not
    /// frontmost. When false, the bubble and the button are hidden unless
    /// `chat_forced` keeps the bubble open, and their rects stop taking
    /// clicks.
    chat_enabled: bool,
    /// A live agent is blocked/errored or done and unseen: the bubble stays
    /// open even if Herdr is frontmost or the user collapsed it, until the
    /// agent is handled or the done tab is viewed. The button still follows
    /// `chat_enabled`.
    chat_forced: bool,
    /// Address of the agent the user picked with the bubble's arrows. Kept
    /// while that agent still needs the bubble; otherwise the automatic focus
    /// order applies again.
    chat_selected: Option<String>,
    /// Pet press origin, kept so a release without movement opens Herdr.
    click_candidate: Option<(f64, f64)>,
    started: Instant,
    next_tick: Instant,
    /// UV rect of the frame the next redraw should show.
    pending_uv: [f32; 4],
    /// UV rect last handed to the renderer; redraw only when it changes.
    presented_uv: Option<[f32; 4]>,
    /// Force the next redraw even if `pending_uv` matches (resize, new atlas,
    /// re-shown window, or a failed present).
    force_redraw: bool,
    config: Config,
    dragging: Option<Drag>,
    cursor_hittest: Option<bool>,
    primary_height: f64,
    primary_scale: f64,
    tray: Option<tray::Tray>,
    panel: Option<SettingsPanel>,
    /// When to persist live slider changes after the user stops moving.
    save_due: Option<Instant>,
    pets: Vec<PathBuf>,
    pet_index: usize,
    chat_layout: ChatLayout,
    mood: Option<AgentStatus>,
    last_anim_input: Option<AnimInput>,
    next_anim_ms: u64,
    frame: Frame,
    overlay: Option<platform::ChatOverlay>,
    /// Window size, scale, and layout the native views were last fitted to.
    overlay_key: Option<(u32, u32, u64, ChatLayout, bool)>,
    last_chat_text: Option<ChatText>,
    /// Frame, pet rect and scale the Windows hit region was last built from.
    hit_region_key: Option<HitRegionKey>,
}

impl App {
    fn new(
        pet: Pet,
        messages: Receiver<HerdrMessage>,
        attention: Receiver<Attention>,
        mut config: Config,
    ) -> Self {
        config.size = clamp_size(config.size);
        config.chat_opacity = clamp_chat_opacity(config.chat_opacity);
        let animator = Animator::new(config.gaze && pet.atlas.gaze_enabled());
        let pending_uv = pet.atlas.cell_uv(0, 0);
        let pets = pet::pet_dirs_in(config.pets_dir.as_deref()).unwrap_or_default();
        let pet_index = pets.iter().position(|dir| dir == &pet.dir).unwrap_or(0);
        let chat_layout = ChatLayout::new(config.size);
        Self {
            pet,
            window: None,
            renderer: None,
            table: StatusTable::new(),
            animator,
            messages,
            attention,
            herdr_attention: Attention::default(),
            herdr_frontmost: false,
            next_front_check: Instant::now(),
            chat_enabled: false,
            chat_forced: false,
            chat_selected: None,
            click_candidate: None,
            started: Instant::now(),
            next_tick: Instant::now(),
            pending_uv,
            presented_uv: None,
            force_redraw: true,
            config,
            dragging: None,
            cursor_hittest: None,
            primary_height: 0.0,
            primary_scale: 1.0,
            tray: None,
            panel: None,
            save_due: None,
            pets,
            pet_index,
            chat_layout,
            mood: None,
            last_anim_input: None,
            next_anim_ms: 0,
            frame: Frame { row: 0, col: 0 },
            overlay: None,
            overlay_key: None,
            last_chat_text: None,
            hit_region_key: None,
        }
    }

    fn drain_messages(&mut self) {
        let mut changes = Vec::new();
        while let Ok(message) = self.messages.try_recv() {
            match message {
                HerdrMessage::Snapshot { terminals, .. } => self.table.apply_snapshot(terminals),
                HerdrMessage::Event(event) => {
                    if let Some(change) = self.table.apply_event(*event) {
                        changes.push(change);
                    }
                }
                HerdrMessage::Disconnected => {}
            }
        }
        self.mood = self.table.aggregate().status;
        self.last_anim_input = None;
        if self.config.notifications && !changes.is_empty() {
            notify::announce(&changes, self.config.notification_sound);
        }
    }

    fn save_config(&mut self) {
        if let Some(window) = &self.window
            && let Ok(position) = window.outer_position()
        {
            // Persist the pet's top-left, not the window's: the bubble may be
            // collapsed or the window resized without moving the pet.
            let scale = window.scale_factor();
            let (origin_x, origin_y) = self.chat_layout.pet_origin();
            self.config.position = Some([
                position.x + (origin_x * scale).round() as i32,
                position.y + (origin_y * scale).round() as i32,
            ]);
        }
        self.config.pet = Some(pet_spec(&self.pet, self.config.pets_dir.as_deref()));
        if let Err(error) = self.config.save() {
            eprintln!("herdr-pet: cannot save config: {error:#}");
        }
    }

    fn pointer(&self) -> Option<(f64, f64)> {
        platform::mouse_position(self.primary_height, self.primary_scale)
    }

    /// Pointer position in window-local logical points, top-left origin.
    fn local_point(&self, pointer: (f64, f64)) -> Option<(f64, f64)> {
        let snapshot = WindowSnapshot::new(self.window.as_ref()?)?;
        Some(snapshot.local_point(pointer))
    }

    fn set_cursor_hittest(&mut self, hittest: bool) {
        if self.cursor_hittest == Some(hittest) {
            return;
        }
        let Some(window) = &self.window else {
            return;
        };
        platform::set_cursor_hittest(window, hittest);
        self.cursor_hittest = Some(hittest);
    }

    /// Whether the menu-bar icon should stay hidden. The setting applies only
    /// while click-through is off, because the pet's right-click menu needs
    /// the mouse, and only while the pet is visible, because a hidden pet
    /// would leave no way to reopen it.
    fn tray_hidden(&self) -> bool {
        let pet_visible = self
            .window
            .as_ref()
            .is_none_or(|window| window.is_visible().unwrap_or(true));
        tray_icon_hidden(
            self.config.hide_tray_icon,
            self.config.click_through,
            pet_visible,
        )
    }

    fn apply_tray_visibility(&mut self) {
        let Some(tray) = &self.tray else {
            return;
        };
        if let Err(error) = tray.set_visible(!self.tray_hidden()) {
            eprintln!("herdr-pet: cannot update the tray icon: {error:#}");
        }
    }

    /// Right press on the pet opens the menu that the hidden menu-bar icon
    /// would otherwise provide.
    fn right_press(&mut self) {
        if !self.tray_hidden() || self.dragging.is_some() {
            return;
        }
        let Some(pointer) = self.pointer() else {
            return;
        };
        let Some((x, y)) = self.local_point(pointer) else {
            return;
        };
        if !self.chat_layout.pet.contains(x, y) {
            return;
        }
        let Some(window) = self.window.clone() else {
            return;
        };
        if let Some(tray) = &self.tray {
            tray.show_pet_menu(&window);
        }
    }

    /// Left press: the button toggles the bubble, the bubble opens Herdr at
    /// the shown session, and only the pet sprite starts a drag.
    fn press(&mut self) {
        if self.config.click_through || self.dragging.is_some() {
            return;
        }
        let Some(pointer) = self.pointer() else {
            return;
        };
        let Some((x, y)) = self.local_point(pointer) else {
            return;
        };
        if self.chat_enabled && self.chat_layout.button.contains(x, y) {
            self.toggle_chat();
            return;
        }
        if self.bubble_visible() && self.chat_layout.bubble.contains(x, y) {
            if self.pager_visible() {
                if self.chat_layout.next_arrow.contains(x, y) {
                    self.cycle_chat_agent(1);
                    return;
                }
                if self.chat_layout.prev_arrow.contains(x, y) {
                    self.cycle_chat_agent(-1);
                    return;
                }
            }
            self.focus_herdr_chat();
            return;
        }
        if self.chat_layout.pet.contains(x, y) {
            self.start_drag();
        }
    }

    fn bubble_visible(&self) -> bool {
        chat_bubble_visible(self.chat_enabled, self.config.chat_open, self.chat_forced)
    }

    fn apply_chat_visibility(&mut self) {
        let bubble = self.bubble_visible();
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.set_visible(bubble, self.chat_enabled);
        }
        self.next_tick = Instant::now();
        self.sync_overlay_bitmap();
    }

    fn drain_attention(&mut self) {
        while let Ok(attention) = self.attention.try_recv() {
            self.herdr_attention = attention;
        }
    }

    fn refresh_herdr_frontmost(&mut self) {
        let now = Instant::now();
        if now < self.next_front_check {
            return;
        }
        self.next_front_check = now + FRONT_CHECK_INTERVAL;
        self.herdr_frontmost = platform::herdr_app_frontmost();
    }

    /// The chat UI exists only while an agent needs attention and Herdr is not
    /// frontmost (or not running). Done agents count until Herdr marks them
    /// seen; blocked/working/error always count. Blocked/error agents and
    /// unseen done also force the bubble open while Herdr is frontmost or the
    /// bubble is collapsed.
    fn update_chat_enabled(&mut self) {
        let seen = &self.herdr_attention.seen;
        let active = self.table.is_active_with(|address| !seen.contains(address));
        let enabled = chat_ui_available(active, self.herdr_frontmost);
        let forced = self.table.has_blocked_or_error()
            || self
                .table
                .has_unseen_done(|address| !seen.contains(address));
        if enabled == self.chat_enabled && forced == self.chat_forced {
            return;
        }
        self.chat_enabled = enabled;
        self.chat_forced = forced;
        self.last_anim_input = None;
        self.apply_chat_visibility();
    }

    fn toggle_chat(&mut self) {
        self.set_chat_open(!self.config.chat_open);
    }

    fn set_chat_open(&mut self, open: bool) {
        self.config.chat_open = open;
        self.save_config();
        self.apply_chat_visibility();
        self.sync_panel();
    }

    /// Agents that need the bubble right now, in the same focus order the
    /// bubble's arrows cycle through.
    fn focus_candidates(&self) -> Vec<&TerminalAgent> {
        let seen = &self.herdr_attention.seen;
        self.table
            .focus_candidates_with(|address| !seen.contains(address))
    }

    /// The agent the bubble shows (and click-to-jump targets): the manual
    /// pick while it still needs the bubble, otherwise the automatic focus.
    fn focus_agent(&self) -> Option<&TerminalAgent> {
        let candidates = self.focus_candidates();
        let index = selected_index(&candidates, self.chat_selected.as_deref());
        if let Some(agent) = candidates.get(index) {
            return Some(agent);
        }
        let seen = &self.herdr_attention.seen;
        self.table.focus_with(|address| !seen.contains(address))
    }

    /// Whether the bubble's arrows should show: the bubble is visible and
    /// more than one agent needs it.
    fn pager_visible(&self) -> bool {
        self.bubble_visible() && self.focus_candidates().len() > 1
    }

    /// Forget a manual pick once its agent stops needing the bubble, so the
    /// bubble falls back to the automatic focus order and the same address
    /// does not silently reappear here later.
    fn reconcile_chat_selection(&mut self) {
        let Some(selected) = self.chat_selected.as_deref() else {
            return;
        };
        let still_needed = self
            .focus_candidates()
            .iter()
            .any(|agent| agent.address == selected);
        if !still_needed {
            self.chat_selected = None;
            self.last_anim_input = None;
        }
    }

    /// Move the bubble to the previous/next agent that needs it and keep that
    /// manual pick until the agent stops needing the bubble.
    fn cycle_chat_agent(&mut self, step: isize) {
        let next = {
            let candidates = self.focus_candidates();
            if candidates.len() < 2 {
                return;
            }
            let current = selected_index(&candidates, self.chat_selected.as_deref());
            let index = wrap_index(current, step, candidates.len());
            candidates[index].address.clone()
        };
        self.chat_selected = Some(next);
        self.sync_chat();
    }

    fn focus_herdr_chat(&self) {
        match self.focus_agent() {
            Some(agent) => herdr_app::focus_terminal(agent.address.clone(), agent.is_sub_agent()),
            None => herdr_app::raise_app_in_background(),
        }
    }

    /// A pet press becomes a click (and opens Herdr) only when the pointer
    /// stayed within `CLICK_SLOP`; anything further is a drag.
    fn finish_click(&mut self, pointer: Option<(f64, f64)>) {
        let Some(start) = self.click_candidate.take() else {
            return;
        };
        let Some(pointer) = pointer else {
            return;
        };
        if within_click_slop(start, pointer) {
            self.focus_herdr_chat();
        }
    }

    fn sync_chat(&mut self) {
        if !self.bubble_visible() {
            return;
        }
        let elapsed_ms = self.started.elapsed().as_millis() as u64;
        let text = chat_text(self.focus_agent(), elapsed_ms);
        if self.last_chat_text.as_ref() == Some(&text) {
            return;
        }
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.set_text(&text.title, &text.subtitle);
            overlay.set_status(text.status);
            let icon = text
                .icon
                .and_then(|icon| herdr_app::icon_path(icon.file).map(|path| (path, icon.tint)));
            overlay.set_icon(icon.as_ref().map(|(path, tint)| (path.as_path(), *tint)));
        }
        self.last_chat_text = Some(text);
    }

    /// Layout for a pet sprite rectangle on screen, in physical pixels. The
    /// pet size always comes from `config.size`; `pet` only carries where it
    /// currently sits.
    fn chat_layout_for_pet(&self, pet: Rect, scale: f64, areas: &[Rect]) -> ChatLayout {
        let placement = bubble_placement(pet, scale, areas, self.chat_layout.placement.below);
        ChatLayout::placed(self.config.size, placement)
    }

    /// Keep the pet anchored on screen while the bubble flips or slides as it
    /// approaches a screen edge. Dragging computes the placement itself from
    /// the pointer target, so this only runs once the pet has settled.
    fn sync_bubble_placement(&mut self) {
        if self.dragging.is_some() {
            return;
        }
        let Some(window) = self.window.clone() else {
            return;
        };
        let Ok(position) = window.outer_position() else {
            return;
        };
        let scale = window.scale_factor();
        let areas: Vec<Rect> = window
            .available_monitors()
            .map(|monitor| screen_area(&monitor))
            .collect();
        if areas.is_empty() {
            return;
        }
        let (origin_x, origin_y) = self.chat_layout.pet_origin();
        let pet = Rect {
            x: f64::from(position.x) + origin_x * scale,
            y: f64::from(position.y) + origin_y * scale,
            w: self.chat_layout.pet.w * scale,
            h: self.chat_layout.pet.h * scale,
        };
        let layout = self.chat_layout_for_pet(pet, scale, &areas);
        if layout == self.chat_layout {
            return;
        }
        self.chat_layout = layout;
        window.set_outer_position(PhysicalPosition::new(
            (pet.x - layout.pet.x * scale).round() as i32,
            (pet.y - layout.pet.y * scale).round() as i32,
        ));
        self.force_redraw = true;
    }

    /// Refresh the logical layout and refit the native views after a resize or
    /// a size change. AppKit frames are in point coordinates.
    fn sync_overlay(&mut self) {
        let Some(window) = &self.window else {
            return;
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        let scale = window.scale_factor();
        let pager = self.pager_visible();
        let key = (
            size.width,
            size.height,
            scale.to_bits(),
            self.chat_layout,
            pager,
        );
        if self.overlay_key == Some(key) {
            return;
        }
        let bubble = self.bubble_visible();
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.layout(&self.chat_layout, f64::from(size.height) / scale, pager);
            overlay.set_visible(bubble, self.chat_enabled);
        }
        self.overlay_key = Some(key);
        self.force_redraw = true;
    }

    /// Upload a freshly rasterized chat overlay (Windows) and request a
    /// redraw. AppKit draws the overlay with native views, so `take_bitmap`
    /// returns None there. While neither the bubble nor the button is shown,
    /// the renderer draws no overlay at all.
    fn sync_overlay_bitmap(&mut self) {
        let visible = self.bubble_visible() || self.chat_enabled;
        let Some(overlay) = self.overlay.as_mut() else {
            return;
        };
        if !visible {
            if let Some(renderer) = &mut self.renderer {
                renderer.set_overlay(None);
            }
            return;
        }
        let Some(bitmap) = overlay.take_bitmap() else {
            return;
        };
        if let Some(renderer) = &mut self.renderer {
            renderer.set_overlay(Some(&bitmap));
        }
        self.force_redraw = true;
    }

    /// Rebuild the Windows click-through region when the visible pet frame or
    /// its window rectangle changes. Other platforms hit-test per pixel in
    /// `update_cursor`.
    fn sync_hit_region(&mut self, frame: Frame) {
        let Some(window) = &self.window else {
            return;
        };
        let scale = window.scale_factor();
        let pet = self.chat_layout.pet;
        // The drawn bubble and toggle also have to be clickable, which on
        // Windows means joining their rectangles to the pet's hit region.
        let drawn = self.overlay.is_some();
        let bubble = (drawn && self.bubble_visible()).then_some(self.chat_layout.bubble);
        let button = (drawn && self.chat_enabled).then_some(self.chat_layout.button);
        let key = HitRegionKey::new(frame, pet, bubble, button, scale);
        if self.hit_region_key == Some(key) {
            return;
        }
        let extras: Vec<Rect> = [bubble, button].into_iter().flatten().collect();
        platform::update_pet_region(
            window,
            &self.pet.atlas,
            frame,
            pet,
            &extras,
            scale,
            CLICK_ALPHA,
        );
        self.hit_region_key = Some(key);
    }

    /// Clip-space quad for the pet sprite inside the taller window.
    fn pet_quad(&self) -> [f32; 4] {
        let Some(window) = &self.window else {
            return [-1.0, 1.0, 1.0, -1.0];
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return [-1.0, 1.0, 1.0, -1.0];
        }
        let scale = window.scale_factor();
        let width = f64::from(size.width);
        let height = f64::from(size.height);
        let pet = self.chat_layout.pet;
        [
            ((pet.x * scale) / width * 2.0 - 1.0) as f32,
            (1.0 - (pet.y * scale) / height * 2.0) as f32,
            (((pet.x + pet.w) * scale) / width * 2.0 - 1.0) as f32,
            (1.0 - ((pet.y + pet.h) * scale) / height * 2.0) as f32,
        ]
    }

    fn start_drag(&mut self) {
        if self.dragging.is_some() || self.config.click_through {
            return;
        }
        let Some(window) = self.window.clone() else {
            return;
        };
        let Some(pointer) = self.pointer() else {
            return;
        };
        if let Ok(position) = window.outer_position() {
            let areas = window
                .available_monitors()
                .map(|monitor| screen_area(&monitor))
                .collect();
            let scale = window.scale_factor();
            let (origin_x, origin_y) = self.chat_layout.pet_origin();
            let start_pet = (
                f64::from(position.x) + origin_x * scale,
                f64::from(position.y) + origin_y * scale,
            );
            self.dragging = Some(Drag {
                start_mouse: pointer,
                last_mouse: pointer,
                start_pet,
                dir: DragDir::Right,
                areas,
            });
            self.click_candidate = Some(pointer);
            self.set_cursor_hittest(true);
        }
    }

    fn update_drag(&mut self, pointer: Option<(f64, f64)>) {
        let Some(mut drag) = self.dragging.clone() else {
            return;
        };
        let Some(pointer) = pointer else {
            return;
        };
        if !platform::left_button_down() {
            self.dragging = None;
            self.finish_click(Some(pointer));
            self.save_config();
            return;
        }
        let dx = pointer.0 - drag.start_mouse.0;
        let dy = pointer.1 - drag.start_mouse.1;
        if !within_click_slop(drag.start_mouse, pointer) {
            self.click_candidate = None;
        }
        drag.dir = step_dir(drag.dir, pointer.0 - drag.last_mouse.0);
        drag.last_mouse = pointer;
        if let Some(window) = self.window.clone() {
            // The window's scale follows the monitor under it, so crossing a
            // mixed-DPI boundary resizes the sprite instead of jumping it.
            let scale = window.scale_factor();
            let target = Rect {
                x: drag.start_pet.0 + dx,
                y: drag.start_pet.1 + dy,
                w: self.chat_layout.pet.w * scale,
                h: self.chat_layout.pet.h * scale,
            };
            let pet = clamp_pet_to_screens(target, &drag.areas);
            let layout = self.chat_layout_for_pet(pet, scale, &drag.areas);
            self.chat_layout = layout;
            window.set_outer_position(PhysicalPosition::new(
                (pet.x - layout.pet.x * scale).round() as i32,
                (pet.y - layout.pet.y * scale).round() as i32,
            ));
        }
        self.dragging = Some(drag);
    }

    fn gaze(&self, snapshot: WindowSnapshot, pointer: Option<(f64, f64)>) -> Option<GazeCell> {
        if self.dragging.is_some() || snapshot.size.width == 0 || snapshot.size.height == 0 {
            return None;
        }
        let pointer = pointer?;
        // Gaze pivots on the pet cell, not the taller window centre.
        let (center_x, center_y) = self.chat_layout.pet.center();
        let center_x = snapshot.origin.x as f64 + center_x * snapshot.scale;
        let center_y = snapshot.origin.y as f64 + center_y * snapshot.scale;
        gaze_cell(
            ((pointer.0 - center_x) / snapshot.scale) as f32,
            ((pointer.1 - center_y) / snapshot.scale) as f32,
        )
    }

    fn hovered(&self, local: Option<(f64, f64)>) -> bool {
        if self.dragging.is_some() {
            return false;
        }
        let Some((x, y)) = local else {
            return false;
        };
        self.chat_layout.pet.contains(x, y)
    }

    fn update_cursor(&mut self, local: Option<(f64, f64)>, frame: Frame) {
        if self.dragging.is_some() {
            self.set_cursor_hittest(true);
            return;
        }
        if self.config.click_through {
            self.set_cursor_hittest(false);
            return;
        }
        let Some((x, y)) = local else {
            return;
        };
        let layout = &self.chat_layout;
        if (self.chat_enabled && layout.button.contains(x, y))
            || (self.bubble_visible() && layout.bubble.contains(x, y))
        {
            self.set_cursor_hittest(true);
            return;
        }
        if !layout.pet.contains(x, y) {
            self.set_cursor_hittest(false);
            return;
        }
        let alpha = self.pet.atlas.alpha_at_view(
            frame.row,
            frame.col,
            x - layout.pet.x,
            y - layout.pet.y,
            layout.pet.w,
            layout.pet.h,
        );
        self.set_cursor_hittest(alpha.is_some_and(|alpha| alpha >= CLICK_ALPHA));
    }

    /// Advance input and animation state on every tick, but only ask for a
    /// redraw when the visible cell actually changes.
    fn tick(&mut self) {
        self.drain_messages();
        self.poll_panel();
        self.flush_pending_save();
        let visible = self
            .window
            .as_ref()
            .is_some_and(|window| window.is_visible().unwrap_or(true));
        if !visible {
            return;
        }

        self.drain_attention();
        self.refresh_herdr_frontmost();
        self.update_chat_enabled();
        self.reconcile_chat_selection();
        let pointer = self.pointer();
        self.update_drag(pointer);
        self.sync_bubble_placement();
        self.sync_overlay();
        // Sample the window after any drag or bubble-flip move so pointer
        // coordinates, gaze and hover all use the current geometry.
        let snapshot = self.window.as_deref().and_then(WindowSnapshot::new);
        let local = snapshot
            .zip(pointer)
            .map(|(snapshot, pointer)| snapshot.local_point(pointer));
        let gaze = snapshot.and_then(|snapshot| self.gaze(snapshot, pointer));
        let hover = self.hovered(local);
        let dragging = self.dragging.as_ref().map(|drag| drag.dir);
        let seen = &self.herdr_attention.seen;
        let unseen_done = self.mood == Some(AgentStatus::Done)
            && self
                .table
                .has_unseen_done(|address| !seen.contains(address));
        let input = AnimInput {
            mood: mood_for(self.mood, unseen_done),
            dragging,
            gaze,
            hover,
        };
        let now_ms = self.started.elapsed().as_millis() as u64;
        if self.last_anim_input != Some(input) || now_ms >= self.next_anim_ms {
            self.frame = self.animator.update(now_ms, input);
            self.next_anim_ms = self.animator.next_deadline_ms(now_ms).unwrap_or(u64::MAX);
            self.last_anim_input = Some(input);
        }
        // The Thinking placeholder animates on its own clock, so refresh the
        // bubble every tick; `sync_chat` only touches the overlay when text
        // changes.
        self.sync_chat();
        self.sync_overlay_bitmap();
        let frame = self.frame;

        self.sync_hit_region(frame);
        self.update_cursor(local, frame);
        let uv = self.pet.atlas.cell_uv(frame.row, frame.col);
        if self.force_redraw || self.presented_uv != Some(uv) {
            self.pending_uv = uv;
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
    }

    fn present(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };
        if !window.is_visible().unwrap_or(true) {
            return;
        }
        let quad = self.pet_quad();
        let Some(renderer) = &mut self.renderer else {
            return;
        };
        let uv = self.pending_uv;
        renderer.set_frame(uv, quad);
        match renderer.render() {
            Ok(()) => {
                self.presented_uv = Some(uv);
                self.force_redraw = false;
            }
            Err(error) => {
                eprintln!("herdr-pet: render error: {error:#}");
                self.force_redraw = true;
            }
        }
    }

    fn handle_tray(&mut self, event_loop: &ActiveEventLoop) {
        for action in tray::poll() {
            match action {
                Action::ToggleVisible => {
                    if let Some(window) = &self.window {
                        let visible = window.is_visible().unwrap_or(true);
                        window.set_visible(!visible);
                        if visible {
                            self.cursor_hittest = None;
                            self.set_cursor_hittest(!self.config.click_through);
                        } else {
                            self.force_redraw = true;
                        }
                        self.next_tick = Instant::now();
                    }
                    self.apply_tray_visibility();
                }
                Action::Settings => self.open_panel(),
                Action::About => platform::show_about(self.window.as_deref()),
                // Windows has no settings panel yet, so the menus own this
                // switch; the tray icon stays visible while it is on.
                #[cfg(target_os = "windows")]
                Action::ToggleClickThrough => {
                    self.set_click_through(!self.config.click_through);
                }
                Action::Quit => {
                    self.save_config();
                    event_loop.exit();
                }
            }
        }
    }

    fn open_panel(&mut self) {
        if self.panel.is_none() {
            let names: Vec<String> = self.pets.iter().map(|dir| pet_display_name(dir)).collect();
            let pets_dir = pet::pets_base(self.config.pets_dir.as_deref()).unwrap_or_default();
            match SettingsPanel::new(
                &names,
                self.pet_index,
                &self.config,
                &pets_dir,
                self.config.pets_dir.is_some(),
            ) {
                Some(panel) => self.panel = Some(panel),
                None => {
                    eprintln!("herdr-pet: settings panel unavailable");
                    return;
                }
            }
        }
        if let Some(panel) = &mut self.panel {
            panel.show();
        }
    }

    fn poll_panel(&mut self) {
        let Some(panel) = &mut self.panel else {
            return;
        };
        let actions = panel.poll();
        for action in actions {
            match action {
                PanelAction::SelectPet(index) => {
                    self.select_pet(index);
                }
                PanelAction::SetSize(height) => self.preview_size(height),
                PanelAction::SetChatOpacity(opacity) => self.set_chat_opacity(opacity),
                PanelAction::SetNotifications(enabled) => self.set_notifications(enabled),
                PanelAction::SetNotificationSound(enabled) => {
                    self.set_notification_sound(enabled);
                }
                PanelAction::SetGaze(enabled) => self.set_gaze(enabled),
                PanelAction::SetClickThrough(enabled) => self.set_click_through(enabled),
                PanelAction::SetHideTrayIcon(enabled) => self.set_hide_tray_icon(enabled),
                PanelAction::SetChatOpen(open) => self.set_chat_open(open),
                PanelAction::SetPetsDir(dir) => self.set_pets_dir(Some(dir)),
                PanelAction::ResetPetsDir => self.set_pets_dir(None),
            }
        }
    }

    fn sync_panel(&mut self) {
        if let Some(panel) = &mut self.panel {
            panel.sync(&self.config, self.pet_index);
        }
    }

    fn sync_panel_pets(&mut self) {
        let names: Vec<String> = self.pets.iter().map(|dir| pet_display_name(dir)).collect();
        let pets_dir = pet::pets_base(self.config.pets_dir.as_deref()).unwrap_or_default();
        if let Some(panel) = &mut self.panel {
            panel.sync_pets(
                &names,
                self.pet_index,
                &pets_dir,
                self.config.pets_dir.is_some(),
            );
        }
    }

    fn schedule_save(&mut self) {
        self.save_due = Some(Instant::now() + SAVE_DEBOUNCE);
    }

    fn flush_pending_save(&mut self) {
        if self.save_due.is_some_and(|due| Instant::now() >= due) {
            self.save_due = None;
            self.save_config();
        }
    }

    fn set_notifications(&mut self, enabled: bool) {
        self.config.notifications = enabled;
        self.save_config();
        self.sync_panel();
    }

    fn set_notification_sound(&mut self, enabled: bool) {
        self.config.notification_sound = enabled;
        self.save_config();
        self.sync_panel();
    }

    fn set_gaze(&mut self, enabled: bool) {
        self.config.gaze = enabled;
        self.animator
            .set_look_enabled(enabled && self.pet.atlas.gaze_enabled());
        self.last_anim_input = None;
        self.save_config();
        self.sync_panel();
    }

    fn set_click_through(&mut self, enabled: bool) {
        self.config.click_through = enabled;
        if enabled {
            self.dragging = None;
        }
        if let Some(window) = &self.window {
            platform::set_click_through(window, enabled);
        }
        // Let the next tick reapply the per-pixel state on macOS.
        self.cursor_hittest = None;
        if let Some(tray) = &self.tray {
            tray.set_click_through(enabled);
        }
        self.apply_tray_visibility();
        self.save_config();
        self.sync_panel();
    }

    fn set_hide_tray_icon(&mut self, enabled: bool) {
        self.config.hide_tray_icon = enabled;
        self.apply_tray_visibility();
        self.save_config();
        self.sync_panel();
    }

    /// Apply a slider drag immediately; the disk write waits for the debounce.
    fn preview_size(&mut self, height: f64) {
        self.apply_size(height);
        self.sync_panel();
        self.schedule_save();
    }

    fn apply_size(&mut self, height: f64) {
        self.config.size = clamp_size(height);
        let layout = ChatLayout::placed(self.config.size, self.chat_layout.placement);
        self.chat_layout = layout;
        if let Some(window) = &self.window {
            let _ = window.request_inner_size(LogicalSize::new(layout.window.0, layout.window.1));
        }
    }

    fn set_chat_opacity(&mut self, opacity: f64) {
        self.config.chat_opacity = clamp_chat_opacity(opacity);
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.set_opacity(self.config.chat_opacity);
        }
        self.sync_overlay_bitmap();
        self.sync_panel();
        self.schedule_save();
    }

    fn select_pet(&mut self, index: usize) {
        let Some(dir) = self.pets.get(index) else {
            self.sync_panel();
            return;
        };
        match pet::load_pet(dir) {
            Ok(next) => {
                self.pet_index = index;
                self.install_pet(next);
                self.save_config();
                self.sync_panel();
            }
            Err(error) => {
                eprintln!("herdr-pet: cannot load {}: {error:#}", dir.display());
                self.sync_panel();
            }
        }
    }

    fn install_pet(&mut self, pet: Pet) {
        self.pet = pet;
        self.animator = Animator::new(self.config.gaze && self.pet.atlas.gaze_enabled());
        self.last_anim_input = None;
        self.config.pet = Some(pet_spec(&self.pet, self.config.pets_dir.as_deref()));
        if let Some(renderer) = &mut self.renderer {
            renderer.replace_atlas(&self.pet.atlas);
        }
        self.pending_uv = self.pet.atlas.cell_uv(0, 0);
        self.force_redraw = true;
        self.hit_region_key = None;
        self.next_tick = Instant::now();
    }

    fn set_pets_dir(&mut self, dir: Option<PathBuf>) {
        let dirs = match pet::pet_dirs_in(dir.as_deref()) {
            Ok(dirs) => dirs,
            Err(error) => {
                eprintln!("herdr-pet: cannot read the pets directory: {error:#}");
                self.sync_panel();
                return;
            }
        };
        let mut chosen = None;
        if let Some(index) = dirs.iter().position(|candidate| candidate == &self.pet.dir) {
            chosen = Some((index, None));
        } else {
            for (index, path) in dirs.iter().enumerate() {
                match pet::load_pet(path) {
                    Ok(pet) => {
                        chosen = Some((index, Some(pet)));
                        break;
                    }
                    Err(error) => eprintln!("herdr-pet: skipping {}: {error:#}", path.display()),
                }
            }
        }
        let Some((index, loaded)) = chosen else {
            eprintln!("herdr-pet: no loadable pet in the selected directory");
            self.sync_panel();
            return;
        };
        self.config.pets_dir = dir;
        self.pets = dirs;
        self.pet_index = index;
        if let Some(pet) = loaded {
            self.install_pet(pet);
        }
        self.save_config();
        self.sync_panel();
        self.sync_panel_pets();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        if let Some(monitor) = event_loop.primary_monitor() {
            self.primary_scale = monitor.scale_factor();
            self.primary_height = f64::from(monitor.size().height) / self.primary_scale;
        }
        self.chat_layout = ChatLayout::new(self.config.size);
        let (width, height) = self.chat_layout.window;
        let pet_width = (self.chat_layout.pet.w * self.primary_scale).round() as i32;
        let pet_height = (self.chat_layout.pet.h * self.primary_scale).round() as i32;
        let position = self
            .config
            .position
            .filter(|anchor| pet_visible(event_loop, *anchor, pet_width, pet_height))
            .map(|anchor| anchor_to_window(anchor, &self.chat_layout, self.primary_scale))
            .unwrap_or_else(|| default_position(event_loop, &self.chat_layout));
        let attributes = Window::default_attributes()
            .with_title("herdr-pet")
            .with_active(false)
            .with_inner_size(LogicalSize::new(width, height))
            .with_position(position)
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop);
        // DirectComposition presents the pet through the HWND visual, so the
        // window itself needs no redirection bitmap. The taskbar button is
        // skipped separately from the Alt+Tab entry (see `prepare_window`).
        #[cfg(target_os = "windows")]
        let attributes = {
            use winit::platform::windows::WindowAttributesExtWindows;
            attributes
                .with_skip_taskbar(true)
                .with_no_redirection_bitmap(true)
        };
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                eprintln!("herdr-pet: cannot create the window: {error}");
                event_loop.exit();
                return;
            }
        };
        // The window may open on a monitor whose scale differs from the
        // primary one, so re-anchor the saved pet with the window's scale.
        if let Some(anchor) = self
            .config
            .position
            .filter(|anchor| pet_visible(event_loop, *anchor, pet_width, pet_height))
        {
            let scale = window.scale_factor();
            if scale != self.primary_scale {
                window.set_outer_position(anchor_to_window(anchor, &self.chat_layout, scale));
            }
        }
        platform::prepare_window(&window);
        platform::set_click_through(&window, self.config.click_through);
        if !platform::float_on_all_spaces(&window) {
            eprintln!("herdr-pet: cannot make the window float on all Spaces");
        }
        if !platform::allow_window_above_menu_bar(&window) {
            eprintln!("herdr-pet: cannot lift the menu bar frame constraint");
        }
        match pollster::block_on(render::Renderer::new(window.clone(), &self.pet.atlas)) {
            Ok(renderer) => self.renderer = Some(renderer),
            Err(error) => {
                eprintln!("herdr-pet: cannot initialize the renderer: {error:#}");
                event_loop.exit();
                return;
            }
        }
        platform::prepare_notifications();
        match tray::Tray::new() {
            Ok(tray) => {
                tray.set_click_through(self.config.click_through);
                self.tray = Some(tray);
            }
            Err(error) => eprintln!("herdr-pet: tray unavailable: {error:#}"),
        }
        self.apply_tray_visibility();
        self.overlay =
            platform::ChatOverlay::new(&window, self.config.chat_open, self.config.chat_opacity);
        if self.overlay.is_none() {
            eprintln!("herdr-pet: chat overlay unavailable");
        }
        self.pending_uv = self.pet.atlas.cell_uv(0, 0);
        self.force_redraw = true;
        self.window = Some(window);
        self.cursor_hittest = None;
        self.set_cursor_hittest(!self.config.click_through);
        self.herdr_frontmost = platform::herdr_app_frontmost();
        self.next_front_check = Instant::now() + FRONT_CHECK_INTERVAL;
        self.sync_bubble_placement();
        self.sync_overlay();
        self.sync_chat();
        self.sync_overlay_bitmap();
        self.sync_hit_region(self.frame);
        // Start hidden until the first snapshot says an agent is active.
        self.apply_chat_visibility();
        self.next_tick = Instant::now();
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_tick));
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                self.save_config();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size.width, size.height);
                }
                self.sync_bubble_placement();
                self.force_redraw = true;
                self.next_tick = Instant::now();
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.press(),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => self.right_press(),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let was_dragging = self.dragging.take().is_some();
                self.finish_click(self.pointer());
                if was_dragging {
                    self.save_config();
                }
            }
            WindowEvent::RedrawRequested => self.present(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.handle_tray(event_loop);
        let Some(window) = &self.window else {
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        };
        let interval = if window.is_visible().unwrap_or(true) {
            TICK_INTERVAL
        } else {
            HIDDEN_INTERVAL
        };
        let now = Instant::now();
        if now >= self.next_tick {
            self.next_tick = now + interval;
            self.tick();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_tick));
    }
}

/// Whether a pet rectangle overlaps any monitor. The window's transparent
/// overhang is much wider than the pet, so the window rect must not stand in
/// for the pet here.
fn pet_visible(event_loop: &ActiveEventLoop, position: [i32; 2], width: i32, height: i32) -> bool {
    event_loop.available_monitors().any(|monitor| {
        let origin = monitor.position();
        let size = monitor.size();
        position[0] < origin.x + size.width as i32
            && position[0] + width > origin.x
            && position[1] < origin.y + size.height as i32
            && position[1] + height > origin.y
    })
}

fn default_position(event_loop: &ActiveEventLoop, layout: &ChatLayout) -> PhysicalPosition<i32> {
    let Some(monitor) = event_loop.primary_monitor() else {
        return PhysicalPosition::new(0, 0);
    };
    let scale = monitor.scale_factor();
    let margin = (EDGE_MARGIN * scale).round() as i32;
    let width = (layout.window.0 * scale).round() as i32;
    let height = (layout.window.1 * scale).round() as i32;
    // The window grew sideways to let the bubble slide; centre that extra
    // width so the default pet position matches the old bubble-sized window.
    let base_width = ((chat::BUBBLE_WIDTH + chat::WINDOW_PAD * 2.0) * scale).round() as i32;
    // The work area, not the full monitor, keeps the pet above the taskbar.
    let area = screen_area(&monitor);
    let right = (area.x + area.w).round() as i32;
    let bottom = (area.y + area.h).round() as i32;
    PhysicalPosition::new(
        right - margin - base_width - (width - base_width) / 2,
        bottom - height - margin,
    )
}

/// Convert the persisted pet top-left into the window's top-left.
fn anchor_to_window(anchor: [i32; 2], layout: &ChatLayout, scale: f64) -> PhysicalPosition<i32> {
    let (origin_x, origin_y) = layout.pet_origin();
    PhysicalPosition::new(
        anchor[0] - (origin_x * scale).round() as i32,
        anchor[1] - (origin_y * scale).round() as i32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn cli_defaults_to_first_pet() {
        assert_eq!(
            parse_cli(args(&[]).into_iter()).unwrap(),
            Cli::Run { pet: None }
        );
    }

    #[test]
    fn cli_accepts_pet_and_list_pets() {
        assert_eq!(
            parse_cli(args(&["--pet", "diana"]).into_iter()).unwrap(),
            Cli::Run {
                pet: Some("diana".to_owned())
            }
        );
        assert_eq!(
            parse_cli(args(&["--list-pets"]).into_iter()).unwrap(),
            Cli::ListPets
        );
    }

    #[test]
    fn cli_rejects_bad_arguments() {
        assert!(parse_cli(args(&["--pet"]).into_iter()).is_err());
        assert!(parse_cli(args(&["--nope"]).into_iter()).is_err());
        assert!(parse_cli(args(&["--pet", "a", "--pet", "b"]).into_iter()).is_err());
        assert!(parse_cli(args(&["--list-pets", "--pet", "a"]).into_iter()).is_err());
    }

    #[test]
    fn drag_direction_follows_instant_movement() {
        assert_eq!(step_dir(DragDir::Left, 1.0), DragDir::Right);
        assert_eq!(step_dir(DragDir::Right, -1.0), DragDir::Left);
        assert_eq!(step_dir(DragDir::Left, 0.0), DragDir::Left);
        assert_eq!(step_dir(DragDir::Right, 0.0), DragDir::Right);
    }

    #[test]
    fn click_slop_separates_clicks_from_drags() {
        assert!(within_click_slop((10.0, 10.0), (10.0, 10.0)));
        assert!(within_click_slop((10.0, 10.0), (12.0, 12.0)));
        assert!(!within_click_slop((10.0, 10.0), (13.1, 10.0)));
    }

    #[test]
    fn window_snapshot_converts_pointer_with_one_geometry_sample() {
        let snapshot = WindowSnapshot {
            origin: PhysicalPosition::new(1000, 500),
            size: winit::dpi::PhysicalSize::new(672, 400),
            scale: 2.0,
        };
        assert_eq!(snapshot.local_point((1100.0, 600.0)), (50.0, 50.0));
    }

    #[test]
    fn chat_opacity_is_clamped_and_invalid_falls_back() {
        assert_eq!(clamp_chat_opacity(0.4), 0.4);
        assert_eq!(clamp_chat_opacity(-0.5), 0.0);
        assert_eq!(clamp_chat_opacity(1.5), 1.0);
        assert_eq!(clamp_chat_opacity(f64::NAN), config::DEFAULT_CHAT_OPACITY);
    }

    #[test]
    fn manual_selection_falls_back_and_wraps() {
        use crate::status::{StatusRecord, Terminal};

        let terminal = |address: &str, state: &str, status_ms: u64| Terminal {
            address: address.to_owned(),
            title: address.to_owned(),
            state: "live".to_owned(),
            status: Some(StatusRecord {
                state: state.to_owned(),
                ..Default::default()
            }),
            status_ms: Some(status_ms),
            ..Default::default()
        };
        let mut table = StatusTable::new();
        table.apply_snapshot(vec![
            terminal("t1", "blocked", 100),
            terminal("t2", "error", 200),
            terminal("t3", "working", 300),
        ]);
        let candidates = table.focus_candidates_with(|_| true);
        assert_eq!(selected_index(&candidates, None), 0);
        assert_eq!(selected_index(&candidates, Some("t2")), 1);
        assert_eq!(
            selected_index(&candidates, Some("gone")),
            0,
            "a pick that stopped needing the bubble falls back to the first"
        );

        assert_eq!(wrap_index(0, 1, 3), 1);
        assert_eq!(wrap_index(2, 1, 3), 0);
        assert_eq!(wrap_index(0, -1, 3), 2);
    }

    #[test]
    fn tray_icon_hides_only_with_a_mouse_reachable_pet() {
        assert!(tray_icon_hidden(true, false, true));
        assert!(!tray_icon_hidden(true, true, true));
        assert!(!tray_icon_hidden(true, false, false));
        assert!(!tray_icon_hidden(false, false, true));
    }

    #[test]
    fn chat_ui_needs_an_active_agent_and_herdr_in_the_background() {
        assert!(chat_ui_available(true, false));
        assert!(!chat_ui_available(true, true));
        assert!(!chat_ui_available(false, false));
        assert!(!chat_ui_available(false, true));
    }

    #[test]
    fn forced_agents_show_the_bubble_even_when_hidden_or_collapsed() {
        assert!(chat_bubble_visible(true, true, false));
        assert!(!chat_bubble_visible(true, false, false));
        assert!(!chat_bubble_visible(false, true, false));
        assert!(!chat_bubble_visible(false, false, false));
        assert!(chat_bubble_visible(false, false, true));
        assert!(chat_bubble_visible(false, true, true));
        assert!(chat_bubble_visible(true, false, true));
        assert!(chat_bubble_visible(true, true, true));
    }

    #[test]
    fn saved_pet_anchor_round_trips_through_the_window_origin() {
        let layout = ChatLayout::placed(
            104.0,
            chat::BubblePlacement {
                below: true,
                shift_x: 112.0,
            },
        );
        let anchor = [100, 200];
        let window = anchor_to_window(anchor, &layout, 2.0);
        // Mirrors `App::save_config`, so a toggle or restart keeps the pet
        // exactly where the anchor says, independent of the bubble layout.
        assert_eq!(
            [
                window.x + (layout.pet.x * 2.0).round() as i32,
                window.y + (layout.pet.y * 2.0).round() as i32,
            ],
            anchor
        );
    }

    fn pet_at(x: f64, y: f64) -> Rect {
        Rect {
            x,
            y,
            w: 96.0,
            h: 104.0,
        }
    }

    fn default_area() -> Rect {
        Rect {
            x: 0.0,
            y: 25.0,
            w: 1440.0,
            h: 875.0,
        }
    }

    #[test]
    fn pet_drag_is_clamped_inside_the_screen_below_the_menu_bar() {
        let areas = [default_area()];

        // The pet never enters the menu bar, even when dragged far past it.
        let top_left = clamp_pet_to_screens(pet_at(-5000.0, -5000.0), &areas);
        assert_eq!((top_left.x, top_left.y), (0.0, 25.0));
        assert_eq!((top_left.w, top_left.h), (96.0, 104.0));

        // Nor does it leave the right or bottom edge.
        let bottom_right = clamp_pet_to_screens(pet_at(5000.0, 5000.0), &areas);
        assert_eq!(
            (bottom_right.x, bottom_right.y),
            (f64::from(1440 - 96), f64::from(25 + 875 - 104))
        );

        // Positions already inside the screen are left alone.
        let inside = pet_at(500.0, 300.0);
        assert_eq!(clamp_pet_to_screens(inside, &areas), inside);
    }

    #[test]
    fn pet_drag_crosses_adjacent_screens_without_a_jump() {
        let left = default_area();
        let right = Rect {
            x: 1440.0,
            y: 25.0,
            w: 1440.0,
            h: 875.0,
        };
        let areas = [left, right];

        // The sprite straddles the shared edge; both screens cover it.
        let straddling = pet_at(1400.0, 588.0);
        assert_eq!(clamp_pet_to_screens(straddling, &areas), straddling);

        // Beyond the outer edge it stops at the right screen's edge.
        let far_right = clamp_pet_to_screens(pet_at(5000.0, 500.0), &areas);
        assert_eq!(
            (far_right.x, far_right.y),
            (f64::from(1440 + 1440 - 96), 500.0)
        );
    }

    #[test]
    fn pet_drag_does_not_enter_the_gap_between_screens() {
        let left = default_area();
        let right = Rect {
            x: 2000.0,
            y: 25.0,
            w: 1440.0,
            h: 875.0,
        };
        let areas = [left, right];

        // The sprite straddles the left screen and the gap; its centre is on
        // the left screen, so it is clamped to that screen's edge.
        let in_gap = clamp_pet_to_screens(pet_at(1350.0, 588.0), &areas);
        assert_eq!((in_gap.x, in_gap.y), (f64::from(1440 - 96), 588.0));
    }

    #[test]
    fn pet_drag_uses_the_menu_bar_of_the_screen_it_is_on() {
        let primary = Rect {
            x: 0.0,
            y: 39.0,
            w: 1800.0,
            h: 1130.0,
        };
        let secondary = Rect {
            x: 1800.0,
            y: 25.0,
            w: 1440.0,
            h: 875.0,
        };
        let areas = [primary, secondary];

        let on_primary = clamp_pet_to_screens(pet_at(300.0, -5000.0), &areas);
        assert_eq!((on_primary.x, on_primary.y), (300.0, 39.0));

        let on_secondary = clamp_pet_to_screens(pet_at(2300.0, -100.0), &areas);
        assert_eq!((on_secondary.x, on_secondary.y), (2300.0, 25.0));
    }

    #[test]
    fn pet_clamp_does_not_panic_when_the_pet_is_larger_than_the_screen() {
        let tiny = Rect {
            x: 0.0,
            y: 25.0,
            w: 100.0,
            h: 80.0,
        };
        let pet = Rect {
            x: 500.0,
            y: 300.0,
            w: 192.0,
            h: 208.0,
        };
        let clamped = clamp_pet_to_screens(pet, &[tiny]);
        assert_eq!((clamped.x, clamped.y), (0.0, 25.0));
    }

    #[test]
    fn bubble_defaults_to_above_a_centred_pet() {
        let placement = bubble_placement(pet_at(600.0, 300.0), 1.0, &[default_area()], false);
        assert_eq!(placement, chat::BubblePlacement::default());
    }

    #[test]
    fn bubble_flips_below_when_the_top_edge_leaves_no_room() {
        let placement = bubble_placement(pet_at(600.0, 25.0), 1.0, &[default_area()], false);
        assert_eq!(
            placement,
            chat::BubblePlacement {
                below: true,
                shift_x: 0.0
            }
        );

        // The bottom edge still has room for the default row.
        let bottom = bubble_placement(pet_at(600.0, 796.0), 1.0, &[default_area()], false);
        assert_eq!(bottom, chat::BubblePlacement::default());
    }

    #[test]
    fn bubble_slides_to_stay_inside_the_side_edges() {
        let area = default_area();
        let left = bubble_placement(pet_at(0.0, 300.0), 1.0, &[area], false);
        assert_eq!(
            left,
            chat::BubblePlacement {
                below: false,
                shift_x: 112.0
            }
        );
        let right = bubble_placement(pet_at(1344.0, 300.0), 1.0, &[area], false);
        assert_eq!(
            right,
            chat::BubblePlacement {
                below: false,
                shift_x: -112.0
            }
        );

        // Corners combine the flip and the slide.
        let top_left = bubble_placement(pet_at(0.0, 25.0), 1.0, &[area], false);
        assert_eq!(
            top_left,
            chat::BubblePlacement {
                below: true,
                shift_x: 112.0
            }
        );
        let top_right = bubble_placement(pet_at(1344.0, 25.0), 1.0, &[area], false);
        assert_eq!(
            top_right,
            chat::BubblePlacement {
                below: true,
                shift_x: -112.0
            }
        );
    }

    #[test]
    fn bubble_restores_only_after_clearing_the_top_edge() {
        let area = default_area();
        // 82 px of headroom: enough above, but inside the flip-back margin.
        assert_eq!(
            bubble_placement(pet_at(600.0, 107.0), 1.0, &[area], false),
            chat::BubblePlacement::default()
        );
        assert_eq!(
            bubble_placement(pet_at(600.0, 107.0), 1.0, &[area], true),
            chat::BubblePlacement {
                below: true,
                shift_x: 0.0
            }
        );
        // 84 px clears the margin and returns to the default row.
        assert_eq!(
            bubble_placement(pet_at(600.0, 109.0), 1.0, &[area], true),
            chat::BubblePlacement::default()
        );
    }

    #[test]
    fn bubble_takes_the_row_that_overflows_least_on_a_short_screen() {
        // Neither row fits: 79 px above and 100 px below, needing 80 and 114.
        // Above overflows by one point; below would overflow by fourteen.
        let short = Rect {
            x: 0.0,
            y: 25.0,
            w: 1440.0,
            h: 283.0,
        };
        let placement = bubble_placement(pet_at(600.0, 104.0), 1.0, &[short], false);
        assert_eq!(placement, chat::BubblePlacement::default());
    }

    #[test]
    fn bubble_does_not_slide_at_the_seam_between_screens() {
        let left = default_area();
        let right = Rect {
            x: 1440.0,
            y: 25.0,
            w: 1440.0,
            h: 875.0,
        };
        let placement = bubble_placement(pet_at(1392.0, 300.0), 1.0, &[left, right], false);
        assert_eq!(
            placement,
            chat::BubblePlacement::default(),
            "the union of both screens covers the centred bubble"
        );
    }

    #[test]
    fn aggregate_status_maps_to_mood() {
        assert_eq!(mood_for(Some(AgentStatus::Blocked), false), Mood::Blocked);
        assert_eq!(mood_for(Some(AgentStatus::Error), false), Mood::Failed);
        assert_eq!(mood_for(Some(AgentStatus::Done), true), Mood::Review);
        assert_eq!(mood_for(Some(AgentStatus::Done), false), Mood::Done);
        assert_eq!(mood_for(Some(AgentStatus::Working), false), Mood::Working);
        assert_eq!(mood_for(Some(AgentStatus::Idle), true), Mood::Idle);
        assert_eq!(mood_for(None, true), Mood::Idle);
    }
}
