//! Chat bubble geometry and text, kept free of windowing so it stays testable.
//!
//! The window is wider than the pet: the bubble spans a Codex-like 320 pt at
//! the top, the pet sits centred in the middle, and the toggle button sits
//! below it. Collapsing only hides the bubble views; the window keeps its size
//! so the pet never jumps.

use crate::status::{AgentStatus, StatusTable};

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
        }
    }

    /// Pet top-left inside the window, in logical points.
    pub fn pet_origin(&self) -> (f64, f64) {
        (self.pet.x, self.pet.y)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatText {
    pub title: String,
    pub subtitle: String,
}

/// Two lines for the bubble: the focused agent's task title and its latest
/// action (`status.msg`), falling back to a localized state word.
pub fn chat_text(table: &StatusTable) -> ChatText {
    match table.focus() {
        Some(agent) => {
            let title = one_line(&agent.notification_text());
            let subtitle = agent
                .status_msg()
                .map(one_line)
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| {
                    status_label(agent.status.unwrap_or(AgentStatus::Idle)).to_owned()
                });
            ChatText { title, subtitle }
        }
        None => ChatText {
            title: "暂无任务".to_owned(),
            subtitle: "等待 agent 状态".to_owned(),
        },
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
            chat_text(&table),
            ChatText {
                title: "t2 title".to_owned(),
                subtitle: "Permission needed".to_owned()
            }
        );
    }

    #[test]
    fn text_falls_back_to_localized_state_and_placeholder() {
        let mut table = StatusTable::new();
        table.apply_snapshot(vec![terminal("t1", "live", Some("done"), "")]);
        assert_eq!(chat_text(&table).subtitle, "已完成");

        let empty = StatusTable::new();
        assert_eq!(
            chat_text(&empty),
            ChatText {
                title: "暂无任务".to_owned(),
                subtitle: "等待 agent 状态".to_owned()
            }
        );
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
