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

fn mood_for(status: Option<AgentStatus>) -> Mood {
    match status {
        Some(AgentStatus::Blocked) => Mood::Blocked,
        Some(AgentStatus::Error) => Mood::Failed,
        Some(AgentStatus::Done) => Mood::Done,
        Some(AgentStatus::Working) => Mood::Working,
        Some(AgentStatus::Idle) | None => Mood::Idle,
    }
}

#[derive(Debug, Clone)]
struct Drag {
    start_mouse: (f64, f64),
    last_mouse: (f64, f64),
    start_window: PhysicalPosition<i32>,
    dir: DragDir,
    /// Usable screen areas captured at drag start; the pet may cross them.
    areas: Vec<Rect>,
    /// Window scale the areas and drag offsets are expressed in.
    scale: f64,
}

/// Usable area of `monitor`: the screen with its top edge below the menu bar.
/// The bubble and button may hang outside a screen; only the pet is bounded.
fn screen_area(monitor: &MonitorHandle) -> Rect {
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

/// Move a window top-left so the pet sprite stays on screen. A sprite fully
/// covered by the screen areas keeps its position, so dragging across
/// adjacent screens is smooth; otherwise it is clamped into the screen
/// holding its centre.
fn clamp_pet_to_screens(
    window: PhysicalPosition<i32>,
    pet: Rect,
    scale: f64,
    areas: &[Rect],
) -> PhysicalPosition<i32> {
    if areas.is_empty() {
        return window;
    }
    let offset = (pet.x * scale, pet.y * scale);
    let target = Rect {
        x: f64::from(window.x) + offset.0,
        y: f64::from(window.y) + offset.1,
        w: pet.w * scale,
        h: pet.h * scale,
    };
    if covered_by_any(target, areas) {
        return window;
    }
    let Some(area) = screen_for(target, areas) else {
        return window;
    };
    PhysicalPosition::new(
        (clamp_axis(target.x, area.x, area.x + area.w - target.w) - offset.0).round() as i32,
        (clamp_axis(target.y, area.y, area.y + area.h - target.h) - offset.1).round() as i32,
    )
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
        let _ = window.set_cursor_hittest(hittest);
        self.cursor_hittest = Some(hittest);
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
        if let Some(overlay) = &self.overlay {
            overlay.set_visible(bubble, self.chat_enabled);
        }
        self.next_tick = Instant::now();
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
            Some(agent) => herdr_app::focus_terminal(agent.address.clone()),
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
        let text = chat_text(self.focus_agent());
        if self.last_chat_text.as_ref() == Some(&text) {
            return;
        }
        if let Some(overlay) = &self.overlay {
            overlay.set_text(&text.title, &text.subtitle);
            let icon = text
                .icon
                .and_then(|icon| herdr_app::icon_path(icon.file).map(|path| (path, icon.tint)));
            overlay.set_icon(icon.as_ref().map(|(path, tint)| (path.as_path(), *tint)));
        }
        self.last_chat_text = Some(text);
    }

    /// Refresh the logical layout and refit the native views after a resize or
    /// a size change. AppKit frames are in point coordinates.
    fn sync_overlay(&mut self) {
        let layout = ChatLayout::new(self.config.size);
        if layout != self.chat_layout {
            self.chat_layout = layout;
        }
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
        if let Some(overlay) = &self.overlay {
            overlay.layout(&self.chat_layout, f64::from(size.height) / scale, pager);
            overlay.set_visible(self.bubble_visible(), self.chat_enabled);
        }
        self.overlay_key = Some(key);
        self.force_redraw = true;
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
            self.dragging = Some(Drag {
                start_mouse: pointer,
                last_mouse: pointer,
                start_window: position,
                dir: DragDir::Right,
                areas,
                scale: window.scale_factor(),
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
            let target = PhysicalPosition::new(
                (drag.start_window.x as f64 + dx).round() as i32,
                (drag.start_window.y as f64 + dy).round() as i32,
            );
            let position =
                clamp_pet_to_screens(target, self.chat_layout.pet, drag.scale, &drag.areas);
            window.set_outer_position(position);
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
        self.sync_overlay();
        let pointer = self.pointer();
        let snapshot = self.window.as_deref().and_then(WindowSnapshot::new);
        let local = snapshot
            .zip(pointer)
            .map(|(snapshot, pointer)| snapshot.local_point(pointer));
        self.update_drag(pointer);
        let gaze = snapshot.and_then(|snapshot| self.gaze(snapshot, pointer));
        let hover = self.hovered(local);
        let dragging = self.dragging.as_ref().map(|drag| drag.dir);
        let input = AnimInput {
            mood: mood_for(self.mood),
            dragging,
            gaze,
            hover,
        };
        let now_ms = self.started.elapsed().as_millis() as u64;
        if self.last_anim_input != Some(input) || now_ms >= self.next_anim_ms {
            self.frame = self.animator.update(now_ms, input);
            self.next_anim_ms = self.animator.next_deadline_ms(now_ms).unwrap_or(u64::MAX);
            self.last_anim_input = Some(input);
            self.sync_chat();
        }
        let frame = self.frame;

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
                }
                Action::Settings => self.open_panel(),
                Action::About => platform::show_about(),
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
        self.set_cursor_hittest(!enabled);
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
        let layout = ChatLayout::new(self.config.size);
        self.chat_layout = layout;
        if let Some(window) = &self.window {
            let _ = window.request_inner_size(LogicalSize::new(layout.window.0, layout.window.1));
        }
    }

    fn set_chat_opacity(&mut self, opacity: f64) {
        self.config.chat_opacity = clamp_chat_opacity(opacity);
        if let Some(overlay) = &self.overlay {
            overlay.set_opacity(self.config.chat_opacity);
        }
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
        let physical_width = (width * self.primary_scale).round() as i32;
        let physical_height = (height * self.primary_scale).round() as i32;
        let position = self
            .config
            .position
            .map(|position| anchor_to_window(position, &self.chat_layout, self.primary_scale))
            .filter(|position| {
                position_visible(
                    event_loop,
                    [position.x, position.y],
                    physical_width,
                    physical_height,
                )
            })
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
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                eprintln!("herdr-pet: cannot create the window: {error}");
                event_loop.exit();
                return;
            }
        };
        if self.config.click_through {
            let _ = window.set_cursor_hittest(false);
        }
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
        match tray::Tray::new() {
            Ok(tray) => self.tray = Some(tray),
            Err(error) => eprintln!("herdr-pet: tray unavailable: {error:#}"),
        }
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
        self.sync_overlay();
        self.sync_chat();
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
                self.force_redraw = true;
                self.next_tick = Instant::now();
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.press(),
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

fn position_visible(
    event_loop: &ActiveEventLoop,
    position: [i32; 2],
    width: i32,
    height: i32,
) -> bool {
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
    let size = monitor.size();
    let position = monitor.position();
    PhysicalPosition::new(
        position.x + size.width as i32 - width - margin,
        position.y + size.height as i32 - height - margin,
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

    fn pet_in_window() -> Rect {
        Rect {
            x: 120.0,
            y: 88.0,
            w: 96.0,
            h: 104.0,
        }
    }

    fn pet_top_left(position: PhysicalPosition<i32>, pet: Rect, scale: f64) -> (f64, f64) {
        (
            f64::from(position.x) + pet.x * scale,
            f64::from(position.y) + pet.y * scale,
        )
    }

    #[test]
    fn pet_drag_is_clamped_inside_the_screen_below_the_menu_bar() {
        let area = Rect {
            x: 0.0,
            y: 25.0,
            w: 1440.0,
            h: 875.0,
        };
        let pet = pet_in_window();
        let areas = [area];

        // The pet never enters the menu bar, even when dragged far past it.
        let top_left = clamp_pet_to_screens(PhysicalPosition::new(-5000, -5000), pet, 2.0, &areas);
        assert_eq!(pet_top_left(top_left, pet, 2.0), (0.0, 25.0));

        // Nor does it leave the right or bottom edge.
        let bottom_right =
            clamp_pet_to_screens(PhysicalPosition::new(5000, 5000), pet, 2.0, &areas);
        assert_eq!(
            pet_top_left(bottom_right, pet, 2.0),
            (f64::from(1440 - 96 * 2), f64::from(25 + 875 - 104 * 2))
        );

        // Positions already inside the screen are left alone.
        let inside = PhysicalPosition::new(500, 300);
        assert_eq!(clamp_pet_to_screens(inside, pet, 2.0, &areas), inside);

        // Non-Retina screens use the same rules.
        let standard = clamp_pet_to_screens(PhysicalPosition::new(5000, 5000), pet, 1.0, &areas);
        assert_eq!(
            pet_top_left(standard, pet, 1.0),
            (f64::from(1440 - 96), f64::from(25 + 875 - 104))
        );
    }

    #[test]
    fn pet_drag_crosses_adjacent_screens_without_a_jump() {
        let left = Rect {
            x: 0.0,
            y: 25.0,
            w: 1440.0,
            h: 875.0,
        };
        let right = Rect {
            x: 1440.0,
            y: 25.0,
            w: 1440.0,
            h: 875.0,
        };
        let areas = [left, right];
        let pet = pet_in_window();

        // The sprite straddles the shared edge; both screens cover it.
        let straddling = PhysicalPosition::new(1280, 500);
        assert_eq!(pet_top_left(straddling, pet, 1.0), (1400.0, 588.0));
        assert_eq!(
            clamp_pet_to_screens(straddling, pet, 1.0, &areas),
            straddling
        );

        // Beyond the outer edge it stops at the right screen's edge.
        let far_right = clamp_pet_to_screens(PhysicalPosition::new(5000, 500), pet, 1.0, &areas);
        assert_eq!(
            pet_top_left(far_right, pet, 1.0),
            (f64::from(1440 + 1440 - 96), 588.0)
        );
    }

    #[test]
    fn pet_drag_does_not_enter_the_gap_between_screens() {
        let left = Rect {
            x: 0.0,
            y: 25.0,
            w: 1440.0,
            h: 875.0,
        };
        let right = Rect {
            x: 2000.0,
            y: 25.0,
            w: 1440.0,
            h: 875.0,
        };
        let areas = [left, right];
        let pet = pet_in_window();

        // Sprite left at 1470 sits entirely in the 560 px gap; its centre is
        // nearer the left screen, so it is clamped to that screen's edge.
        let in_gap = PhysicalPosition::new(1350, 500);
        let clamped = clamp_pet_to_screens(in_gap, pet, 1.0, &areas);
        assert_eq!(
            pet_top_left(clamped, pet, 1.0),
            (f64::from(1440 - 96), 588.0)
        );
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
        let pet = pet_in_window();

        let on_primary = clamp_pet_to_screens(PhysicalPosition::new(300, -5000), pet, 1.0, &areas);
        assert_eq!(pet_top_left(on_primary, pet, 1.0), (420.0, 39.0));

        let on_secondary =
            clamp_pet_to_screens(PhysicalPosition::new(2300, -100), pet, 1.0, &areas);
        assert_eq!(pet_top_left(on_secondary, pet, 1.0), (2420.0, 25.0));
    }

    #[test]
    fn pet_clamp_does_not_panic_when_the_pet_is_larger_than_the_screen() {
        let tiny = Rect {
            x: 0.0,
            y: 25.0,
            w: 100.0,
            h: 80.0,
        };
        let pet = pet_in_window();
        let clamped = clamp_pet_to_screens(PhysicalPosition::new(500, 300), pet, 2.0, &[tiny]);
        assert_eq!(pet_top_left(clamped, pet, 2.0), (0.0, 25.0));
    }

    #[test]
    fn aggregate_status_maps_to_mood() {
        assert_eq!(mood_for(Some(AgentStatus::Blocked)), Mood::Blocked);
        assert_eq!(mood_for(Some(AgentStatus::Error)), Mood::Failed);
        assert_eq!(mood_for(Some(AgentStatus::Done)), Mood::Done);
        assert_eq!(mood_for(Some(AgentStatus::Working)), Mood::Working);
        assert_eq!(mood_for(Some(AgentStatus::Idle)), Mood::Idle);
        assert_eq!(mood_for(None), Mood::Idle);
    }
}
