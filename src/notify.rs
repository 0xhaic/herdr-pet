//! Desktop notifications and sounds. The notification fallback never goes
//! through a shell.

#[cfg(target_os = "macos")]
use std::process::Command;

use crate::platform;
use crate::status::{AgentStatus, StatusChange};

/// One reminder for all changes collected in a frame; blocked wins.
pub fn announce(changes: &[StatusChange], sound: bool) {
    announce_with(changes, sound, deliver, play_sound);
}

/// Show one reminder. Windows toasts need no shell fallback; macOS keeps the
/// osascript path for bare binaries without a bundle identity.
fn deliver(message: &str) {
    if platform::send_notification(message) {
        return;
    }
    #[cfg(target_os = "macos")]
    run_detached(&mut osascript_command(message));
    #[cfg(target_os = "windows")]
    eprintln!("herdr-pet: cannot show the notification");
}

fn announce_with(
    changes: &[StatusChange],
    sound: bool,
    mut deliver: impl FnMut(&str),
    mut play: impl FnMut(bool),
) {
    let Some(status) = merged_status(changes) else {
        return;
    };
    let count = changes
        .iter()
        .filter(|change| change.status == status)
        .count();
    let text = changes
        .iter()
        .find(|change| change.status == status)
        .map(|change| change.text.as_str())
        .unwrap_or("agent");
    deliver(&message_for(status, count, text));
    if sound {
        play(status == AgentStatus::Blocked);
    }
}

#[cfg(target_os = "macos")]
fn play_sound(blocked: bool) {
    let mut command = Command::new("afplay");
    command.arg(format!(
        "/System/Library/Sounds/{}.aiff",
        sound_name(blocked)
    ));
    run_detached(&mut command);
}

#[cfg(target_os = "windows")]
fn play_sound(blocked: bool) {
    platform::play_notification_sound(blocked);
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn play_sound(blocked: bool) {
    let _ = blocked;
}

#[cfg(target_os = "macos")]
fn sound_name(blocked: bool) -> &'static str {
    if blocked { "Basso" } else { "Glass" }
}

#[cfg(target_os = "macos")]
fn osascript_command(message: &str) -> Command {
    let script = format!(
        "display notification {} with title \"herdr-pet\"",
        applescript_string(message)
    );
    let mut command = Command::new("osascript");
    command.arg("-e").arg(script);
    command
}

fn merged_status(changes: &[StatusChange]) -> Option<AgentStatus> {
    if changes
        .iter()
        .any(|change| change.status == AgentStatus::Blocked)
    {
        Some(AgentStatus::Blocked)
    } else if changes
        .iter()
        .any(|change| change.status == AgentStatus::Done)
    {
        Some(AgentStatus::Done)
    } else {
        None
    }
}

fn message_for(status: AgentStatus, count: usize, text: &str) -> String {
    match status {
        AgentStatus::Blocked if count == 1 => format!("{text} 等待输入"),
        AgentStatus::Blocked => format!("{count} 个 agent 等待输入"),
        AgentStatus::Done if count == 1 => format!("{text} 已完成"),
        AgentStatus::Done => format!("{count} 个 agent 已完成"),
        _ => String::new(),
    }
}

/// Quote a string for AppleScript: backslash, quote, and line breaks.
#[cfg(target_os = "macos")]
fn applescript_string(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(target_os = "macos")]
fn run_detached(command: &mut Command) {
    match command.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(error) => eprintln!("herdr-pet: cannot run {:?}: {error}", command.get_program()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(status: AgentStatus, text: &str) -> StatusChange {
        StatusChange {
            address: "t1".to_owned(),
            status,
            text: text.to_owned(),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn escapes_applescript_strings() {
        assert_eq!(applescript_string("plain"), "\"plain\"");
        assert_eq!(applescript_string(r#"a"b"#), r#""a\"b""#);
        assert_eq!(applescript_string(r"a\b"), r#""a\\b""#);
        assert_eq!(applescript_string("a\nb\r"), r#""a\nb\r""#);
    }

    #[test]
    fn merges_same_frame_changes() {
        let blocked = [
            change(AgentStatus::Blocked, "one"),
            change(AgentStatus::Blocked, "two"),
        ];
        assert_eq!(merged_status(&blocked), Some(AgentStatus::Blocked));
        assert_eq!(
            message_for(AgentStatus::Blocked, 2, "one"),
            "2 个 agent 等待输入"
        );
        assert_eq!(message_for(AgentStatus::Blocked, 1, "one"), "one 等待输入");

        let mixed = [
            change(AgentStatus::Done, "done"),
            change(AgentStatus::Blocked, "blocked"),
        ];
        assert_eq!(merged_status(&mixed), Some(AgentStatus::Blocked));

        assert_eq!(merged_status(&[]), None);
    }

    #[test]
    fn done_messages() {
        assert_eq!(message_for(AgentStatus::Done, 1, "task"), "task 已完成");
        assert_eq!(
            message_for(AgentStatus::Done, 3, "task"),
            "3 个 agent 已完成"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_sound_names() {
        assert_eq!(sound_name(true), "Basso");
        assert_eq!(sound_name(false), "Glass");
    }

    #[test]
    fn announcement_is_delivered_once_and_sound_follows_config() {
        let changes = [change(AgentStatus::Blocked, "task")];

        let mut delivered = Vec::new();
        let mut played = Vec::new();
        announce_with(
            &changes,
            false,
            |message| delivered.push(message.to_owned()),
            |blocked| played.push(blocked),
        );
        assert_eq!(delivered, ["task 等待输入"]);
        assert!(played.is_empty());

        announce_with(&changes, true, |_| {}, |blocked| played.push(blocked));
        assert_eq!(played, [true]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn osascript_fallback_quotes_external_text() {
        let command = osascript_command("a\"b\nc");
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "-e");
        assert_eq!(
            args[1],
            "display notification \"a\\\"b\\nc\" with title \"herdr-pet\""
        );
    }
}
