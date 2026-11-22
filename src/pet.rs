//! Codex pet manifest, spritesheet atlas, and animation state machine.
//!
//! Everything here is pure logic: no windowing, no herdr, no platform code.
//! The atlas is kept as one RGBA image; frames are addressed by row/column.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub const CELL_W: u32 = 192;
pub const CELL_H: u32 = 208;
pub const ATLAS_COLS: u32 = 8;
pub const MIN_ROWS: u32 = 9;
pub const LOOK_ROWS: u32 = 11;
pub const MAX_SPRITESHEET_BYTES: u64 = 20 * 1024 * 1024;

/// Gaze dead zone in logical pixels. Closer than this falls back to idle.
pub const GAZE_DEAD_ZONE: f32 = 24.0;

/// Done sits without responding to the mouse for this long after the aggregate
/// status first reports Done.
const DONE_GAZE_DELAY_MS: u64 = 30_000;
/// Gaze stays available for this long after the delay, then Done settles back
/// into the idle sitting pose.
const DONE_GAZE_WINDOW_MS: u64 = 60_000;

/// Closest possible delay between ambient idle actions, in milliseconds.
const IDLE_ACTION_MIN_MS: u64 = 15_000;
const IDLE_ACTION_RANGE_MS: u64 = 15_001;
const IDLE_ACTIONS: [Action; 2] = [Action::Waving, Action::Jumping];

/// Hover reactions play the jumping row this many times before settling into
/// the idle loop, matching Codex's pet component.
const HOVER_JUMPS: u64 = 3;

const DEFAULT_SPRITESHEET: &str = "spritesheet.webp";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PetManifest {
    pub id: String,
    pub display_name: String,
    /// Part of the Codex manifest; not shown in the UI yet.
    #[allow(dead_code)]
    #[serde(default)]
    pub description: String,
    /// Look rows are detected from the atlas height, so this is informational.
    #[allow(dead_code)]
    #[serde(default = "default_sprite_version")]
    pub sprite_version_number: u32,
    #[serde(default = "default_spritesheet_path")]
    pub spritesheet_path: String,
}

fn default_sprite_version() -> u32 {
    1
}

fn default_spritesheet_path() -> String {
    DEFAULT_SPRITESHEET.to_owned()
}

#[derive(Debug, Clone)]
pub struct Atlas {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8, row-major, top-left origin.
    pub rgba: Vec<u8>,
}

impl Atlas {
    pub fn rows(&self) -> u32 {
        self.height / CELL_H
    }

    /// Look rows 9-10 exist only in 11-row (or taller) atlases.
    pub fn gaze_enabled(&self) -> bool {
        self.rows() >= LOOK_ROWS
    }

    /// `[u0, v0, u1, v1]` for a cell, with v measured from the top.
    pub fn cell_uv(&self, row: u32, col: u32) -> [f32; 4] {
        let u0 = (col * CELL_W) as f32 / self.width as f32;
        let v0 = (row * CELL_H) as f32 / self.height as f32;
        let u1 = ((col + 1) * CELL_W) as f32 / self.width as f32;
        let v1 = ((row + 1) * CELL_H) as f32 / self.height as f32;
        [u0, v0, u1, v1]
    }

    /// Alpha of the pixel under `(x, y)` in a view that displays the whole
    /// cell. Used for click-through hit testing.
    pub fn alpha_at_view(
        &self,
        row: u32,
        col: u32,
        x: f64,
        y: f64,
        view_w: f64,
        view_h: f64,
    ) -> Option<u8> {
        if view_w <= 0.0 || view_h <= 0.0 || x < 0.0 || y < 0.0 || x >= view_w || y >= view_h {
            return None;
        }
        if row >= self.rows() || col >= ATLAS_COLS {
            return None;
        }
        let cell_x = ((x / view_w * f64::from(CELL_W)) as u32).min(CELL_W - 1);
        let cell_y = ((y / view_h * f64::from(CELL_H)) as u32).min(CELL_H - 1);
        let index =
            (((row * CELL_H + cell_y) * self.width + col * CELL_W + cell_x) * 4 + 3) as usize;
        self.rgba.get(index).copied()
    }
}

#[derive(Debug, Clone)]
pub struct Pet {
    pub manifest: PetManifest,
    pub dir: PathBuf,
    pub atlas: Atlas,
}

pub fn load_manifest(dir: &Path) -> Result<PetManifest> {
    let manifest_path = dir.join("pet.json");
    let text = fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", manifest_path.display()))
}

pub fn load_pet(dir: &Path) -> Result<Pet> {
    let manifest = load_manifest(dir)?;
    let sprite_path = resolve_spritesheet(dir, &manifest.spritesheet_path)?;
    let len = fs::metadata(&sprite_path)
        .with_context(|| format!("statting {}", sprite_path.display()))?
        .len();
    if len > MAX_SPRITESHEET_BYTES {
        bail!(
            "spritesheet {} is {} bytes (limit {})",
            sprite_path.display(),
            len,
            MAX_SPRITESHEET_BYTES
        );
    }
    let bytes =
        fs::read(&sprite_path).with_context(|| format!("reading {}", sprite_path.display()))?;
    let atlas = decode_atlas(&bytes)?;
    if !atlas.gaze_enabled() {
        eprintln!(
            "herdr-pet: {} has {} rows; look rows disabled",
            sprite_path.display(),
            atlas.rows()
        );
    }

    Ok(Pet {
        manifest,
        dir: dir.to_path_buf(),
        atlas,
    })
}

/// `$CODEX_HOME/pets`, defaulting to `~/.codex/pets`.
pub fn pets_dir() -> Result<PathBuf> {
    let codex_home = std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
        .context("cannot locate the Codex home directory")?;
    Ok(codex_home.join("pets"))
}

pub fn pet_dirs() -> Result<Vec<PathBuf>> {
    let dir = pets_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut dirs: Vec<PathBuf> = fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    Ok(dirs)
}

/// Load the first pet that decodes cleanly, skipping broken candidates.
pub fn load_first_pet() -> Result<Pet> {
    let dirs = pet_dirs()?;
    for dir in dirs {
        match load_pet(&dir) {
            Ok(pet) => return Ok(pet),
            Err(error) => eprintln!("herdr-pet: skipping {}: {error:#}", dir.display()),
        }
    }
    bail!("no loadable pet found in {}", pets_dir()?.display())
}

/// Resolve `spritesheetPath` inside the pet directory, rejecting absolute
/// paths, traversal, and symlinks that escape it.
fn resolve_spritesheet(dir: &Path, rel: &str) -> Result<PathBuf> {
    if rel.is_empty() {
        bail!("spritesheetPath is empty");
    }
    let rel = Path::new(rel);
    if rel.is_absolute() {
        bail!("spritesheetPath must be relative: {}", rel.display());
    }
    let dir = dir
        .canonicalize()
        .with_context(|| format!("resolving {}", dir.display()))?;
    let path = dir
        .join(rel)
        .canonicalize()
        .with_context(|| format!("resolving spritesheet {rel:?}"))?;
    if !path.starts_with(&dir) {
        bail!(
            "spritesheetPath escapes the pet directory: {}",
            rel.display()
        );
    }
    Ok(path)
}

fn decode_atlas(bytes: &[u8]) -> Result<Atlas> {
    // Validate the header before decoding so a decompression bomb cannot
    // make us allocate a huge buffer first.
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("recognizing spritesheet format")?;
    let (width, height) = reader
        .into_dimensions()
        .context("reading spritesheet size")?;
    validate_dimensions(width, height)?;

    let image = image::load_from_memory(bytes).context("decoding spritesheet")?;
    let rgba = image.to_rgba8();
    if rgba.dimensions() != (width, height) {
        bail!("spritesheet dimensions changed while decoding");
    }
    Ok(Atlas {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

fn validate_dimensions(width: u32, height: u32) -> Result<()> {
    if width != ATLAS_COLS * CELL_W {
        bail!(
            "spritesheet width {width} (expected {})",
            ATLAS_COLS * CELL_W
        );
    }
    if !height.is_multiple_of(CELL_H) || height / CELL_H < MIN_ROWS {
        bail!(
            "spritesheet height {height} (expected a multiple of {CELL_H}, at least {} rows)",
            MIN_ROWS
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mood {
    Idle,
    Working,
    Blocked,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragDir {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Idle,
    RunningRight,
    RunningLeft,
    Waving,
    Jumping,
    Failed,
    Waiting,
    Running,
}

impl Action {
    pub fn row(self) -> u32 {
        match self {
            Action::Idle => 0,
            Action::RunningRight => 1,
            Action::RunningLeft => 2,
            Action::Waving => 3,
            Action::Jumping => 4,
            Action::Failed => 5,
            Action::Waiting => 6,
            Action::Running => 7,
        }
    }

    pub fn durations(self) -> &'static [u32] {
        match self {
            Action::Idle => &[1680, 660, 660, 840, 840, 1920],
            Action::RunningRight | Action::RunningLeft => &[120, 120, 120, 120, 120, 120, 120, 220],
            Action::Waving => &[140, 140, 140, 280],
            Action::Jumping => &[140, 140, 140, 140, 280],
            Action::Failed => &[140, 140, 140, 140, 140, 140, 140, 240],
            Action::Waiting => &[150, 150, 150, 150, 150, 260],
            Action::Running => &[120, 120, 120, 120, 120, 220],
        }
    }

    pub fn entry_for(mood: Mood) -> Self {
        match mood {
            Mood::Idle => Action::Idle,
            Mood::Working => Action::Running,
            Mood::Blocked => Action::Waving,
            Mood::Done => Action::Jumping,
            Mood::Failed => Action::Failed,
        }
    }

    /// Status-entry rows that hand over to their looping row. Waving and
    /// jumping only appear as entries for Blocked and Done; ambient idle
    /// gestures and hover reactions are tracked separately.
    fn next_after_one_shot(self) -> Option<Self> {
        match self {
            Action::Waving => Some(Action::Waiting),
            Action::Jumping => Some(Action::Idle),
            _ => None,
        }
    }

    fn total_ms(self) -> u64 {
        self.durations().iter().map(|&ms| u64::from(ms)).sum()
    }
}

/// Column index for `elapsed_ms` within the action's timeline.
///
/// Looping actions wrap. If `one_shot` is true, clamp to the last frame until
/// the animator advances the row.
pub fn frame_index(action: Action, elapsed_ms: u64, one_shot: bool) -> usize {
    let durations = action.durations();
    let total = action.total_ms();
    let elapsed = if one_shot {
        elapsed_ms.min(total - 1)
    } else {
        elapsed_ms % total
    };
    let mut acc = 0;
    for (index, &duration) in durations.iter().enumerate() {
        acc += u64::from(duration);
        if elapsed < acc {
            return index;
        }
    }
    durations.len() - 1
}

/// A gaze target cell: 0-7 in look row 9, 8-15 in look row 10.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GazeCell(u8);

impl GazeCell {
    pub fn row(self) -> u32 {
        if self.0 < 8 { 9 } else { 10 }
    }

    pub fn col(self) -> u32 {
        u32::from(self.0 % 8)
    }
}

/// Pick the look cell for a vector from the pet's center to the mouse, in
/// top-left-origin screen pixels (y grows downward). 000 degrees is up.
pub fn gaze_cell(dx: f32, dy: f32) -> Option<GazeCell> {
    if dx * dx + dy * dy < GAZE_DEAD_ZONE * GAZE_DEAD_ZONE {
        return None;
    }
    let degrees = dx.atan2(-dy).to_degrees().rem_euclid(360.0);
    let cell = (((degrees + 11.25) / 22.5).floor() as u32 % 16) as u8;
    Some(GazeCell(cell))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimInput {
    pub mood: Mood,
    pub dragging: Option<DragDir>,
    pub gaze: Option<GazeCell>,
    pub hover: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    pub row: u32,
    pub col: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Active {
    Base,
    Drag(DragDir),
    Hover,
    Look,
}

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        // xorshift64*: enough for non-security ambient choices, deterministic in tests.
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
}

enum Base {
    Mood,
    Ambient(Action),
}

pub struct Animator {
    look_enabled: bool,
    mood: Mood,
    action: Action,
    row_started_ms: u64,
    active: Active,
    base: Base,
    random: Random,
    next_idle_action_ms: u64,
    /// Set once Done's one-shot jump has handed over to the sitting pose.
    done_gaze_ready: bool,
    /// First tick that observed a continuous Done; anchors the gaze delay and
    /// window. Cleared as soon as another mood is observed.
    done_since_ms: Option<u64>,
}

impl Animator {
    pub fn new(look_enabled: bool) -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos() as u64);
        Self::with_seed(look_enabled, seed)
    }

    /// Toggle gaze rows at runtime, e.g. from the settings panel.
    pub fn set_look_enabled(&mut self, enabled: bool) {
        self.look_enabled = enabled;
    }

    fn with_seed(look_enabled: bool, seed: u64) -> Self {
        let mut animator = Self {
            look_enabled,
            mood: Mood::Idle,
            action: Action::Idle,
            row_started_ms: 0,
            active: Active::Base,
            base: Base::Mood,
            random: Random(seed),
            next_idle_action_ms: 0,
            done_gaze_ready: false,
            done_since_ms: None,
        };
        animator.schedule_idle_action(0);
        animator
    }

    fn schedule_idle_action(&mut self, now_ms: u64) {
        self.next_idle_action_ms = now_ms
            .saturating_add(IDLE_ACTION_MIN_MS)
            .saturating_add(self.random.next() % IDLE_ACTION_RANGE_MS);
    }

    fn enter_action(&mut self, action: Action, now_ms: u64) {
        self.action = action;
        self.row_started_ms = now_ms;
    }

    fn begin_idle(&mut self, now_ms: u64) {
        self.base = Base::Mood;
        self.enter_action(Action::Idle, now_ms);
        self.schedule_idle_action(now_ms);
    }

    fn enter_mood(&mut self, mood: Mood, now_ms: u64) {
        self.done_gaze_ready = false;
        self.mood = mood;
        if mood == Mood::Idle {
            self.begin_idle(now_ms);
        } else {
            self.base = Base::Mood;
            self.enter_action(Action::entry_for(mood), now_ms);
        }
    }

    /// Look rows are available while Idle and, once the one-shot jump has
    /// finished, during the 60 s window that opens 30 s after Done began.
    fn can_look(&self, mood: Mood, now_ms: u64) -> bool {
        if !self.look_enabled {
            return false;
        }
        match mood {
            Mood::Idle => true,
            Mood::Done => {
                self.mood == Mood::Done
                    && self.done_gaze_ready
                    && self.done_since_ms.is_some_and(|since| {
                        (DONE_GAZE_DELAY_MS..DONE_GAZE_DELAY_MS + DONE_GAZE_WINDOW_MS)
                            .contains(&now_ms.saturating_sub(since))
                    })
            }
            _ => false,
        }
    }

    pub fn update(&mut self, now_ms: u64, input: AnimInput) -> Frame {
        if input.mood == Mood::Done {
            self.done_since_ms.get_or_insert(now_ms);
        } else {
            self.done_since_ms = None;
            self.done_gaze_ready = false;
        }
        let active = match (input.dragging, input.hover, input.gaze) {
            (Some(dir), _, _) => Active::Drag(dir),
            (None, true, _) => Active::Hover,
            (None, false, Some(_)) if self.can_look(input.mood, now_ms) => Active::Look,
            _ => Active::Base,
        };

        if active != self.active {
            let leaving_look = self.active == Active::Look;
            self.active = active;
            self.row_started_ms = now_ms;
            if active == Active::Base {
                if leaving_look && input.mood == Mood::Done && self.done_gaze_ready {
                    // Done already jumped once; leaving its sitting gaze must
                    // not replay the one-shot reminder.
                    self.begin_idle(now_ms);
                } else {
                    self.enter_mood(input.mood, now_ms);
                }
            }
        }

        match active {
            Active::Look => {
                let cell = input.gaze.expect("look mode requires a gaze cell");
                Frame {
                    row: cell.row(),
                    col: cell.col(),
                }
            }
            Active::Drag(dir) => {
                self.action = match dir {
                    DragDir::Left => Action::RunningLeft,
                    DragDir::Right => Action::RunningRight,
                };
                self.frame_at(now_ms)
            }
            Active::Hover => {
                let jump = Action::Jumping.total_ms();
                let elapsed = now_ms.saturating_sub(self.row_started_ms);
                if elapsed < HOVER_JUMPS * jump {
                    Frame {
                        row: Action::Jumping.row(),
                        col: frame_index(Action::Jumping, elapsed % jump, false) as u32,
                    }
                } else {
                    Frame {
                        row: Action::Idle.row(),
                        col: frame_index(Action::Idle, elapsed - HOVER_JUMPS * jump, false) as u32,
                    }
                }
            }
            Active::Base => {
                if input.mood != self.mood {
                    self.enter_mood(input.mood, now_ms);
                }
                self.advance_base(now_ms);
                self.frame_at(now_ms)
            }
        }
    }

    fn advance_base(&mut self, now_ms: u64) {
        match self.base {
            Base::Mood => {
                if let Some(next) = self.action.next_after_one_shot() {
                    let total = self.action.total_ms();
                    if now_ms.saturating_sub(self.row_started_ms) >= total {
                        self.action = next;
                        self.row_started_ms += total;
                        if self.mood == Mood::Done && next == Action::Idle {
                            self.done_gaze_ready = true;
                        }
                    }
                }
                if self.mood == Mood::Idle
                    && self.action == Action::Idle
                    && now_ms >= self.next_idle_action_ms
                {
                    let choice =
                        IDLE_ACTIONS[(self.random.next() % IDLE_ACTIONS.len() as u64) as usize];
                    self.base = Base::Ambient(choice);
                    self.enter_action(choice, now_ms);
                }
            }
            Base::Ambient(gesture) => {
                if now_ms.saturating_sub(self.row_started_ms) >= gesture.total_ms() {
                    self.begin_idle(self.row_started_ms + gesture.total_ms());
                }
            }
        }
    }

    fn frame_at(&self, now_ms: u64) -> Frame {
        let elapsed = now_ms.saturating_sub(self.row_started_ms);
        let one_shot =
            self.action.next_after_one_shot().is_some() || matches!(self.base, Base::Ambient(_));
        Frame {
            row: self.action.row(),
            col: frame_index(self.action, elapsed, one_shot) as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "herdr-pet-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_manifest(dir: &Path, json: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("pet.json"), json).unwrap();
    }

    fn write_transparent_png(path: &Path, rows: u32) {
        let image = image::RgbaImage::from_pixel(
            ATLAS_COLS * CELL_W,
            rows * CELL_H,
            image::Rgba([0, 0, 0, 0]),
        );
        image
            .save_with_format(path, image::ImageFormat::Png)
            .unwrap();
    }

    fn input(mood: Mood, dragging: Option<DragDir>, gaze: Option<GazeCell>) -> AnimInput {
        AnimInput {
            mood,
            dragging,
            gaze,
            hover: false,
        }
    }

    fn hover_input(mood: Mood, gaze: Option<GazeCell>) -> AnimInput {
        AnimInput {
            mood,
            dragging: None,
            gaze,
            hover: true,
        }
    }

    #[test]
    fn manifest_defaults_and_unknown_fields() {
        let manifest: PetManifest = serde_json::from_str(
            r#"{"id":"diana","displayName":"Diana","description":"d","kind":"person"}"#,
        )
        .unwrap();
        assert_eq!(manifest.id, "diana");
        assert_eq!(manifest.display_name, "Diana");
        assert_eq!(manifest.sprite_version_number, 1);
        assert_eq!(manifest.spritesheet_path, "spritesheet.webp");
    }

    #[test]
    fn manifest_requires_id_and_display_name() {
        assert!(serde_json::from_str::<PetManifest>(r#"{"id":"x"}"#).is_err());
        assert!(serde_json::from_str::<PetManifest>(r#"{"displayName":"X"}"#).is_err());
    }

    #[test]
    fn validates_atlas_dimensions() {
        assert!(validate_dimensions(1536, 1872).is_ok());
        assert!(validate_dimensions(1536, 2288).is_ok());
        assert!(validate_dimensions(1536, 2496).is_ok());
        assert!(validate_dimensions(1535, 1872).is_err());
        assert!(validate_dimensions(1536, 1873).is_err());
        assert!(validate_dimensions(1536, 1664).is_err());
    }

    #[test]
    fn look_rows_enabled_from_eleven_rows() {
        let atlas = Atlas {
            width: 1536,
            height: 11 * CELL_H,
            rgba: Vec::new(),
        };
        assert!(atlas.gaze_enabled());
        assert_eq!(atlas.rows(), 11);

        let atlas = Atlas {
            width: 1536,
            height: 9 * CELL_H,
            rgba: Vec::new(),
        };
        assert!(!atlas.gaze_enabled());
    }

    #[test]
    fn cell_uv_matches_atlas_grid() {
        let atlas = Atlas {
            width: 1536,
            height: 11 * CELL_H,
            rgba: Vec::new(),
        };
        assert_eq!(atlas.cell_uv(0, 0), [0.0, 0.0, 0.125, 1.0 / 11.0]);
        let [u0, v0, u1, v1] = atlas.cell_uv(9, 7);
        assert!((u0 - 7.0 / 8.0).abs() < 1e-6);
        assert!((v0 - 9.0 / 11.0).abs() < 1e-6);
        assert!((u1 - 1.0).abs() < 1e-6);
        assert!((v1 - 10.0 / 11.0).abs() < 1e-6);
    }

    #[test]
    fn loads_pet_and_decodes_atlas() {
        let temp = TempDir::new();
        let dir = temp.path().join("diana");
        write_manifest(
            &dir,
            r#"{"id":"diana","displayName":"Diana","spritesheetPath":"sheet.png"}"#,
        );
        write_transparent_png(&dir.join("sheet.png"), MIN_ROWS);

        let pet = load_pet(&dir).unwrap();
        assert_eq!(pet.manifest.id, "diana");
        assert_eq!(pet.atlas.width, 1536);
        assert_eq!(pet.atlas.height, 1872);
        assert_eq!(pet.atlas.rows(), 9);
        assert!(!pet.atlas.gaze_enabled());
        assert_eq!(pet.atlas.rgba.len(), (1536 * 1872 * 4) as usize);
    }

    #[test]
    fn rejects_path_traversal() {
        let temp = TempDir::new();
        let dir = temp.path().join("pet");
        write_manifest(
            &dir,
            r#"{"id":"x","displayName":"X","spritesheetPath":"../evil.png"}"#,
        );
        fs::write(temp.path().join("evil.png"), b"not an image").unwrap();
        assert!(load_pet(&dir).is_err());
    }

    #[test]
    fn rejects_absolute_spritesheet_path() {
        let temp = TempDir::new();
        let dir = temp.path().join("pet");
        write_manifest(
            &dir,
            r#"{"id":"x","displayName":"X","spritesheetPath":"/etc/passwd"}"#,
        );
        assert!(load_pet(&dir).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escaping_pet_directory() {
        let temp = TempDir::new();
        let dir = temp.path().join("pet");
        write_manifest(
            &dir,
            r#"{"id":"x","displayName":"X","spritesheetPath":"link.png"}"#,
        );
        let outside = temp.path().join("outside.png");
        fs::write(&outside, b"not an image").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("link.png")).unwrap();
        assert!(load_pet(&dir).is_err());
    }

    #[test]
    fn rejects_oversized_spritesheet() {
        let temp = TempDir::new();
        let dir = temp.path().join("pet");
        write_manifest(
            &dir,
            r#"{"id":"x","displayName":"X","spritesheetPath":"big.webp"}"#,
        );
        fs::File::create(dir.join("big.webp"))
            .unwrap()
            .set_len(MAX_SPRITESHEET_BYTES + 1)
            .unwrap();
        assert!(load_pet(&dir).is_err());
    }

    #[test]
    fn frame_index_boundaries() {
        // Idle loops over 6.6 s.
        assert_eq!(frame_index(Action::Idle, 0, false), 0);
        assert_eq!(frame_index(Action::Idle, 1679, false), 0);
        assert_eq!(frame_index(Action::Idle, 1680, false), 1);
        assert_eq!(frame_index(Action::Idle, 6599, false), 5);
        assert_eq!(frame_index(Action::Idle, 6600, false), 0);

        // Running loops over 820 ms.
        assert_eq!(frame_index(Action::Running, 0, false), 0);
        assert_eq!(frame_index(Action::Running, 119, false), 0);
        assert_eq!(frame_index(Action::Running, 120, false), 1);
        assert_eq!(frame_index(Action::Running, 600, false), 5);
        assert_eq!(frame_index(Action::Running, 819, false), 5);
        assert_eq!(frame_index(Action::Running, 820, false), 0);

        // One-shots clamp on their last frame.
        assert_eq!(frame_index(Action::Waving, 0, true), 0);
        assert_eq!(frame_index(Action::Waving, 139, true), 0);
        assert_eq!(frame_index(Action::Waving, 140, true), 1);
        assert_eq!(frame_index(Action::Waving, 699, true), 3);
        assert_eq!(frame_index(Action::Waving, 700, true), 3);
        assert_eq!(frame_index(Action::Waving, 5000, true), 3);
        // The same gesture can loop when it is used as a blocked entry action.
        assert_eq!(frame_index(Action::Waving, 5000, false), 0);
    }

    #[test]
    fn idle_schedules_ambient_actions_with_bounded_random_delays() {
        for (seed, delay, row) in [(0, 15_000, 3), (1, 16_748, 4)] {
            let mut animator = Animator::with_seed(false, seed);
            assert_eq!(animator.next_idle_action_ms, delay);
            let expected_idle = frame_index(Action::Idle, delay - 1, false) as u32;
            assert_eq!(
                animator.update(delay - 1, input(Mood::Idle, None, None)),
                Frame {
                    row: 0,
                    col: expected_idle
                }
            );
            assert_eq!(
                animator.update(delay, input(Mood::Idle, None, None)),
                Frame { row, col: 0 }
            );
        }
    }

    #[test]
    fn idle_random_delay_includes_thirty_second_upper_bound() {
        for seed in 0..100_000 {
            let mut animator = Animator::with_seed(false, seed);
            if animator.next_idle_action_ms == 30_000 {
                assert_ne!(
                    animator.update(30_000, input(Mood::Idle, None, None)).row,
                    0
                );
                return;
            }
        }
        panic!("no seed exercised the upper random-delay bound");
    }

    #[test]
    fn ambient_actions_return_to_idle_and_reschedule() {
        let mut animator = Animator::with_seed(false, 0);
        let due = animator.next_idle_action_ms;
        assert_eq!(
            animator.update(due, input(Mood::Idle, None, None)),
            Frame { row: 3, col: 0 }
        );
        let end = due + Action::Waving.total_ms();
        assert_eq!(
            animator.update(end, input(Mood::Idle, None, None)),
            Frame { row: 0, col: 0 }
        );
        assert_eq!(
            animator.update(end + IDLE_ACTION_MIN_MS - 1, input(Mood::Idle, None, None)),
            Frame {
                row: 0,
                col: frame_index(Action::Idle, IDLE_ACTION_MIN_MS - 1, false) as u32
            }
        );
        assert!(animator.next_idle_action_ms >= end + IDLE_ACTION_MIN_MS);
        assert!(animator.next_idle_action_ms <= end + IDLE_ACTION_MIN_MS + 15_000);
    }

    #[test]
    fn non_idle_moods_keep_their_distinct_animations() {
        for (mood, entry_row, settled_row) in [
            (Mood::Working, 7, 7),
            (Mood::Blocked, 3, 6),
            (Mood::Done, 4, 0),
            (Mood::Failed, 5, 5),
        ] {
            let mut animator = Animator::with_seed(false, 0);
            assert_eq!(animator.update(0, input(mood, None, None)).row, entry_row);
            assert_eq!(
                animator.update(40_000, input(mood, None, None)).row,
                settled_row
            );
        }
    }

    #[test]
    fn gaze_interrupts_ambient_action_and_restarts_idle_timer() {
        let mut animator = Animator::with_seed(true, 0);
        let due = animator.next_idle_action_ms;
        assert_eq!(
            animator.update(due, input(Mood::Idle, None, None)),
            Frame { row: 3, col: 0 }
        );
        let cell = gaze_cell(100.0, 0.0).unwrap();
        assert_eq!(
            animator.update(16_000, input(Mood::Idle, None, Some(cell))),
            Frame { row: 9, col: 4 }
        );
        assert_eq!(
            animator.update(16_100, input(Mood::Idle, None, None)),
            Frame { row: 0, col: 0 }
        );
        let idle_since = animator.row_started_ms;
        assert!(animator.next_idle_action_ms >= idle_since + IDLE_ACTION_MIN_MS);
        assert!(animator.next_idle_action_ms <= idle_since + 30_000);
        let next = animator.next_idle_action_ms;
        let gesture = animator.update(next, input(Mood::Idle, None, None));
        assert_ne!(gesture.row, 0);
        assert_eq!(gesture.col, 0);
        assert_eq!(
            animator.update(next + 140, input(Mood::Idle, None, None)),
            Frame {
                row: gesture.row,
                col: 1
            }
        );
    }

    #[test]
    fn drag_interrupts_idle_and_restarts_idle_timer() {
        let mut animator = Animator::with_seed(false, 0);
        animator.update(1_000, input(Mood::Idle, None, None));
        animator.update(1_100, input(Mood::Idle, Some(DragDir::Right), None));
        assert_eq!(
            animator.update(1_200, input(Mood::Idle, None, None)),
            Frame { row: 0, col: 0 }
        );
        assert_eq!(animator.next_idle_action_ms, 16_200);
    }

    #[test]
    fn blocked_plays_waving_then_waiting() {
        let mut animator = Animator::new(true);
        assert_eq!(
            animator.update(0, input(Mood::Blocked, None, None)),
            Frame { row: 3, col: 0 }
        );
        assert_eq!(
            animator.update(699, input(Mood::Blocked, None, None)),
            Frame { row: 3, col: 3 }
        );
        assert_eq!(
            animator.update(700, input(Mood::Blocked, None, None)),
            Frame { row: 6, col: 0 }
        );
        assert_eq!(
            animator.update(850, input(Mood::Blocked, None, None)),
            Frame { row: 6, col: 1 }
        );
    }

    #[test]
    fn done_plays_jumping_then_idle() {
        let mut animator = Animator::new(true);
        assert_eq!(
            animator.update(0, input(Mood::Done, None, None)),
            Frame { row: 4, col: 0 }
        );
        assert_eq!(
            animator.update(839, input(Mood::Done, None, None)),
            Frame { row: 4, col: 4 }
        );
        assert_eq!(
            animator.update(840, input(Mood::Done, None, None)),
            Frame { row: 0, col: 0 }
        );
        assert_eq!(
            animator.update(2520, input(Mood::Done, None, None)),
            Frame { row: 0, col: 1 }
        );
    }

    #[test]
    fn done_gaze_opens_after_thirty_seconds_and_closes_after_one_minute() {
        let mut animator = Animator::with_seed(true, 0);
        let cell = gaze_cell(100.0, 0.0).unwrap();

        // The one-shot jump plays, then the sitting pose ignores the mouse for
        // the first 30 seconds.
        assert_eq!(
            animator.update(0, input(Mood::Done, None, Some(cell))),
            Frame { row: 4, col: 0 }
        );
        assert_eq!(
            animator.update(840, input(Mood::Done, None, Some(cell))),
            Frame { row: 0, col: 0 }
        );
        assert_eq!(
            animator.update(29_999, input(Mood::Done, None, Some(cell))),
            Frame {
                row: 0,
                col: frame_index(Action::Idle, 29_159, false) as u32
            }
        );

        // The 60-second gaze window opens at 30 s.
        assert_eq!(
            animator.update(30_000, input(Mood::Done, None, Some(cell))),
            Frame { row: 9, col: 4 }
        );
        // Leaving and re-entering inside the window is allowed.
        assert_eq!(
            animator.update(31_000, input(Mood::Done, None, None)),
            Frame { row: 0, col: 0 }
        );
        assert_eq!(
            animator.update(32_000, input(Mood::Done, None, Some(cell))),
            Frame { row: 9, col: 4 }
        );
        assert_eq!(
            animator.update(89_999, input(Mood::Done, None, Some(cell))),
            Frame { row: 9, col: 4 }
        );

        // At 90 s the window closes and the pet sits, still ignoring the mouse.
        assert_eq!(
            animator.update(90_000, input(Mood::Done, None, Some(cell))),
            Frame { row: 0, col: 0 }
        );
        assert_eq!(
            animator.update(93_000, input(Mood::Done, None, Some(cell))),
            Frame {
                row: 0,
                col: frame_index(Action::Idle, 3_000, false) as u32
            }
        );
    }

    #[test]
    fn done_entry_is_not_skipped_by_gaze() {
        let mut animator = Animator::with_seed(true, 0);
        let cell = gaze_cell(100.0, 0.0).unwrap();

        // Idle gaze is active when Done arrives; the jump still plays in full.
        assert_eq!(
            animator.update(0, input(Mood::Idle, None, Some(cell))),
            Frame { row: 9, col: 4 }
        );
        assert_eq!(
            animator.update(16, input(Mood::Done, None, Some(cell))),
            Frame { row: 4, col: 0 }
        );
        assert_eq!(
            animator.update(856, input(Mood::Done, None, Some(cell))),
            Frame { row: 0, col: 0 }
        );
        // The window is anchored to the first Done observation (16 ms).
        assert_eq!(
            animator.update(30_015, input(Mood::Done, None, Some(cell))),
            Frame {
                row: 0,
                col: frame_index(Action::Idle, 30_015 - 856, false) as u32
            }
        );
        assert_eq!(
            animator.update(30_016, input(Mood::Done, None, Some(cell))),
            Frame { row: 9, col: 4 }
        );
    }

    #[test]
    fn new_done_while_gazing_replays_jumping_and_restarts_the_window() {
        let mut animator = Animator::with_seed(true, 0);
        let cell = gaze_cell(100.0, 0.0).unwrap();

        assert_eq!(
            animator.update(0, input(Mood::Done, None, Some(cell))),
            Frame { row: 4, col: 0 }
        );
        assert_eq!(
            animator.update(840, input(Mood::Done, None, Some(cell))),
            Frame { row: 0, col: 0 }
        );
        assert_eq!(
            animator.update(30_000, input(Mood::Done, None, Some(cell))),
            Frame { row: 9, col: 4 }
        );

        // Done -> Idle keeps gazing; a new Done is a new completion, so the
        // jump plays again and the 30 s delay restarts.
        assert_eq!(
            animator.update(31_000, input(Mood::Idle, None, Some(cell))),
            Frame { row: 9, col: 4 }
        );
        assert_eq!(
            animator.update(31_016, input(Mood::Done, None, Some(cell))),
            Frame { row: 4, col: 0 }
        );
        assert_eq!(
            animator.update(31_856, input(Mood::Done, None, Some(cell))),
            Frame { row: 0, col: 0 }
        );
        assert_eq!(
            animator.update(61_015, input(Mood::Done, None, Some(cell))),
            Frame {
                row: 0,
                col: frame_index(Action::Idle, 29_159, false) as u32
            }
        );
        assert_eq!(
            animator.update(61_016, input(Mood::Done, None, Some(cell))),
            Frame { row: 9, col: 4 }
        );
    }

    #[test]
    fn hover_does_not_reset_the_done_gaze_window() {
        let mut animator = Animator::with_seed(true, 0);
        let cell = gaze_cell(100.0, 0.0).unwrap();

        // Done arrives while the mouse is hovering; the timer starts anyway.
        assert_eq!(
            animator.update(0, hover_input(Mood::Done, Some(cell))),
            Frame { row: 4, col: 0 }
        );
        // Leaving hover replays the entry jump once, but the window still
        // counts from Done's first observation.
        assert_eq!(
            animator.update(40_000, input(Mood::Done, None, Some(cell))),
            Frame { row: 4, col: 0 }
        );
        assert_eq!(
            animator.update(40_840, input(Mood::Done, None, Some(cell))),
            Frame { row: 0, col: 0 }
        );
        assert_eq!(
            animator.update(40_856, input(Mood::Done, None, Some(cell))),
            Frame { row: 9, col: 4 }
        );
        // The window still closes 90 s after the first observation.
        assert_eq!(
            animator.update(90_000, input(Mood::Done, None, Some(cell))),
            Frame { row: 0, col: 0 }
        );
    }

    #[test]
    fn done_without_look_rows_stays_sitting() {
        let mut animator = Animator::with_seed(false, 0);
        let cell = gaze_cell(100.0, 0.0).unwrap();

        assert_eq!(
            animator.update(0, input(Mood::Done, None, Some(cell))),
            Frame { row: 4, col: 0 }
        );
        assert_eq!(
            animator.update(840, input(Mood::Done, None, Some(cell))),
            Frame { row: 0, col: 0 }
        );
        assert_eq!(
            animator.update(900, input(Mood::Done, None, Some(cell))),
            Frame { row: 0, col: 0 }
        );
    }

    #[test]
    fn hover_plays_three_jumps_then_idle() {
        let mut animator = Animator::new(true);
        assert_eq!(
            animator.update(0, hover_input(Mood::Working, None)),
            Frame { row: 4, col: 0 }
        );
        assert_eq!(
            animator.update(839, hover_input(Mood::Working, None)),
            Frame { row: 4, col: 4 }
        );
        assert_eq!(
            animator.update(840, hover_input(Mood::Working, None)),
            Frame { row: 4, col: 0 }
        );
        assert_eq!(
            animator.update(2520, hover_input(Mood::Working, None)),
            Frame { row: 0, col: 0 }
        );
        assert_eq!(
            animator.update(4200, hover_input(Mood::Working, None)),
            Frame { row: 0, col: 1 }
        );
    }

    #[test]
    fn hover_overrides_gaze_and_drag_takes_precedence() {
        let mut animator = Animator::new(true);
        let cell = gaze_cell(100.0, 0.0).unwrap();
        assert_eq!(
            animator.update(0, hover_input(Mood::Idle, Some(cell))),
            Frame { row: 4, col: 0 }
        );
        assert_eq!(
            animator.update(
                100,
                AnimInput {
                    mood: Mood::Idle,
                    dragging: Some(DragDir::Right),
                    gaze: None,
                    hover: true,
                }
            ),
            Frame { row: 1, col: 0 }
        );
    }

    #[test]
    fn leaving_hover_returns_to_mood() {
        let mut animator = Animator::with_seed(false, 0);
        assert_eq!(
            animator.update(100, hover_input(Mood::Idle, None)),
            Frame { row: 4, col: 0 }
        );
        assert_eq!(
            animator.update(1_100, input(Mood::Idle, None, None)),
            Frame { row: 0, col: 0 }
        );
        assert_eq!(animator.next_idle_action_ms, 16_100);
    }

    #[test]
    fn one_shot_carries_leftover_time() {
        // The animator was not ticked during the whole waving row. The waiting
        // row must continue from the leftover 300 ms, not restart at frame 0.
        let mut animator = Animator::new(true);
        animator.update(0, input(Mood::Blocked, None, None));
        assert_eq!(
            animator.update(1000, input(Mood::Blocked, None, None)),
            Frame { row: 6, col: 2 }
        );
    }

    #[test]
    fn mood_change_restarts_entry_animation() {
        let mut animator = Animator::new(true);
        assert_eq!(
            animator.update(0, input(Mood::Working, None, None)),
            Frame { row: 7, col: 0 }
        );
        assert_eq!(
            animator.update(500, input(Mood::Working, None, None)),
            Frame { row: 7, col: 4 }
        );
        assert_eq!(
            animator.update(600, input(Mood::Idle, None, None)),
            Frame { row: 0, col: 0 }
        );
        assert_eq!(
            animator.update(2280, input(Mood::Idle, None, None)),
            Frame { row: 0, col: 1 }
        );
    }

    #[test]
    fn drag_overrides_mood_and_switches_direction() {
        let mut animator = Animator::new(true);
        assert_eq!(
            animator.update(0, input(Mood::Blocked, Some(DragDir::Right), None)),
            Frame { row: 1, col: 0 }
        );
        assert_eq!(
            animator.update(120, input(Mood::Blocked, Some(DragDir::Right), None)),
            Frame { row: 1, col: 1 }
        );
        assert_eq!(
            animator.update(200, input(Mood::Blocked, Some(DragDir::Left), None)),
            Frame { row: 2, col: 0 }
        );
        assert_eq!(
            animator.update(300, input(Mood::Idle, None, None)),
            Frame { row: 0, col: 0 }
        );
    }

    #[test]
    fn gaze_cell_maps_basic_directions() {
        assert_eq!(
            gaze_cell(0.0, -100.0).map(|cell| (cell.row(), cell.col())),
            Some((9, 0))
        );
        assert_eq!(
            gaze_cell(70.7, -70.7).map(|cell| (cell.row(), cell.col())),
            Some((9, 2))
        );
        assert_eq!(
            gaze_cell(100.0, 0.0).map(|cell| (cell.row(), cell.col())),
            Some((9, 4))
        );
        assert_eq!(
            gaze_cell(70.7, 70.7).map(|cell| (cell.row(), cell.col())),
            Some((9, 6))
        );
        assert_eq!(
            gaze_cell(0.0, 100.0).map(|cell| (cell.row(), cell.col())),
            Some((10, 0))
        );
        assert_eq!(
            gaze_cell(-70.7, 70.7).map(|cell| (cell.row(), cell.col())),
            Some((10, 2))
        );
        assert_eq!(
            gaze_cell(-100.0, 0.0).map(|cell| (cell.row(), cell.col())),
            Some((10, 4))
        );
        assert_eq!(
            gaze_cell(-70.7, -70.7).map(|cell| (cell.row(), cell.col())),
            Some((10, 6))
        );
    }

    #[test]
    fn gaze_cell_rounds_to_all_sixteen_cells() {
        for cell in 0..16u8 {
            let degrees = f32::from(cell) * 22.5;
            let (sin, cos) = degrees.to_radians().sin_cos();
            let picked = gaze_cell(sin * 100.0, -cos * 100.0).unwrap();
            let index = if picked.row() == 9 {
                picked.col()
            } else {
                8 + picked.col()
            };
            assert_eq!(index, u32::from(cell), "cell {cell}");
            assert_eq!(picked.row(), if cell < 8 { 9 } else { 10 });
            assert_eq!(picked.col(), u32::from(cell % 8));
        }
    }

    #[test]
    fn gaze_dead_zone_falls_back() {
        assert_eq!(gaze_cell(0.0, 0.0), None);
        assert_eq!(gaze_cell(16.0, 16.0), None);
        assert!(gaze_cell(17.0, 17.0).is_some());
        assert_eq!(gaze_cell(23.0, 0.0), None);
        assert!(gaze_cell(24.0, 0.0).is_some());
    }

    #[test]
    fn idle_gaze_uses_look_rows() {
        let mut animator = Animator::new(true);
        let cell = gaze_cell(0.0, 100.0).unwrap();
        assert_eq!(
            animator.update(0, input(Mood::Idle, None, Some(cell))),
            Frame { row: 10, col: 0 }
        );
    }

    #[test]
    fn gaze_is_ignored_when_not_idle_or_disabled() {
        let cell = gaze_cell(100.0, 0.0).unwrap();

        let mut animator = Animator::new(true);
        assert_eq!(
            animator.update(0, input(Mood::Working, None, Some(cell))),
            Frame { row: 7, col: 0 }
        );

        let mut animator = Animator::new(false);
        assert_eq!(
            animator.update(0, input(Mood::Idle, None, Some(cell))),
            Frame { row: 0, col: 0 }
        );
    }

    #[test]
    fn drag_wins_over_gaze() {
        let mut animator = Animator::new(true);
        let cell = gaze_cell(100.0, 0.0).unwrap();
        assert_eq!(
            animator.update(0, input(Mood::Idle, Some(DragDir::Right), Some(cell))),
            Frame { row: 1, col: 0 }
        );
    }

    #[test]
    fn leaving_gaze_returns_to_idle_frame_zero() {
        let mut animator = Animator::new(true);
        let cell = gaze_cell(100.0, 0.0).unwrap();
        animator.update(0, input(Mood::Idle, None, Some(cell)));
        assert_eq!(
            animator.update(100, input(Mood::Idle, None, None)),
            Frame { row: 0, col: 0 }
        );
    }

    fn one_row_atlas(alpha: u8) -> Atlas {
        let mut rgba = vec![0u8; (1536 * CELL_H * 4) as usize];
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel[3] = alpha;
        }
        Atlas {
            width: 1536,
            height: CELL_H,
            rgba,
        }
    }

    #[test]
    fn alpha_at_view_maps_window_coordinates_to_cell() {
        let mut atlas = one_row_atlas(0);
        let center = (((CELL_H / 2) * atlas.width + CELL_W / 2) * 4 + 3) as usize;
        atlas.rgba[center] = 200;
        assert_eq!(
            atlas.alpha_at_view(0, 0, 48.0, 52.0, 96.0, 104.0),
            Some(200)
        );
        assert_eq!(atlas.alpha_at_view(0, 0, 0.0, 0.0, 96.0, 104.0), Some(0));
        assert_eq!(atlas.alpha_at_view(0, 0, -1.0, 0.0, 96.0, 104.0), None);
        assert_eq!(atlas.alpha_at_view(0, 0, 96.0, 104.0, 96.0, 104.0), None);
        assert_eq!(atlas.alpha_at_view(1, 0, 0.0, 0.0, 96.0, 104.0), None);
    }
}
