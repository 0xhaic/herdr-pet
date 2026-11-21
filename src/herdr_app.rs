//! Best-effort bridge for raising Herdr.app at a session.
//!
//! Visibility does not need this module: the pet only asks NSWorkspace whether
//! Herdr is frontmost. Clicking the bubble or pet uses the documented
//! `herdr-app` CLI to show the terminal, so this never mixes with herdr-core's
//! event subscription.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use serde::Deserialize;

use crate::platform;

const BUNDLE_ID: &str = "dev.herdr.app";
const FOCUS_ATTEMPTS: u32 = 25;
const FOCUS_RETRY_DELAY: Duration = Duration::from_millis(200);
const ATTENTION_INTERVAL: Duration = Duration::from_millis(500);

static FOCUSING: AtomicBool = AtomicBool::new(false);

/// Terminals Herdr currently marks as seen. A `done` agent outside this set is
/// still waiting for the person to look at it, so the chat stays visible.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attention {
    pub seen: BTreeSet<String>,
}

#[derive(Debug, Deserialize, Default)]
struct ViewDoc {
    #[serde(default)]
    attention: BTreeMap<String, AttentionEntry>,
}

#[derive(Debug, Deserialize, Default)]
struct AttentionEntry {
    #[serde(default)]
    seen: bool,
}

/// Follow Herdr's attention map (view.json) in the background. Reads a small
/// local file; there is no documented CLI for per-terminal seen state.
pub fn spawn_attention(sender: Sender<Attention>) {
    let thread = std::thread::Builder::new()
        .name("herdr-app-attention".to_owned())
        .spawn(move || {
            let path = app_home().map(|home| home.join("view.json"));
            let mut last = Attention::default();
            loop {
                let next = path
                    .as_deref()
                    .and_then(read_attention)
                    .unwrap_or_else(|| last.clone());
                if next != last {
                    last = next;
                    if sender.send(last.clone()).is_err() {
                        return;
                    }
                }
                std::thread::sleep(ATTENTION_INTERVAL);
            }
        });
    if let Err(error) = thread {
        eprintln!("herdr-pet: cannot start the Herdr.app attention thread: {error}");
    }
}

fn read_attention(path: &Path) -> Option<Attention> {
    let text = fs::read_to_string(path).ok()?;
    let doc: ViewDoc = serde_json::from_str(&text).ok()?;
    Some(attention_from(doc))
}

fn attention_from(doc: ViewDoc) -> Attention {
    Attention {
        seen: doc
            .attention
            .into_iter()
            .filter(|(_, entry)| entry.seen)
            .map(|(address, _)| address)
            .collect(),
    }
}

fn app_home() -> Option<PathBuf> {
    if let Some(home) = env::var_os("HERDR_APP_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(dir) = env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(dir).join("herdr-app"));
    }
    dirs::home_dir().map(|home| home.join(".config").join("herdr-app"))
}

/// Raise Herdr.app and show `address`, retrying while a freshly launched app
/// finishes opening its window. Runs off the UI thread and never fails loudly.
pub fn focus_terminal(address: String) {
    if FOCUSING.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("herdr-app-jump".to_owned())
        .spawn(move || {
            raise_app();
            if let Some(cli) = app_cli() {
                let mut focused = false;
                for _ in 0..FOCUS_ATTEMPTS {
                    focused = Command::new(&cli)
                        .args(["do", "agent.focus", "--terminal", &address])
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()
                        .is_ok_and(|status| status.success());
                    if focused {
                        break;
                    }
                    std::thread::sleep(FOCUS_RETRY_DELAY);
                }
                if !focused {
                    eprintln!("herdr-pet: cannot show {address} in Herdr.app");
                }
            }
            FOCUSING.store(false, Ordering::SeqCst);
        });
    if spawned.is_err() {
        eprintln!("herdr-pet: cannot start the Herdr.app jump thread");
        FOCUSING.store(false, Ordering::SeqCst);
    }
}

/// Raise Herdr.app without targeting a session (used when no agent is known).
pub fn raise_app_in_background() {
    let spawned = std::thread::Builder::new()
        .name("herdr-app-raise".to_owned())
        .spawn(raise_app);
    if spawned.is_err() {
        eprintln!("herdr-pet: cannot start the Herdr.app raise thread");
    }
}

fn raise_app() {
    let _ = Command::new("/usr/bin/open")
        .args(["-b", BUNDLE_ID])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn app_cli() -> Option<PathBuf> {
    platform::herdr_app_path()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_seen_addresses_from_attention() {
        let doc: ViewDoc = serde_json::from_str(
            r#"{
                "active": "s1",
                "attention": {
                    "t1": {"word": "done", "seen": false},
                    "t2": {"word": "working", "seen": true},
                    "machine/t3": {"word": "done", "seen": true}
                }
            }"#,
        )
        .unwrap();
        let attention = attention_from(doc);
        assert_eq!(
            attention.seen,
            BTreeSet::from(["t2".to_owned(), "machine/t3".to_owned()])
        );
        assert!(!attention.seen.contains("t1"));
    }
}
