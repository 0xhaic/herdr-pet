//! Desktop notifications and sounds. Never goes through a shell for the
//! osascript fallback.

use std::process::Command;

use crate::platform;
use crate::status::{AgentStatus, StatusChange};

/// One reminder for all changes collected in a frame; blocked wins.
pub fn announce(changes: &[StatusChange], sound: bool) {
    announce_with(
        changes,
        sound,
        |message| {
            // Bundled runs use the app's own notification identity, so the
            // banner carries the app icon; bare binaries have no bundle
            // identifier and fall back to osascript.
            if !platform::send_notification(message) {
                run_detached(&mut osascript_command(message));
            }
        },
        run_detached,
    );
}

fn announce_with(
    changes: &[StatusChange],
    sound: bool,
    mut deliver: impl FnMut(&str),
    mut run: impl FnMut(&mut Command),
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
        run(Command::new("afplay")
            .arg(format!("/System/Library/Sounds/{}.aiff", sound_for(status))));
    }
}

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

fn sound_for(status: AgentStatus) -> &'static str {
    if status == AgentStatus::Blocked {
        "Basso"
    } else {
        "Glass"
    }
}

/// Quote a string for AppleScript: backslash, quote, and line breaks.
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
    fn done_messages_and_sounds() {
        assert_eq!(message_for(AgentStatus::Done, 1, "task"), "task 已完成");
        assert_eq!(
            message_for(AgentStatus::Done, 3, "task"),
            "3 个 agent 已完成"
        );
        assert_eq!(sound_for(AgentStatus::Blocked), "Basso");
        assert_eq!(sound_for(AgentStatus::Done), "Glass");
    }

    #[test]
    fn announcement_is_delivered_once_and_sound_follows_config() {
        let changes = [change(AgentStatus::Blocked, "task")];

        let mut delivered = Vec::new();
        let mut commands = Vec::new();
        announce_with(
            &changes,
            false,
            |message| delivered.push(message.to_owned()),
            |command| commands.push(command.get_program().to_owned()),
        );
        assert_eq!(delivered, ["task 等待输入"]);
        assert!(commands.is_empty());

        announce_with(
            &changes,
            true,
            |_| {},
            |command| commands.push(command.get_program().to_owned()),
        );
        assert_eq!(commands, ["afplay"]);
    }

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
