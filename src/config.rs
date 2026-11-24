//! User configuration with atomic writes.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_SIZE: f64 = 104.0;
/// Allowed pet sprite heights in logical points.
pub const MIN_SIZE: f64 = 52.0;
pub const MAX_SIZE: f64 = 312.0;
/// Opacity of the chat bubble's glass layer; labels stay fully opaque.
pub const DEFAULT_CHAT_OPACITY: f64 = 0.7;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Root directory containing one subdirectory per pet. Missing means
    /// `$CODEX_HOME/pets` (or `~/.codex/pets`).
    pub pets_dir: Option<PathBuf>,
    pub pet: Option<String>,
    pub size: f64,
    /// Pet top-left in physical pixels (so expanding the bubble never moves
    /// the pet); top-left origin.
    pub position: Option<[i32; 2]>,
    pub notifications: bool,
    pub notification_sound: bool,
    pub gaze: bool,
    /// Whole-window mouse pass-through; when true the pet ignores the mouse
    /// entirely and cannot be dragged.
    pub click_through: bool,
    /// Hide the menu-bar icon. Applies only while `click_through` is off and
    /// the pet window is visible, so a mouse-reachable entry point remains.
    pub hide_tray_icon: bool,
    /// Whether the chat bubble above the pet is expanded.
    pub chat_open: bool,
    /// Opacity of the chat bubble's glass layer: 0.0 is fully transparent
    /// (text only), 1.0 is fully opaque.
    pub chat_opacity: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            pets_dir: None,
            pet: None,
            size: DEFAULT_SIZE,
            position: None,
            notifications: true,
            notification_sound: true,
            gaze: true,
            click_through: false,
            hide_tray_icon: false,
            chat_open: true,
            chat_opacity: DEFAULT_CHAT_OPACITY,
        }
    }
}

pub fn config_path() -> Result<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        // Windows keeps its per-user files under the roaming profile; the
        // milestone before this one wrote to ~/.config, so move that file
        // once instead of losing the user's settings.
        let dir = dirs::config_dir().context("cannot locate %APPDATA%")?;
        let path = dir.join("herdr-pet").join("config.json");
        if !path.exists()
            && let Some(legacy) = legacy_config_path()
        {
            migrate_legacy_config(&path, &legacy);
        }
        Ok(path)
    }
    #[cfg(not(target_os = "windows"))]
    {
        // `dirs::config_dir()` is ~/Library/Application Support on macOS; the
        // documented path is $XDG_CONFIG_HOME or ~/.config.
        let dir = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
            .context("cannot locate the user config directory")?;
        Ok(dir.join("herdr-pet").join("config.json"))
    }
}

/// The pre-M1 Windows location, computed the way macOS still does.
#[cfg(target_os = "windows")]
fn legacy_config_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))?;
    Some(dir.join("herdr-pet").join("config.json"))
}

/// Move the legacy config into the roaming profile. A cross-volume rename
/// fails, so fall back to copy-and-delete.
#[cfg(target_os = "windows")]
fn migrate_legacy_config(target: &Path, legacy: &Path) -> bool {
    if target.exists() || !legacy.is_file() {
        return false;
    }
    if let Some(parent) = target.parent()
        && let Err(error) = fs::create_dir_all(parent)
    {
        eprintln!("herdr-pet: cannot create {}: {error}", parent.display());
        return false;
    }
    let moved = fs::rename(legacy, target).or_else(|_| {
        fs::copy(legacy, target)
            .map(|_| ())
            .and_then(|()| fs::remove_file(legacy))
    });
    match moved {
        Ok(()) => {
            eprintln!(
                "herdr-pet: moved the config from {} to {}",
                legacy.display(),
                target.display()
            );
            true
        }
        Err(error) => {
            eprintln!(
                "herdr-pet: cannot move {} to {}: {error}",
                legacy.display(),
                target.display()
            );
            false
        }
    }
}

/// Missing or broken files fall back to defaults; the user's file is left
/// untouched until the next explicit save.
pub fn load() -> Config {
    match config_path() {
        Ok(path) => load_from(&path),
        Err(error) => {
            eprintln!("herdr-pet: {error:#}; using defaults");
            Config::default()
        }
    }
}

pub fn load_from(path: &Path) -> Config {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Config::default(),
        Err(error) => {
            eprintln!(
                "herdr-pet: reading {}: {error}; using defaults",
                path.display()
            );
            return Config::default();
        }
    };
    match serde_json::from_str(&text) {
        Ok(config) => config,
        Err(error) => {
            eprintln!(
                "herdr-pet: parsing {}: {error}; using defaults",
                path.display()
            );
            Config::default()
        }
    }
}

impl Config {
    pub fn save(&self) -> Result<()> {
        let path = config_path()?;
        self.save_to(&path)
    }

    /// Write to a sibling temp file and rename, so a crash cannot leave a
    /// truncated config behind.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let bytes = serde_json::to_vec_pretty(self).context("serializing config")?;
        let tmp = path.with_extension("json.tmp");
        if let Err(error) = fs::write(&tmp, bytes).and_then(|()| fs::rename(&tmp, path)) {
            let _ = fs::remove_file(&tmp);
            return Err(error).with_context(|| format!("writing {}", path.display()));
        }
        Ok(())
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
                "herdr-pet-config-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn defaults_are_sane() {
        let config = Config::default();
        assert_eq!(config.pets_dir, None);
        assert_eq!(config.pet, None);
        assert_eq!(config.size, 104.0);
        assert_eq!(config.position, None);
        assert!(config.notifications);
        assert!(config.notification_sound);
        assert!(config.gaze);
        assert!(!config.click_through);
        assert!(!config.hide_tray_icon);
        assert!(config.chat_open);
        assert_eq!(config.chat_opacity, 0.7);
    }

    #[test]
    fn partial_json_keeps_defaults() {
        let config: Config = serde_json::from_str(r#"{"size": 150.0}"#).unwrap();
        assert_eq!(config.size, 150.0);
        assert!(config.notifications);
        assert!(config.notification_sound);
        assert!(config.gaze);
        assert!(!config.click_through);
        assert!(!config.hide_tray_icon);
        assert!(config.chat_open);
        assert_eq!(config.chat_opacity, 0.7);
        assert_eq!(config.pets_dir, None);
        assert_eq!(config.pet, None);
    }

    #[test]
    fn round_trips_through_disk() {
        let temp = TempDir::new();
        let path = temp.join("config.json");
        let config = Config {
            pets_dir: Some(PathBuf::from("/tmp/pets")),
            pet: Some("frieren".to_owned()),
            size: 130.0,
            position: Some([100, 200]),
            notifications: false,
            notification_sound: false,
            gaze: false,
            click_through: true,
            hide_tray_icon: true,
            chat_open: false,
            chat_opacity: 0.4,
        };
        config.save_to(&path).unwrap();
        assert_eq!(load_from(&path), config);
        assert!(!temp.join("config.json.tmp").exists());
    }

    #[test]
    fn missing_and_corrupt_files_fall_back() {
        let temp = TempDir::new();
        let missing = temp.join("missing.json");
        assert_eq!(load_from(&missing), Config::default());

        let corrupt = temp.join("corrupt.json");
        fs::write(&corrupt, "{not json").unwrap();
        assert_eq!(load_from(&corrupt), Config::default());
        assert_eq!(
            fs::read_to_string(&corrupt).unwrap(),
            "{not json",
            "load must not overwrite the user's file"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn legacy_config_moves_to_the_roaming_profile() {
        let temp = TempDir::new();
        let legacy = temp.join("legacy.json");
        let target = temp.join("herdr-pet").join("config.json");
        fs::write(&legacy, r#"{"size": 150.0}"#).unwrap();

        assert!(migrate_legacy_config(&target, &legacy));
        assert!(!legacy.exists(), "the legacy file is moved, not copied");
        assert_eq!(load_from(&target).size, 150.0);

        // The target wins and an unrelated legacy file stays untouched.
        let other = temp.join("other.json");
        fs::write(&other, r#"{"size": 80.0}"#).unwrap();
        assert!(!migrate_legacy_config(&target, &other));
        assert_eq!(load_from(&target).size, 150.0);
        assert!(other.exists());
    }

    #[test]
    fn atomic_save_replaces_existing_file() {
        let temp = TempDir::new();
        let path = temp.join("config.json");
        fs::write(&path, "old").unwrap();
        Config::default().save_to(&path).unwrap();
        assert_eq!(load_from(&path), Config::default());
        assert!(!temp.join("config.json.tmp").exists());
    }
}
