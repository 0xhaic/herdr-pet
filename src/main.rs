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
use winit::window::{Window, WindowId, WindowLevel};

use crate::chat::{ChatLayout, ChatText, chat_text};
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

#[derive(Debug, Clone, Copy)]
struct Drag {
    start_mouse: (f64, f64),
    last_mouse: (f64, f64),
    start_window: PhysicalPosition<i32>,
    dir: DragDir,
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
    /// frontmost. When false, both the bubble and the button are hidden and
    /// their rects stop taking clicks.
    chat_enabled: bool,
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
    primary_height: f64,
    primary_scale: f64,
    tray: Option<tray::Tray>,
    panel: Option<SettingsPanel>,
    /// When to persist live slider changes after the user stops moving.
    save_due: Option<Instant>,
    pets: Vec<PathBuf>,
    pet_index: usize,
    chat_layout: ChatLayout,
    overlay: Option<platform::ChatOverlay>,
    /// Window size, scale, and layout the native views were last fitted to.
    overlay_key: Option<(u32, u32, u64, ChatLayout)>,
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
            click_candidate: None,
            started: Instant::now(),
            next_tick: Instant::now(),
            pending_uv,
            presented_uv: None,
            force_redraw: true,
            config,
            dragging: None,
            primary_height: 0.0,
            primary_scale: 1.0,
            tray: None,
            panel: None,
            save_due: None,
            pets,
            pet_index,
            chat_layout,
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
                    if let Some(change) = self.table.apply_event(event) {
                        changes.push(change);
                    }
                }
                HerdrMessage::Disconnected => {}
            }
        }
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
        let window = self.window.as_ref()?;
        let position = window.outer_position().ok()?;
        let scale = window.scale_factor();
        Some((
            (pointer.0 - f64::from(position.x)) / scale,
            (pointer.1 - f64::from(position.y)) / scale,
        ))
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
        if self.chat_enabled {
            if self.chat_layout.button.contains(x, y) {
                self.toggle_chat();
                return;
            }
            if self.config.chat_open && self.chat_layout.bubble.contains(x, y) {
                self.focus_herdr_chat();
                return;
            }
        }
        if self.chat_layout.pet.contains(x, y) {
            self.start_drag();
        }
    }

    fn apply_chat_visibility(&mut self) {
        if let Some(overlay) = &self.overlay {
            overlay.set_visible(
                self.chat_enabled && self.config.chat_open,
                self.chat_enabled,
            );
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
    /// seen; blocked/working/error always count.
    fn update_chat_enabled(&mut self) {
        let seen = &self.herdr_attention.seen;
        let active = self.table.is_active_with(|address| !seen.contains(address));
        let enabled = chat_ui_available(active, self.herdr_frontmost);
        if enabled == self.chat_enabled {
            return;
        }
        self.chat_enabled = enabled;
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

    /// The agent the bubble shows (and click-to-jump targets), using the same
    /// priority and Herdr seen state as the visibility rule.
    fn focus_agent(&self) -> Option<&TerminalAgent> {
        let seen = &self.herdr_attention.seen;
        self.table.focus_with(|address| !seen.contains(address))
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
        let text = chat_text(self.focus_agent());
        if self.last_chat_text.as_ref() == Some(&text) {
            return;
        }
        if let Some(overlay) = &self.overlay {
            overlay.set_text(&text.title, &text.subtitle);
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
        let key = (size.width, size.height, scale.to_bits(), self.chat_layout);
        if self.overlay_key == Some(key) {
            return;
        }
        if let Some(overlay) = &self.overlay {
            overlay.layout(&self.chat_layout, f64::from(size.height) / scale);
            overlay.set_visible(
                self.chat_enabled && self.config.chat_open,
                self.chat_enabled,
            );
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
            self.dragging = Some(Drag {
                start_mouse: pointer,
                last_mouse: pointer,
                start_window: position,
                dir: DragDir::Right,
            });
            self.click_candidate = Some(pointer);
            let _ = window.set_cursor_hittest(true);
        }
    }

    fn update_drag(&mut self, pointer: Option<(f64, f64)>) {
        let Some(mut drag) = self.dragging else {
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
            let position = PhysicalPosition::new(
                (drag.start_window.x as f64 + dx).round() as i32,
                (drag.start_window.y as f64 + dy).round() as i32,
            );
            window.set_outer_position(position);
        }
        self.dragging = Some(drag);
    }

    fn gaze(&self, pointer: Option<(f64, f64)>) -> Option<GazeCell> {
        if self.dragging.is_some() {
            return None;
        }
        let window = self.window.as_ref()?;
        if !window.is_visible().unwrap_or(true) {
            return None;
        }
        let pointer = pointer?;
        let position = window.outer_position().ok()?;
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return None;
        }
        let scale = window.scale_factor();
        // Gaze pivots on the pet cell, not the taller window centre.
        let (center_x, center_y) = self.chat_layout.pet.center();
        let center_x = position.x as f64 + center_x * scale;
        let center_y = position.y as f64 + center_y * scale;
        gaze_cell(
            ((pointer.0 - center_x) / scale) as f32,
            ((pointer.1 - center_y) / scale) as f32,
        )
    }

    /// Codex treats the pet's bounding box as the hover target; only the sprite
    /// cell counts, not the bubble or the transparent margins.
    fn hovered(&self, pointer: Option<(f64, f64)>) -> bool {
        if self.dragging.is_some() {
            return false;
        }
        let Some(window) = self.window.as_ref() else {
            return false;
        };
        if !window.is_visible().unwrap_or(true) {
            return false;
        }
        let Some(pointer) = pointer else {
            return false;
        };
        let Some((x, y)) = self.local_point(pointer) else {
            return false;
        };
        self.chat_layout.pet.contains(x, y)
    }

    fn update_cursor(&self, pointer: Option<(f64, f64)>, frame: Frame) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        if !window.is_visible().unwrap_or(true) {
            return;
        }
        if self.dragging.is_some() {
            let _ = window.set_cursor_hittest(true);
            return;
        }
        if self.config.click_through {
            let _ = window.set_cursor_hittest(false);
            return;
        }
        let Some(pointer) = pointer else {
            return;
        };
        let Some((x, y)) = self.local_point(pointer) else {
            return;
        };
        let layout = &self.chat_layout;
        if self.chat_enabled
            && (layout.button.contains(x, y)
                || (self.config.chat_open && layout.bubble.contains(x, y)))
        {
            let _ = window.set_cursor_hittest(true);
            return;
        }
        if !layout.pet.contains(x, y) {
            let _ = window.set_cursor_hittest(false);
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
        let _ = window.set_cursor_hittest(alpha.is_some_and(|alpha| alpha >= CLICK_ALPHA));
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
        self.sync_chat();
        self.sync_overlay();
        let pointer = self.pointer();
        self.update_drag(pointer);
        let mood = mood_for(self.table.aggregate().status);
        let gaze = self.gaze(pointer);
        let hover = self.hovered(pointer);
        let dragging = self.dragging.map(|drag| drag.dir);
        let now_ms = self.started.elapsed().as_millis() as u64;
        let frame = self.animator.update(
            now_ms,
            AnimInput {
                mood,
                dragging,
                gaze,
                hover,
            },
        );

        self.update_cursor(pointer, frame);
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
                            let _ = window.set_cursor_hittest(!self.config.click_through);
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
        if let Some(panel) = &self.panel {
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
        self.save_config();
        self.sync_panel();
    }

    fn set_click_through(&mut self, enabled: bool) {
        self.config.click_through = enabled;
        if enabled {
            self.dragging = None;
        }
        if let Some(window) = &self.window {
            let _ = window.set_cursor_hittest(!enabled);
        }
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
    fn chat_opacity_is_clamped_and_invalid_falls_back() {
        assert_eq!(clamp_chat_opacity(0.4), 0.4);
        assert_eq!(clamp_chat_opacity(-0.5), 0.0);
        assert_eq!(clamp_chat_opacity(1.5), 1.0);
        assert_eq!(clamp_chat_opacity(f64::NAN), config::DEFAULT_CHAT_OPACITY);
    }

    #[test]
    fn chat_ui_needs_an_active_agent_and_herdr_in_the_background() {
        assert!(chat_ui_available(true, false));
        assert!(!chat_ui_available(true, true));
        assert!(!chat_ui_available(false, false));
        assert!(!chat_ui_available(false, true));
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
