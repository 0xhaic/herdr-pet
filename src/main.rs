mod config;
mod herdr;
mod notify;
mod pet;
mod platform;
mod render;
mod status;
mod tray;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId, WindowLevel};

use crate::config::Config;
use crate::herdr::HerdrMessage;
use crate::pet::{AnimInput, Animator, DragDir, Frame, GazeCell, Mood, Pet, gaze_cell};
use crate::status::{AgentStatus, StatusTable};
use crate::tray::Action;

const USAGE: &str = "usage: herdr-pet [--pet <name|path>] [--list-pets]";
const PET_HEIGHT: f64 = 104.0;
const PET_WIDTH: f64 = PET_HEIGHT * pet::CELL_W as f64 / pet::CELL_H as f64;
const MIN_PET_HEIGHT: f64 = 52.0;
const MAX_PET_HEIGHT: f64 = 312.0;
const EDGE_MARGIN: f64 = 24.0;
const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const HIDDEN_INTERVAL: Duration = Duration::from_millis(250);
const CLICK_ALPHA: u8 = 16;

#[derive(Debug, PartialEq, Eq)]
enum Cli {
    Run { pet: Option<String> },
    ListPets,
}

fn main() -> Result<()> {
    let config = config::load();
    match parse_cli(std::env::args().skip(1))? {
        Cli::ListPets => list_pets(),
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

fn list_pets() -> Result<()> {
    let dirs = pet::pet_dirs()?;
    if dirs.is_empty() {
        println!("no pets in {}", pet::pets_dir()?.display());
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

    let mut builder = EventLoop::builder();
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder.with_activation_policy(ActivationPolicy::Accessory);
    }
    let event_loop = builder.build().context("creating the event loop")?;
    let mut app = App::new(pet, receiver, config);
    event_loop
        .run_app(&mut app)
        .context("running the event loop")
}

fn open_pet(cli_pet: Option<&str>, config: &Config) -> Result<Pet> {
    if let Some(spec) = cli_pet {
        return open_pet_spec(spec);
    }
    if let Some(spec) = config.pet.as_deref().filter(|spec| !spec.is_empty()) {
        match open_pet_spec(spec) {
            Ok(pet) => return Ok(pet),
            Err(error) => {
                eprintln!(
                    "herdr-pet: configured pet {spec:?} unavailable ({error:#}); falling back"
                );
            }
        }
    }
    pet::load_first_pet()
}

fn open_pet_spec(spec: &str) -> Result<Pet> {
    let direct = PathBuf::from(spec);
    if direct.join("pet.json").is_file() {
        return pet::load_pet(&direct);
    }
    let candidate = pet::pets_dir()?.join(spec);
    if candidate.join("pet.json").is_file() {
        return pet::load_pet(&candidate);
    }
    bail!(
        "pet {spec:?} not found (tried {} and {})",
        direct.display(),
        candidate.display()
    )
}

fn pet_spec(pet: &Pet) -> String {
    if let Ok(pets_dir) = pet::pets_dir()
        && pet.dir.parent() == Some(pets_dir.as_path())
        && let Some(name) = pet.dir.file_name().and_then(|name| name.to_str())
    {
        return name.to_owned();
    }
    pet.dir.display().to_string()
}

fn clamp_size(size: f64) -> f64 {
    if size.is_finite() {
        size.clamp(MIN_PET_HEIGHT, MAX_PET_HEIGHT)
    } else {
        PET_HEIGHT
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

struct App {
    pet: Pet,
    window: Option<Arc<Window>>,
    renderer: Option<render::Renderer>,
    table: StatusTable,
    animator: Animator,
    messages: Receiver<HerdrMessage>,
    started: Instant,
    next_frame: Instant,
    config: Config,
    dragging: Option<Drag>,
    primary_height: f64,
    primary_scale: f64,
    tray: Option<tray::Tray>,
    pets: Vec<PathBuf>,
    pet_index: usize,
}

impl App {
    fn new(pet: Pet, messages: Receiver<HerdrMessage>, mut config: Config) -> Self {
        config.size = clamp_size(config.size);
        let animator = Animator::new(config.gaze && pet.atlas.gaze_enabled());
        let pets = pet::pet_dirs().unwrap_or_default();
        let pet_index = pets.iter().position(|dir| dir == &pet.dir).unwrap_or(0);
        Self {
            pet,
            window: None,
            renderer: None,
            table: StatusTable::new(),
            animator,
            messages,
            started: Instant::now(),
            next_frame: Instant::now(),
            config,
            dragging: None,
            primary_height: 0.0,
            primary_scale: 1.0,
            tray: None,
            pets,
            pet_index,
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
            self.config.position = Some([position.x, position.y]);
        }
        self.config.pet = Some(pet_spec(&self.pet));
        if let Err(error) = self.config.save() {
            eprintln!("herdr-pet: cannot save config: {error:#}");
        }
    }

    fn pointer(&self) -> Option<(f64, f64)> {
        platform::mouse_position(self.primary_height, self.primary_scale)
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
            self.save_config();
            return;
        }
        drag.dir = step_dir(drag.dir, pointer.0 - drag.last_mouse.0);
        drag.last_mouse = pointer;
        let dx = pointer.0 - drag.start_mouse.0;
        let dy = pointer.1 - drag.start_mouse.1;
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
        let center_x = position.x as f64 + size.width as f64 / 2.0;
        let center_y = position.y as f64 + size.height as f64 / 2.0;
        let scale = window.scale_factor();
        gaze_cell(
            ((pointer.0 - center_x) / scale) as f32,
            ((pointer.1 - center_y) / scale) as f32,
        )
    }

    /// Codex treats the pet's bounding box as the hover target; the window is
    /// sized to one sprite cell, so the window rect is that box.
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
        let Ok(position) = window.outer_position() else {
            return false;
        };
        let size = window.inner_size();
        let x = pointer.0 - f64::from(position.x);
        let y = pointer.1 - f64::from(position.y);
        (0.0..f64::from(size.width)).contains(&x) && (0.0..f64::from(size.height)).contains(&y)
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
        let Ok(position) = window.outer_position() else {
            return;
        };
        let size = window.inner_size();
        let alpha = self.pet.atlas.alpha_at_view(
            frame.row,
            frame.col,
            pointer.0 - f64::from(position.x),
            pointer.1 - f64::from(position.y),
            f64::from(size.width),
            f64::from(size.height),
        );
        let _ = window.set_cursor_hittest(alpha.is_some_and(|alpha| alpha >= CLICK_ALPHA));
    }

    fn redraw(&mut self) {
        self.drain_messages();
        let visible = self
            .window
            .as_ref()
            .is_some_and(|window| window.is_visible().unwrap_or(true));
        if !visible {
            return;
        }

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

        if let Some(renderer) = &mut self.renderer {
            renderer.set_uv(self.pet.atlas.cell_uv(frame.row, frame.col));
            if let Err(error) = renderer.render() {
                eprintln!("herdr-pet: render error: {error:#}");
            }
        }
        self.update_cursor(pointer, frame);
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
                        }
                        self.next_frame = Instant::now();
                    }
                }
                Action::NextPet => self.next_pet(),
                Action::Bigger => self.set_size(self.config.size * 1.25),
                Action::Smaller => self.set_size(self.config.size / 1.25),
                Action::ToggleClickThrough => {
                    self.config.click_through = !self.config.click_through;
                    if let Some(tray) = &self.tray {
                        tray.set_click_through(self.config.click_through);
                    }
                    if self.config.click_through {
                        self.dragging = None;
                    }
                    if let Some(window) = &self.window {
                        let _ = window.set_cursor_hittest(!self.config.click_through);
                    }
                    self.save_config();
                }
                Action::ToggleNotifications => {
                    self.config.notifications = !self.config.notifications;
                    if let Some(tray) = &self.tray {
                        tray.set_notifications(self.config.notifications);
                    }
                    self.save_config();
                }
                Action::ToggleNotificationSound => {
                    self.config.notification_sound = !self.config.notification_sound;
                    if let Some(tray) = &self.tray {
                        tray.set_notification_sound(self.config.notification_sound);
                    }
                    self.save_config();
                }
                Action::About => platform::show_about(),
                Action::Quit => {
                    self.save_config();
                    event_loop.exit();
                }
            }
        }
    }

    fn set_size(&mut self, height: f64) {
        self.config.size = clamp_size(height);
        if let Some(window) = &self.window {
            let width = self.config.size * PET_WIDTH / PET_HEIGHT;
            let _ = window.request_inner_size(LogicalSize::new(width, self.config.size));
        }
        self.save_config();
    }

    fn next_pet(&mut self) {
        if self.pets.is_empty() {
            return;
        }
        for offset in 1..=self.pets.len() {
            let index = (self.pet_index + offset) % self.pets.len();
            match pet::load_pet(&self.pets[index]) {
                Ok(next) => {
                    self.pet = next;
                    self.pet_index = index;
                    self.animator =
                        Animator::new(self.config.gaze && self.pet.atlas.gaze_enabled());
                    self.config.pet = Some(pet_spec(&self.pet));
                    if let Some(renderer) = &mut self.renderer {
                        renderer.replace_atlas(&self.pet.atlas);
                    }
                    self.save_config();
                    return;
                }
                Err(error) => eprintln!(
                    "herdr-pet: skipping {}: {error:#}",
                    self.pets[index].display()
                ),
            }
        }
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
        let width = self.config.size * PET_WIDTH / PET_HEIGHT;
        let height = self.config.size;
        let physical_width = (width * self.primary_scale).round() as i32;
        let physical_height = (height * self.primary_scale).round() as i32;
        let position = self
            .config
            .position
            .filter(|position| {
                position_visible(event_loop, *position, physical_width, physical_height)
            })
            .map(|[x, y]| PhysicalPosition::new(x, y))
            .unwrap_or_else(|| default_position(event_loop));
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
        match tray::Tray::new(
            self.config.notifications,
            self.config.notification_sound,
            self.config.click_through,
        ) {
            Ok(tray) => self.tray = Some(tray),
            Err(error) => eprintln!("herdr-pet: tray unavailable: {error:#}"),
        }
        self.window = Some(window);
        self.next_frame = Instant::now();
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
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
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.start_drag(),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.dragging = None;
                self.save_config();
            }
            WindowEvent::RedrawRequested => self.redraw(),
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
            FRAME_INTERVAL
        } else {
            HIDDEN_INTERVAL
        };
        let now = Instant::now();
        if now >= self.next_frame {
            self.next_frame = now + interval;
            window.request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
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

fn default_position(event_loop: &ActiveEventLoop) -> PhysicalPosition<i32> {
    let Some(monitor) = event_loop.primary_monitor() else {
        return PhysicalPosition::new(0, 0);
    };
    let scale = monitor.scale_factor();
    let margin = (EDGE_MARGIN * scale).round() as i32;
    let width = (PET_WIDTH * scale).round() as i32;
    let height = (PET_HEIGHT * scale).round() as i32;
    let size = monitor.size();
    let position = monitor.position();
    PhysicalPosition::new(
        position.x + size.width as i32 - width - margin,
        position.y + size.height as i32 - height - margin,
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
    fn aggregate_status_maps_to_mood() {
        assert_eq!(mood_for(Some(AgentStatus::Blocked)), Mood::Blocked);
        assert_eq!(mood_for(Some(AgentStatus::Error)), Mood::Failed);
        assert_eq!(mood_for(Some(AgentStatus::Done)), Mood::Done);
        assert_eq!(mood_for(Some(AgentStatus::Working)), Mood::Working);
        assert_eq!(mood_for(Some(AgentStatus::Idle)), Mood::Idle);
        assert_eq!(mood_for(None), Mood::Idle);
    }
}
