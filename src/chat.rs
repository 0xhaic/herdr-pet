//! Chat bubble geometry and text, kept free of windowing so it stays testable.
//!
//! The window is wider than the pet: the bubble spans a Codex-like 320 pt at
//! the top, the pet sits centred in the middle, and the toggle button sits
//! below it. Collapsing only hides the bubble views; the window keeps its size
//! so the pet never jumps.

use crate::status::{AgentStatus, TerminalAgent};

/// Codex's activity pill is 320 pt wide with a 32 pt corner radius.
pub const BUBBLE_WIDTH: f64 = 320.0;
/// Fixed height for two lines (13 pt title + 13 pt subtitle).
pub const BUBBLE_HEIGHT: f64 = 64.0;
/// Gap between the bubble and the pet's sprite cell.
pub const BUBBLE_GAP: f64 = 16.0;
/// Diameter of the collapse/expand button at the pet's lower-right corner.
pub const BUTTON_SIZE: f64 = 28.0;
/// Gap between the pet's sprite cell and the button.
pub const BUTTON_GAP: f64 = 6.0;
/// Transparent margin around the UI so rounded edges are not clipped.
pub const WINDOW_PAD: f64 = 8.0;
/// Previous/next agent arrows inside the bubble's trailing end, shown while
/// several agents need the bubble.
pub const ARROW_SIZE: f64 = 22.0;
/// Gap between the two bubble arrows.
pub const ARROW_GAP: f64 = 4.0;
/// Inset from the bubble's right edge to the next arrow.
pub const ARROW_INSET: f64 = 8.0;
/// Width the bubble text keeps clear for both arrows.
pub const ARROW_RESERVED: f64 = ARROW_INSET + ARROW_SIZE * 2.0 + ARROW_GAP;

/// Longest text handed to a single label line; the label truncates the rest.
const MAX_TEXT_CHARS: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChatLayout {
    /// Window size in logical points.
    pub window: (f64, f64),
    /// Bubble rectangle, top-left origin.
    pub bubble: Rect,
    /// Pet sprite cell rectangle, top-left origin.
    pub pet: Rect,
    /// Toggle button rectangle, top-left origin.
    pub button: Rect,
    /// Previous/next agent arrows inside the bubble, top-left origin.
    pub prev_arrow: Rect,
    pub next_arrow: Rect,
}

impl ChatLayout {
    pub fn new(pet_height: f64) -> Self {
        let pet_height = if pet_height.is_finite() && pet_height > 0.0 {
            pet_height
        } else {
            crate::config::DEFAULT_SIZE
        };
        let pet_width = pet_height * crate::pet::CELL_W as f64 / crate::pet::CELL_H as f64;
        let width = BUBBLE_WIDTH + WINDOW_PAD * 2.0;
        let height = WINDOW_PAD
            + BUBBLE_HEIGHT
            + BUBBLE_GAP
            + pet_height
            + BUTTON_GAP
            + BUTTON_SIZE
            + WINDOW_PAD;
        let bubble_y = WINDOW_PAD;
        let pet_y = bubble_y + BUBBLE_HEIGHT + BUBBLE_GAP;
        let button_y = pet_y + pet_height + BUTTON_GAP;
        let next_arrow_x = (width + BUBBLE_WIDTH) / 2.0 - ARROW_INSET - ARROW_SIZE;
        let arrow_y = bubble_y + (BUBBLE_HEIGHT - ARROW_SIZE) / 2.0;
        Self {
            window: (width, height),
            bubble: Rect {
                x: (width - BUBBLE_WIDTH) / 2.0,
                y: bubble_y,
                w: BUBBLE_WIDTH,
                h: BUBBLE_HEIGHT,
            },
            pet: Rect {
                x: (width - pet_width) / 2.0,
                y: pet_y,
                w: pet_width,
                h: pet_height,
            },
            button: Rect {
                // Sit under the pet's lower-right corner, not centred.
                x: (width - pet_width) / 2.0 + pet_width - BUTTON_SIZE,
                y: button_y,
                w: BUTTON_SIZE,
                h: BUTTON_SIZE,
            },
            prev_arrow: Rect {
                x: next_arrow_x - ARROW_GAP - ARROW_SIZE,
                y: arrow_y,
                w: ARROW_SIZE,
                h: ARROW_SIZE,
            },
            next_arrow: Rect {
                x: next_arrow_x,
                y: arrow_y,
                w: ARROW_SIZE,
                h: ARROW_SIZE,
            },
        }
    }

    /// Pet top-left inside the window, in logical points.
    pub fn pet_origin(&self) -> (f64, f64) {
        (self.pet.x, self.pet.y)
    }
}

/// An agent icon from Herdr.app's program table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentIcon {
    /// Icon file inside Herdr.app's home, e.g. `icons/claude.svg`.
    pub file: &'static str,
    /// Herdr draws tinted icons as one-color glyphs in the text color.
    pub tint: bool,
}

const GENERIC_AGENT_ICON: AgentIcon = AgentIcon {
    file: "icons/agent.svg",
    tint: true,
};

/// Herdr.app's aliases for agent programs, limited to each entry's icon and
/// tint flag: (reported app / program name, icon, tinted).
const AGENT_ICONS: &[(&str, &str, bool)] = &[
    ("claude-code", "icons/claude.svg", false),
    ("claude", "icons/claude.svg", false),
    ("codex", "icons/codex.svg", false),
    ("pi", "icons/pi.svg", true),
    ("pig", "icons/pig.svg", false),
    ("opencode", "icons/opencode.svg", true),
    ("cursor-agent", "icons/cursor.svg", true),
    ("cursor", "icons/cursor.svg", true),
    ("github-copilot", "icons/copilot.svg", true),
    ("copilot", "icons/copilot.svg", true),
    ("amp", "icons/amp.svg", false),
    ("grok-build", "icons/grok.svg", true),
    ("grok", "icons/grok.svg", true),
    ("dsh-tui", "icons/dsh-tui.svg", true),
    ("goose", "icons/goose.svg", true),
    ("agy", "icons/antigravity.svg", false),
    ("antigravity", "icons/antigravity.svg", false),
    ("kimi-code", "icons/kimi.svg", false),
    ("kimicodecli", "icons/kimi.svg", false),
    ("kimi", "icons/kimi.svg", false),
    ("kiro-cli", "icons/kiro.svg", false),
    ("kiro", "icons/kiro.svg", false),
    ("hermes", "icons/hermes.svg", true),
    ("mastracode", "icons/mastracode.svg", true),
    ("qodercli", "icons/qoder.svg", true),
    ("qoder", "icons/qoder.svg", true),
    ("muse", "icons/muse.svg", true),
    ("crush", "icons/crush.svg", false),
    ("command-code", "icons/command-code.svg", true),
    ("prime-agent", "icons/prime-agent.svg", true),
    ("gemini", "icons/gemini.svg", true),
    ("ollama", "icons/ollama.svg", true),
    ("oh-my-pi", "icons/omp.svg", true),
    ("omp", "icons/omp.svg", true),
];

fn lookup_agent_icon(key: &str) -> Option<AgentIcon> {
    let key = key.trim();
    AGENT_ICONS
        .iter()
        .find(|(alias, _, _)| key.eq_ignore_ascii_case(alias))
        .map(|&(_, file, tint)| AgentIcon { file, tint })
}

/// The agent's icon from Herdr.app's program table: its own reported identity
/// first, then the program name, else Herdr's generic agent glyph.
pub fn agent_icon(app: Option<&str>, name: Option<&str>) -> AgentIcon {
    app.and_then(lookup_agent_icon)
        .or_else(|| name.and_then(lookup_agent_icon))
        .unwrap_or(GENERIC_AGENT_ICON)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatText {
    pub title: String,
    pub subtitle: String,
    /// Agent icon before the title; None for the no-agent placeholder.
    pub icon: Option<AgentIcon>,
    /// State glyph before the subtitle; None for the no-agent placeholder.
    pub status: Option<AgentStatus>,
}

/// Two lines for the bubble: the focused agent's task title and its latest
/// action (`status.msg`), falling back to a localized state word.
pub fn chat_text(agent: Option<&TerminalAgent>) -> ChatText {
    match agent {
        Some(agent) => {
            let title = one_line(&agent.notification_text());
            let subtitle = agent
                .status_msg()
                .map(one_line)
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| {
                    status_label(agent.status.unwrap_or(AgentStatus::Idle)).to_owned()
                });
            ChatText {
                title,
                subtitle,
                icon: Some(agent_icon(agent.app.as_deref(), agent.name.as_deref())),
                status: Some(agent.status.unwrap_or(AgentStatus::Idle)),
            }
        }
        None => ChatText {
            title: "暂无任务".to_owned(),
            subtitle: "等待 agent 状态".to_owned(),
            icon: None,
            status: None,
        },
    }
}

/// SF Symbol shown before the subtitle for each state. `Working` renders as
/// the native spinner at runtime; the no-agent placeholder gets a neutral
/// dots glyph.
pub fn status_symbol(status: Option<AgentStatus>) -> &'static str {
    match status {
        None => "ellipsis.circle",
        Some(AgentStatus::Idle) => "moon.zzz",
        Some(AgentStatus::Working) => "progress.indicator",
        Some(AgentStatus::Blocked) => "hand.raised",
        Some(AgentStatus::Done) => "checkmark.circle",
        Some(AgentStatus::Error) => "xmark.circle",
    }
}

pub fn status_label(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "空闲",
        AgentStatus::Working => "工作中",
        AgentStatus::Blocked => "等待输入",
        AgentStatus::Done => "已完成",
        AgentStatus::Error => "出错了",
    }
}

/// Collapse whitespace/newlines into single spaces and cap the length so a
/// rogue agent title cannot grow the AppKit label without bound.
fn one_line(text: &str) -> String {
    let mut out = String::new();
    let mut chars = 0;
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
            chars += 1;
        }
        for ch in word.chars() {
            if chars >= MAX_TEXT_CHARS {
                out.push('…');
                return out;
            }
            out.push(ch);
            chars += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{StatusRecord, StatusTable, Terminal};

    fn terminal(address: &str, state: &str, status: Option<&str>, msg: &str) -> Terminal {
        Terminal {
            address: address.to_owned(),
            title: format!("{address} title"),
            state: state.to_owned(),
            status: status.map(|state| StatusRecord {
                state: state.to_owned(),
                msg: msg.to_owned(),
                ..StatusRecord::default()
            }),
            ..Terminal::default()
        }
    }

    #[test]
    fn layout_centres_bubble_pet_and_button() {
        let layout = ChatLayout::new(104.0);
        assert_eq!(layout.window, (336.0, 234.0));
        assert_eq!(
            layout.bubble,
            Rect {
                x: 8.0,
                y: 8.0,
                w: 320.0,
                h: 64.0
            }
        );
        assert_eq!(layout.pet.x, 120.0);
        assert_eq!(layout.pet.y, 88.0);
        assert_eq!(layout.pet.w, 96.0);
        assert_eq!(layout.pet.h, 104.0);
        assert_eq!(
            layout.button,
            Rect {
                x: 188.0,
                y: 198.0,
                w: 28.0,
                h: 28.0
            }
        );
    }

    #[test]
    fn layout_places_pager_arrows_inside_the_bubble() {
        let layout = ChatLayout::new(104.0);
        assert_eq!(
            layout.next_arrow,
            Rect {
                x: 298.0,
                y: 29.0,
                w: 22.0,
                h: 22.0
            }
        );
        assert_eq!(
            layout.prev_arrow,
            Rect {
                x: 272.0,
                y: 29.0,
                w: 22.0,
                h: 22.0
            }
        );
        for arrow in [layout.prev_arrow, layout.next_arrow] {
            assert!(arrow.x >= layout.bubble.x);
            assert!(arrow.x + arrow.w <= layout.bubble.x + layout.bubble.w);
            assert!(arrow.y >= layout.bubble.y);
            assert!(arrow.y + arrow.h <= layout.bubble.y + layout.bubble.h);
        }
        assert_eq!(
            layout.bubble.x + BUBBLE_WIDTH - ARROW_RESERVED,
            layout.prev_arrow.x,
            "text must end before the pager"
        );
    }

    #[test]
    fn layout_grows_vertically_with_pet_size() {
        let small = ChatLayout::new(52.0);
        let large = ChatLayout::new(312.0);
        assert_eq!(small.window.0, large.window.0);
        assert_eq!(large.window.1 - small.window.1, 260.0);
        assert_eq!(large.bubble.y, small.bubble.y);
        assert_eq!(large.pet.y, small.pet.y);
        assert!(large.pet.x < small.pet.x);
        assert_eq!(large.button.x, large.pet.x + large.pet.w - BUTTON_SIZE);
        assert_eq!(small.button.x, small.pet.x + small.pet.w - BUTTON_SIZE);
    }

    #[test]
    fn rect_contains_is_half_open() {
        let rect = Rect {
            x: 10.0,
            y: 20.0,
            w: 30.0,
            h: 40.0,
        };
        assert!(rect.contains(10.0, 20.0));
        assert!(rect.contains(39.9, 59.9));
        assert!(!rect.contains(40.0, 30.0));
        assert!(!rect.contains(10.0, 60.0));
        assert_eq!(rect.center(), (25.0, 40.0));
    }

    #[test]
    fn text_uses_focus_title_and_status_message() {
        let mut table = StatusTable::new();
        table.apply_snapshot(vec![
            terminal("t1", "live", Some("working"), "Running command"),
            terminal("t2", "live", Some("blocked"), "Permission needed"),
        ]);
        assert_eq!(
            chat_text(table.focus_with(|_| false)),
            ChatText {
                title: "t2 title".to_owned(),
                subtitle: "Permission needed".to_owned(),
                icon: Some(GENERIC_AGENT_ICON),
                status: Some(AgentStatus::Blocked),
            }
        );
    }

    #[test]
    fn text_falls_back_to_localized_state_and_placeholder() {
        let mut table = StatusTable::new();
        table.apply_snapshot(vec![terminal("t1", "live", Some("done"), "")]);
        assert_eq!(chat_text(table.focus_with(|_| true)).subtitle, "已完成");

        assert_eq!(
            chat_text(None),
            ChatText {
                title: "暂无任务".to_owned(),
                subtitle: "等待 agent 状态".to_owned(),
                icon: None,
                status: None
            }
        );
    }

    #[test]
    fn status_symbols_cover_every_state_and_placeholder() {
        assert_eq!(status_symbol(None), "ellipsis.circle");
        assert_eq!(status_symbol(Some(AgentStatus::Idle)), "moon.zzz");
        assert_eq!(
            status_symbol(Some(AgentStatus::Working)),
            "progress.indicator"
        );
        assert_eq!(status_symbol(Some(AgentStatus::Blocked)), "hand.raised");
        assert_eq!(status_symbol(Some(AgentStatus::Done)), "checkmark.circle");
        assert_eq!(status_symbol(Some(AgentStatus::Error)), "xmark.circle");
    }

    #[test]
    fn agent_icons_follow_app_then_program_name() {
        assert_eq!(
            agent_icon(Some("claude-code"), None),
            AgentIcon {
                file: "icons/claude.svg",
                tint: false
            }
        );
        assert_eq!(
            agent_icon(Some("pi"), None),
            AgentIcon {
                file: "icons/pi.svg",
                tint: true
            }
        );
        assert_eq!(
            agent_icon(Some("unknown"), Some("codex")),
            AgentIcon {
                file: "icons/codex.svg",
                tint: false
            }
        );
        assert_eq!(
            agent_icon(None, Some("cursor-agent")),
            AgentIcon {
                file: "icons/cursor.svg",
                tint: true
            }
        );
        assert_eq!(agent_icon(Some("unknown"), None), GENERIC_AGENT_ICON);
    }

    #[test]
    fn text_is_single_line_and_truncated() {
        let long = format!("first\nsecond\t{}", "字".repeat(200));
        let line = one_line(&long);
        assert!(!line.contains('\n'));
        assert!(line.starts_with("first second"));
        assert!(line.ends_with('…'));
        assert_eq!(line.chars().count(), MAX_TEXT_CHARS + 1);
    }
}
