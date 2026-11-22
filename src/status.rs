//! Terminal status table, aggregation, and blocked/done change detection.
//!
//! Input is the herdr-core `Terminal` shape (a snapshot or a single event);
//! output is an `Aggregate` for the pet plus `StatusChange` reminders. No I/O
//! happens here, so everything is unit-testable without herdr.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

/// Display priority: `Idle < Done < Working < Error < Blocked`.
///
/// Working outranks Done so live but stale `done` terminals do not mask tasks
/// that are still executing; the pet stands only while work is in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AgentStatus {
    Idle,
    Done,
    Working,
    Error,
    Blocked,
}

impl AgentStatus {
    pub fn parse(state: &str) -> Option<Self> {
        match state {
            "idle" => Some(Self::Idle),
            "working" => Some(Self::Working),
            "done" => Some(Self::Done),
            "error" => Some(Self::Error),
            "blocked" => Some(Self::Blocked),
            _ => None,
        }
    }

    pub fn is_alert(self) -> bool {
        matches!(self, Self::Blocked | Self::Done)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct StatusRecord {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub title: Option<String>,
    /// The program's own words for what it is doing, shown in the chat bubble.
    #[serde(default)]
    pub msg: String,
}

/// The subset of herdr-core's `Terminal` that matters for aggregation.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Terminal {
    pub address: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub status: Option<StatusRecord>,
    #[serde(default)]
    pub status_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalAgent {
    pub address: String,
    pub title: Option<String>,
    pub name: Option<String>,
    pub labels: BTreeMap<String, String>,
    pub live: bool,
    pub status: Option<AgentStatus>,
    pub status_msg: Option<String>,
    pub status_ms: Option<u64>,
    /// Monotonic table order of the latest status write; breaks focus ties.
    updated_at: u64,
}

impl TerminalAgent {
    fn from_terminal(terminal: Terminal, updated_at: u64) -> Self {
        let Terminal {
            address,
            title,
            name,
            labels,
            state,
            status,
            status_ms,
        } = terminal;
        let live = state == "live";
        let agent_status = if live {
            status
                .as_ref()
                .and_then(|record| AgentStatus::parse(&record.state))
        } else {
            None
        };
        let title = if title.is_empty() {
            status_title(&status)
        } else {
            Some(title)
        };
        let status_msg = status
            .as_ref()
            .map(|record| record.msg.trim())
            .filter(|msg| !msg.is_empty())
            .map(str::to_owned);
        Self {
            address,
            title,
            name,
            labels,
            live,
            status: agent_status,
            status_msg,
            status_ms,
            updated_at,
        }
    }

    /// The focused agent's latest action line, when the program reported one.
    pub fn status_msg(&self) -> Option<&str> {
        self.status_msg.as_deref()
    }

    /// Text priority: title, then name, then labels["where"], then address.
    pub fn notification_text(&self) -> String {
        self.title
            .as_deref()
            .filter(|text| !text.is_empty())
            .or_else(|| self.name.as_deref().filter(|text| !text.is_empty()))
            .or_else(|| {
                self.labels
                    .get("where")
                    .map(String::as_str)
                    .filter(|text| !text.is_empty())
            })
            .unwrap_or(&self.address)
            .to_owned()
    }
}

/// Wall clock of the last observed status change; snapshot values are
/// herdr-core's `status_ms`, live events use their receipt time.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

fn order_key(agent: &TerminalAgent) -> (u64, u64) {
    (agent.status_ms.unwrap_or(0), agent.updated_at)
}

fn status_title(status: &Option<StatusRecord>) -> Option<String> {
    status
        .as_ref()
        .and_then(|record| record.title.clone())
        .filter(|title| !title.is_empty())
}

#[derive(Debug, Clone)]
pub enum Event {
    Created(Terminal),
    Status {
        terminal: String,
        status: StatusRecord,
    },
    /// The program cleared its status (for example, the root shell returned to
    /// the foreground); the terminal is idle again.
    StatusCleared {
        terminal: String,
    },
    Finished {
        terminal: String,
    },
    Lost {
        terminal: String,
    },
    Removed {
        terminal: String,
    },
    Title {
        terminal: String,
        title: String,
    },
    Labels {
        terminal: String,
        labels: BTreeMap<String, String>,
    },
    /// The subscriber fell behind; the client should reconnect for a snapshot.
    Missed,
    Ignored,
}

/// Parse the `params` object of an `event` notification.
pub fn parse_event(params: &Value) -> Event {
    let kind = params
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let terminal = params
        .get("terminal")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let data = params.get("data").cloned().unwrap_or(Value::Null);

    match kind {
        "created" => serde_json::from_value(data)
            .map(Event::Created)
            .unwrap_or(Event::Ignored),
        "status" if !terminal.is_empty() => {
            if status_cleared(&data) {
                Event::StatusCleared { terminal }
            } else {
                match parse_status(&data) {
                    Some(status) => Event::Status { terminal, status },
                    None => Event::Ignored,
                }
            }
        }
        "finished" if !terminal.is_empty() => Event::Finished { terminal },
        "lost" if !terminal.is_empty() => Event::Lost { terminal },
        "removed" if !terminal.is_empty() => Event::Removed { terminal },
        "title" if !terminal.is_empty() => {
            let title = data
                .get("title")
                .and_then(Value::as_str)
                .or_else(|| data.as_str());
            match title {
                Some(title) => Event::Title {
                    terminal,
                    title: title.to_owned(),
                },
                None => Event::Ignored,
            }
        }
        "labels" if !terminal.is_empty() => match parse_labels(&data) {
            Some(labels) => Event::Labels { terminal, labels },
            None => Event::Ignored,
        },
        "missed" => Event::Missed,
        _ => Event::Ignored,
    }
}

/// A `status` event's `data` wraps the root 7501 record as
/// `{"records": {...}, "reported": bool, "status": {...}}`. An explicit
/// `"status": null` means the program cleared its status (herdr-core sends it
/// when the root shell returns to the foreground), so the terminal is idle.
/// Older payloads carried the record directly; accept both. A missing
/// `status` key or a malformed record is not a clear and keeps the last status.
fn status_cleared(data: &Value) -> bool {
    matches!(data.get("status"), Some(Value::Null))
}

/// Parse a non-empty root 7501 record; a record without a non-empty `state`
/// is malformed, not an empty-status update.
fn parse_status(data: &Value) -> Option<StatusRecord> {
    let record = data.get("status").unwrap_or(data);
    let status: StatusRecord = serde_json::from_value(record.clone()).ok()?;
    if status.state.is_empty() {
        return None;
    }
    Some(status)
}

fn parse_labels(data: &Value) -> Option<BTreeMap<String, String>> {
    let object = data.get("labels").unwrap_or(data).as_object()?;
    Some(
        object
            .iter()
            .filter_map(|(key, value)| value.as_str().map(|text| (key.clone(), text.to_owned())))
            .collect(),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusChange {
    pub address: String,
    pub status: AgentStatus,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Aggregate {
    pub status: Option<AgentStatus>,
    pub blocked: usize,
    pub error: usize,
    pub done: usize,
    pub working: usize,
    /// Live terminals that reported a status; plain shells are excluded.
    pub total: usize,
}

#[derive(Debug, Default)]
pub struct StatusTable {
    terminals: BTreeMap<String, TerminalAgent>,
    sequence: u64,
}

impl StatusTable {
    pub fn new() -> Self {
        Self::default()
    }

    fn next_sequence(&mut self) -> u64 {
        self.sequence += 1;
        self.sequence
    }

    /// Replace the table from a fresh subscription snapshot. Never notifies.
    pub fn apply_snapshot(&mut self, terminals: Vec<Terminal>) {
        self.terminals.clear();
        for terminal in terminals {
            let updated_at = self.next_sequence();
            let agent = TerminalAgent::from_terminal(terminal, updated_at);
            self.terminals.insert(agent.address.clone(), agent);
        }
    }

    /// Apply one event; returns a reminder only on a transition into
    /// `Blocked` or `Done` for a live terminal.
    pub fn apply_event(&mut self, event: Event) -> Option<StatusChange> {
        match event {
            Event::Created(terminal) => {
                let updated_at = self.next_sequence();
                let agent = TerminalAgent::from_terminal(terminal, updated_at);
                self.terminals.insert(agent.address.clone(), agent);
                None
            }
            Event::Status { terminal, status } => self.update_status(&terminal, status),
            Event::StatusCleared { terminal } => {
                if let Some(agent) = self.terminals.get_mut(&terminal) {
                    agent.status = None;
                    agent.status_msg = None;
                    agent.status_ms = None;
                }
                None
            }
            Event::Finished { terminal } | Event::Lost { terminal } => {
                if let Some(agent) = self.terminals.get_mut(&terminal) {
                    agent.live = false;
                    agent.status = None;
                    agent.status_msg = None;
                    agent.status_ms = None;
                }
                None
            }
            Event::Removed { terminal } => {
                self.terminals.remove(&terminal);
                None
            }
            Event::Title { terminal, title } => {
                if let Some(agent) = self.terminals.get_mut(&terminal) {
                    agent.title = Some(title);
                }
                None
            }
            Event::Labels { terminal, labels } => {
                if let Some(agent) = self.terminals.get_mut(&terminal) {
                    agent.labels = labels;
                }
                None
            }
            Event::Missed | Event::Ignored => None,
        }
    }

    fn update_status(&mut self, address: &str, record: StatusRecord) -> Option<StatusChange> {
        let new = AgentStatus::parse(&record.state)?;
        let updated_at = self.next_sequence();
        let agent = self.terminals.get_mut(address)?;
        if !agent.live {
            return None;
        }
        let old = agent.status;
        agent.status = Some(new);
        agent.status_msg = Some(record.msg.trim())
            .filter(|msg| !msg.is_empty())
            .map(str::to_owned);
        agent.status_ms = Some(now_ms());
        agent.updated_at = updated_at;
        if old == Some(new) || !new.is_alert() {
            return None;
        }
        Some(StatusChange {
            address: agent.address.clone(),
            status: new,
            text: agent.notification_text(),
        })
    }

    /// The agent the chat bubble shows:
    /// 1. blocked / error (earliest status change first);
    /// 2. done that Herdr has not marked seen (earliest completion first);
    /// 3. the working agent whose status changed last;
    /// 4. everything else (earliest first).
    ///
    /// The working agent's last status change is the best available proxy for
    /// "last interacted"; Herdr exposes no per-tab interaction timestamp.
    pub fn focus_with(&self, unseen_done: impl Fn(&str) -> bool) -> Option<&TerminalAgent> {
        let last_working = self
            .terminals
            .values()
            .filter(|agent| agent.live && agent.status == Some(AgentStatus::Working))
            .max_by_key(|agent| order_key(agent))
            .map(|agent| agent.address.as_str());
        let mut best: Option<(&TerminalAgent, u8, (u64, u64))> = None;
        for agent in self.terminals.values() {
            if !agent.live || agent.status.is_none() {
                continue;
            }
            let rank = match agent.status {
                Some(AgentStatus::Blocked | AgentStatus::Error) => 0,
                Some(AgentStatus::Done) if unseen_done(&agent.address) => 1,
                Some(AgentStatus::Working) if last_working == Some(agent.address.as_str()) => 2,
                _ => 3,
            };
            let key = order_key(agent);
            let better = match best {
                None => true,
                Some((_, best_rank, best_key)) => {
                    rank < best_rank || (rank == best_rank && key < best_key)
                }
            };
            if better {
                best = Some((agent, rank, key));
            }
        }
        best.map(|(agent, _, _)| agent)
    }

    /// Whether any live agent needs the chat UI. Blocked, working and error
    /// always do; done only counts while `unseen_done` says it has not been
    /// viewed in Herdr. Idle and plain shells never count.
    pub fn is_active_with(&self, unseen_done: impl Fn(&str) -> bool) -> bool {
        self.terminals.values().any(|agent| {
            agent.live
                && match agent.status {
                    Some(AgentStatus::Blocked | AgentStatus::Working | AgentStatus::Error) => true,
                    Some(AgentStatus::Done) => unseen_done(&agent.address),
                    Some(AgentStatus::Idle) | None => false,
                }
        })
    }

    /// Whether any live agent is blocked or errored. These states wait for the
    /// person, so the chat bubble shows them even while Herdr is frontmost or
    /// the bubble is collapsed.
    pub fn has_blocked_or_error(&self) -> bool {
        self.terminals.values().any(|agent| {
            agent.live
                && matches!(
                    agent.status,
                    Some(AgentStatus::Blocked | AgentStatus::Error)
                )
        })
    }

    /// Whether any live agent finished and Herdr has not marked it seen yet.
    /// Like blocked/error, these force the bubble open until viewed.
    pub fn has_unseen_done(&self, unseen_done: impl Fn(&str) -> bool) -> bool {
        self.terminals.values().any(|agent| {
            agent.live && agent.status == Some(AgentStatus::Done) && unseen_done(&agent.address)
        })
    }

    pub fn aggregate(&self) -> Aggregate {
        let mut aggregate = Aggregate::default();
        for agent in self.terminals.values() {
            if !agent.live {
                continue;
            }
            let Some(status) = agent.status else {
                continue;
            };
            aggregate.total += 1;
            match status {
                AgentStatus::Idle => {}
                AgentStatus::Working => aggregate.working += 1,
                AgentStatus::Done => aggregate.done += 1,
                AgentStatus::Error => aggregate.error += 1,
                AgentStatus::Blocked => aggregate.blocked += 1,
            }
            aggregate.status = Some(match aggregate.status {
                Some(current) => current.max(status),
                None => status,
            });
        }
        aggregate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn terminal(address: &str, state: &str, status: Option<&str>) -> Terminal {
        Terminal {
            address: address.to_owned(),
            title: format!("{address} title"),
            state: state.to_owned(),
            status: status.map(|state| StatusRecord {
                state: state.to_owned(),
                title: None,
                msg: String::new(),
            }),
            ..Terminal::default()
        }
    }

    fn status_event(address: &str, state: &str) -> Event {
        Event::Status {
            terminal: address.to_owned(),
            status: StatusRecord {
                state: state.to_owned(),
                title: None,
                msg: String::new(),
            },
        }
    }

    fn parsed_event(notification: Value) -> Event {
        parse_event(notification.get("params").unwrap())
    }

    #[test]
    fn aggregate_priority_is_blocked_error_working_done_idle() {
        let mut table = StatusTable::new();
        table.apply_snapshot(vec![
            terminal("t1", "live", Some("working")),
            terminal("t2", "live", Some("done")),
            terminal("t3", "live", Some("idle")),
        ]);
        // Stale done terminals must not mask a task that is still executing.
        assert_eq!(table.aggregate().status, Some(AgentStatus::Working));

        table.apply_event(status_event("t1", "done"));
        assert_eq!(table.aggregate().status, Some(AgentStatus::Done));

        table.apply_event(status_event("t1", "error"));
        assert_eq!(table.aggregate().status, Some(AgentStatus::Error));

        table.apply_event(status_event("t2", "blocked"));
        assert_eq!(table.aggregate().status, Some(AgentStatus::Blocked));
    }

    #[test]
    fn table_is_active_with_unseen_done() {
        let table = |state: Option<&str>| {
            let mut table = StatusTable::new();
            if let Some(state) = state {
                table.apply_snapshot(vec![terminal("t1", "live", Some(state))]);
            }
            table
        };
        for state in ["blocked", "working", "error"] {
            assert!(
                table(Some(state)).is_active_with(|_| false),
                "{state} should stay active even when seen"
            );
        }
        assert!(!table(Some("idle")).is_active_with(|_| true));
        assert!(!table(None).is_active_with(|_| true));

        let done = table(Some("done"));
        assert!(done.is_active_with(|_| true));
        assert!(!done.is_active_with(|_| false));

        let mut mixed = table(Some("done"));
        mixed.apply_snapshot(vec![
            terminal("t1", "live", Some("done")),
            terminal("t2", "live", Some("working")),
        ]);
        assert!(mixed.is_active_with(|_| false));
    }

    #[test]
    fn blocked_or_error_agents_need_manual_attention() {
        let table = |state: Option<&str>| {
            let mut table = StatusTable::new();
            if let Some(state) = state {
                table.apply_snapshot(vec![terminal("t1", "live", Some(state))]);
            }
            table
        };
        for state in ["blocked", "error"] {
            assert!(table(Some(state)).has_blocked_or_error(), "{state}");
        }
        for state in ["idle", "done", "working"] {
            assert!(!table(Some(state)).has_blocked_or_error(), "{state}");
        }
        assert!(!table(None).has_blocked_or_error());

        let mut finished = StatusTable::new();
        finished.apply_snapshot(vec![terminal("t1", "finished", Some("blocked"))]);
        assert!(!finished.has_blocked_or_error());
    }

    #[test]
    fn unseen_done_agents_force_attention_until_seen() {
        let mut table = StatusTable::new();
        table.apply_snapshot(vec![terminal("t1", "live", Some("done"))]);
        assert!(table.has_unseen_done(|address| address == "t1"));
        assert!(!table.has_unseen_done(|_| false));

        for state in ["blocked", "error", "working", "idle"] {
            let mut table = StatusTable::new();
            table.apply_snapshot(vec![terminal("t1", "live", Some(state))]);
            assert!(!table.has_unseen_done(|_| true), "{state}");
        }

        let mut finished = StatusTable::new();
        finished.apply_snapshot(vec![terminal("t1", "finished", Some("done"))]);
        assert!(!finished.has_unseen_done(|_| true));
    }

    #[test]
    fn aggregate_counts_only_live_terminals_with_status() {
        let mut table = StatusTable::new();
        table.apply_snapshot(vec![
            terminal("t1", "live", Some("working")),
            terminal("t2", "live", Some("working")),
            terminal("t3", "live", Some("blocked")),
            terminal("t4", "finished", None),
            terminal("t5", "live", None),
        ]);
        let aggregate = table.aggregate();
        assert_eq!(aggregate.status, Some(AgentStatus::Blocked));
        assert_eq!(aggregate.working, 2);
        assert_eq!(aggregate.blocked, 1);
        assert_eq!(aggregate.total, 3);
    }

    #[test]
    fn stale_status_on_finished_terminal_is_ignored() {
        let mut table = StatusTable::new();
        table.apply_snapshot(vec![terminal("t1", "finished", Some("blocked"))]);
        let aggregate = table.aggregate();
        assert_eq!(aggregate.status, None);
        assert_eq!(aggregate.total, 0);
    }

    #[test]
    fn initial_snapshot_never_notifies() {
        let mut table = StatusTable::new();
        table.apply_snapshot(vec![terminal("t1", "live", Some("blocked"))]);
        let aggregate = table.aggregate();
        assert_eq!(aggregate.blocked, 1);
    }

    #[test]
    fn created_alone_never_notifies() {
        let mut table = StatusTable::new();
        let change = table.apply_event(Event::Created(terminal("t1", "live", Some("blocked"))));
        assert!(change.is_none());
        assert_eq!(table.aggregate().blocked, 1);
    }

    #[test]
    fn status_after_created_notifies_on_change_edge() {
        let mut table = StatusTable::new();
        table.apply_event(Event::Created(terminal("t1", "live", Some("working"))));
        let change = table.apply_event(status_event("t1", "blocked")).unwrap();
        assert_eq!(change.address, "t1");
        assert_eq!(change.status, AgentStatus::Blocked);
        assert_eq!(change.text, "t1 title");
    }

    #[test]
    fn notifies_only_on_entering_blocked_or_done() {
        let mut table = StatusTable::new();
        table.apply_snapshot(vec![terminal("t1", "live", Some("working"))]);

        assert!(table.apply_event(status_event("t1", "idle")).is_none());
        assert!(table.apply_event(status_event("t1", "done")).is_some());
        assert!(table.apply_event(status_event("t1", "done")).is_none());
        assert!(table.apply_event(status_event("t1", "working")).is_none());
        assert!(table.apply_event(status_event("t1", "blocked")).is_some());
        assert!(table.apply_event(status_event("t1", "blocked")).is_none());
        assert!(table.apply_event(status_event("t1", "error")).is_none());
    }

    #[test]
    fn finished_and_lost_are_excluded_and_silent() {
        for event in [
            Event::Finished {
                terminal: "t1".to_owned(),
            },
            Event::Lost {
                terminal: "t1".to_owned(),
            },
        ] {
            let mut table = StatusTable::new();
            table.apply_snapshot(vec![terminal("t1", "live", Some("working"))]);
            assert!(table.apply_event(event).is_none());
            assert_eq!(table.aggregate().total, 0);
            assert!(table.apply_event(status_event("t1", "blocked")).is_none());
            assert_eq!(table.aggregate().status, None);
        }
    }

    #[test]
    fn removed_terminal_disappears() {
        let mut table = StatusTable::new();
        table.apply_snapshot(vec![terminal("t1", "live", Some("working"))]);
        table.apply_event(Event::Removed {
            terminal: "t1".to_owned(),
        });
        assert_eq!(table.aggregate().total, 0);
    }

    #[test]
    fn parent_and_child_count_separately() {
        let created = |address: &str, parent: &str| {
            json!({
                "jsonrpc": "2.0",
                "method": "event",
                "params": {
                    "type": "created",
                    "terminal": address,
                    "seq": 1,
                    "data": {
                        "address": address,
                        "state": "live",
                        "parent": parent,
                        "title": address,
                        "labels": {},
                        "status": {"state": "blocked"}
                    }
                }
            })
        };
        let mut table = StatusTable::new();
        table.apply_event(parsed_event(created("t1", "t0")));
        table.apply_event(parsed_event(created("t2", "t1")));
        assert_eq!(table.aggregate().blocked, 2);
        assert_eq!(table.aggregate().total, 2);
    }

    #[test]
    fn parses_real_created_terminal_json() {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "event",
            "params": {
                "type": "created",
                "terminal": "t7",
                "seq": 42,
                "data": {
                    "address": "t7",
                    "argv": ["claude"],
                    "cols": 120,
                    "created_ms": 1,
                    "cwd": "/tmp",
                    "die_with_parent": false,
                    "exit_code": null,
                    "finished_ms": null,
                    "identity": "",
                    "keep": false,
                    "labels": {"where": "repo", "herdr.tab": "1"},
                    "name": "claude",
                    "notify": false,
                    "parent": null,
                    "pid": 1,
                    "reason": null,
                    "reported": true,
                    "rm": false,
                    "rows": 40,
                    "screen": "exact",
                    "signal": null,
                    "state": "live",
                    "status": {"app": "claude-code", "state": "working", "msg": "", "title": "fix bug"},
                    "status_ms": 5,
                    "template": null,
                    "title": "fix bug",
                    "turn": 3
                }
            }
        });
        let Event::Created(terminal) = parsed_event(notification) else {
            panic!("expected created event");
        };
        assert_eq!(terminal.address, "t7");
        assert_eq!(terminal.title, "fix bug");
        assert_eq!(terminal.name.as_deref(), Some("claude"));
        assert_eq!(
            terminal.labels.get("where").map(String::as_str),
            Some("repo")
        );
        assert_eq!(terminal.state, "live");
        assert_eq!(
            terminal.status.as_ref().map(|status| status.state.as_str()),
            Some("working")
        );
        assert_eq!(terminal.status_ms, Some(5));

        let mut table = StatusTable::new();
        table.apply_event(Event::Created(terminal));
        assert_eq!(table.aggregate().status, Some(AgentStatus::Working));
    }

    #[test]
    fn parses_real_status_event_json() {
        let created = json!({
            "jsonrpc": "2.0",
            "method": "event",
            "params": {
                "type": "created",
                "terminal": "t20",
                "seq": 2098,
                "data": {"address": "t20", "state": "live", "title": "agent task"}
            }
        });
        // Captured from herdr-core 0.1.21: the root 7501 record is nested
        // under `data.status`, alongside `records` and `reported`.
        let status = json!({
            "jsonrpc": "2.0",
            "method": "event",
            "params": {
                "type": "status",
                "terminal": "t20",
                "seq": 2099,
                "data": {
                    "records": {},
                    "reported": true,
                    "status": {"state": "blocked", "app": "claude-code", "msg": "permission?"}
                }
            }
        });
        let mut table = StatusTable::new();
        table.apply_event(parsed_event(created));
        let change = table.apply_event(parsed_event(status)).unwrap();
        assert_eq!(change.status, AgentStatus::Blocked);
        assert_eq!(change.text, "agent task");
        assert_eq!(table.aggregate().blocked, 1);
    }

    #[test]
    fn real_status_wrapper_drives_animation_and_notification() {
        // Working -> done must update the aggregate (Mood) and produce the
        // change edge that notify.rs turns into a notification.
        let mut table = StatusTable::new();
        table.apply_snapshot(vec![terminal("t20", "live", Some("working"))]);

        let done = json!({
            "type": "status",
            "terminal": "t20",
            "data": {
                "records": {},
                "reported": true,
                "status": {"state": "done", "app": "claude-code", "msg": ""}
            }
        });
        let change = table.apply_event(parse_event(&done)).unwrap();
        assert_eq!(change.status, AgentStatus::Done);
        assert_eq!(change.text, "t20 title");
        assert_eq!(table.aggregate().status, Some(AgentStatus::Done));
    }

    #[test]
    fn malformed_status_payloads_are_ignored() {
        for data in [
            json!({"records": {}, "reported": false}),
            json!({"state": ""}),
            json!({"status": "not a record"}),
        ] {
            let event = json!({
                "type": "status",
                "terminal": "t1",
                "data": data,
            });
            assert!(
                matches!(parse_event(&event), Event::Ignored),
                "expected Ignored for {event}"
            );
        }
    }

    #[test]
    fn null_status_is_a_clear() {
        let clear = json!({
            "type": "status",
            "terminal": "t1",
            "data": {"records": {}, "reported": true, "status": null}
        });
        assert!(matches!(
            parse_event(&clear),
            Event::StatusCleared { ref terminal } if terminal == "t1"
        ));
    }

    #[test]
    fn status_clear_returns_terminal_to_idle_without_notifying() {
        let clear = |terminal: &str| {
            parse_event(&json!({
                "type": "status",
                "terminal": terminal,
                "data": {"records": {}, "reported": true, "status": null}
            }))
        };

        let mut table = StatusTable::new();
        table.apply_snapshot(vec![terminal("t1", "live", Some("working"))]);
        assert_eq!(table.aggregate().status, Some(AgentStatus::Working));

        assert!(table.apply_event(clear("t1")).is_none());
        let aggregate = table.aggregate();
        assert_eq!(aggregate.status, None);
        assert_eq!(aggregate.working, 0);
        assert_eq!(aggregate.total, 0);

        // Clearing a done terminal is not a new alert edge.
        table.apply_snapshot(vec![terminal("t1", "live", Some("done"))]);
        assert!(table.apply_event(clear("t1")).is_none());
        assert_eq!(table.aggregate().status, None);
    }

    #[test]
    fn unknown_status_is_ignored() {
        let mut table = StatusTable::new();
        table.apply_snapshot(vec![terminal("t1", "live", Some("working"))]);
        assert!(
            table
                .apply_event(status_event("t1", "something-new"))
                .is_none()
        );
        assert_eq!(table.aggregate().status, Some(AgentStatus::Working));
    }

    #[test]
    fn title_priority_is_title_name_where_address() {
        let mut table = StatusTable::new();
        let mut agent = terminal("t1", "live", Some("working"));
        agent.title = String::new();
        agent.name = None;
        table.apply_snapshot(vec![agent.clone()]);
        assert_eq!(
            table
                .apply_event(status_event("t1", "blocked"))
                .unwrap()
                .text,
            "t1"
        );

        agent.labels.insert("where".to_owned(), "repo".to_owned());
        table.apply_snapshot(vec![agent.clone()]);
        assert_eq!(
            table
                .apply_event(status_event("t1", "blocked"))
                .unwrap()
                .text,
            "repo"
        );

        agent.name = Some("claude".to_owned());
        table.apply_snapshot(vec![agent.clone()]);
        assert_eq!(
            table
                .apply_event(status_event("t1", "blocked"))
                .unwrap()
                .text,
            "claude"
        );

        agent.title = "title".to_owned();
        table.apply_snapshot(vec![agent]);
        assert_eq!(
            table
                .apply_event(status_event("t1", "blocked"))
                .unwrap()
                .text,
            "title"
        );
    }

    #[test]
    fn status_title_is_used_when_terminal_title_is_empty() {
        let mut table = StatusTable::new();
        let mut agent = terminal("t1", "live", Some("working"));
        agent.title = String::new();
        agent.status = Some(StatusRecord {
            state: "working".to_owned(),
            title: Some("from status".to_owned()),
            msg: String::new(),
        });
        table.apply_snapshot(vec![agent]);
        assert_eq!(
            table.apply_event(status_event("t1", "done")).unwrap().text,
            "from status"
        );
    }

    #[test]
    fn title_and_labels_events_update_notification_text() {
        let mut table = StatusTable::new();
        let mut agent = terminal("t1", "live", Some("working"));
        agent.title = String::new();
        table.apply_snapshot(vec![agent]);

        table.apply_event(Event::Title {
            terminal: "t1".to_owned(),
            title: "renamed".to_owned(),
        });
        assert_eq!(
            table
                .apply_event(status_event("t1", "blocked"))
                .unwrap()
                .text,
            "renamed"
        );

        table.apply_event(Event::Title {
            terminal: "t1".to_owned(),
            title: String::new(),
        });
        table.apply_event(Event::Labels {
            terminal: "t1".to_owned(),
            labels: BTreeMap::from([("where".to_owned(), "elsewhere".to_owned())]),
        });
        assert_eq!(
            table.apply_event(status_event("t1", "done")).unwrap().text,
            "elsewhere"
        );
    }

    #[test]
    fn parses_title_and_labels_event_shapes() {
        let title = json!({
            "type": "title",
            "terminal": "t1",
            "data": {"title": "new title"}
        });
        assert!(matches!(
            parse_event(&title),
            Event::Title { ref title, .. } if title == "new title"
        ));

        let labels = json!({
            "type": "labels",
            "terminal": "t1",
            "data": {"where": "repo"}
        });
        assert!(matches!(
            parse_event(&labels),
            Event::Labels { ref labels, .. }
                if labels.get("where").map(String::as_str) == Some("repo")
        ));
    }

    #[test]
    fn missed_and_heartbeat_are_distinct() {
        assert!(matches!(
            parse_event(&json!({"type": "missed"})),
            Event::Missed
        ));
        assert!(matches!(
            parse_event(&json!({"type": "heartbeat"})),
            Event::Ignored
        ));
        assert!(matches!(
            parse_event(&json!({"type": "turn", "terminal": "t1", "data": {}})),
            Event::Ignored
        ));
        let mut table = StatusTable::new();
        assert!(table.apply_event(Event::Missed).is_none());
        assert!(table.apply_event(Event::Ignored).is_none());
    }

    #[test]
    fn focus_orders_blocked_error_unseen_done_last_working() {
        let mut blocked = terminal("t1", "live", Some("blocked"));
        blocked.status_ms = Some(100);
        let mut error = terminal("t2", "live", Some("error"));
        error.status_ms = Some(200);
        let mut unseen_done = terminal("t3", "live", Some("done"));
        unseen_done.status_ms = Some(50);
        let mut old_working = terminal("t4", "live", Some("working"));
        old_working.status_ms = Some(300);
        let mut last_working = terminal("t5", "live", Some("working"));
        last_working.status_ms = Some(400);
        let mut seen_done = terminal("t6", "live", Some("done"));
        seen_done.status_ms = Some(10);
        let mut table = StatusTable::new();
        table.apply_snapshot(vec![
            seen_done,
            old_working,
            last_working,
            unseen_done,
            error,
            blocked,
        ]);

        let address = |table: &StatusTable, unseen: &[&str]| {
            table
                .focus_with(|address| unseen.contains(&address))
                .map(|agent| agent.address.clone())
        };
        // Blocked/error first, earliest status change wins.
        assert_eq!(address(&table, &["t3"]), Some("t1".to_owned()));
        table.apply_event(Event::StatusCleared {
            terminal: "t1".to_owned(),
        });
        assert_eq!(address(&table, &["t3"]), Some("t2".to_owned()));
        table.apply_event(Event::StatusCleared {
            terminal: "t2".to_owned(),
        });

        // Unseen done outranks the working tabs.
        assert_eq!(address(&table, &["t3"]), Some("t3".to_owned()));
        // Once seen, the working agent with the latest status change wins.
        assert_eq!(address(&table, &[]), Some("t5".to_owned()));
        table.apply_event(Event::StatusCleared {
            terminal: "t5".to_owned(),
        });
        assert_eq!(address(&table, &[]), Some("t4".to_owned()));
    }

    #[test]
    fn focus_picks_earliest_same_status_entry() {
        let mut older = terminal("t1", "live", Some("done"));
        older.status_ms = Some(100);
        let mut newer = terminal("t2", "live", Some("done"));
        newer.status_ms = Some(200);
        let mut table = StatusTable::new();
        table.apply_snapshot(vec![newer, older]);
        assert_eq!(
            table
                .focus_with(|_| true)
                .map(|agent| agent.address.as_str()),
            Some("t1")
        );
    }

    #[test]
    fn focus_ignores_plain_shells() {
        let table_focus = {
            let mut table = StatusTable::new();
            table.apply_snapshot(vec![terminal("t1", "live", None)]);
            table
                .focus_with(|_| true)
                .map(|agent| agent.address.clone())
        };
        assert_eq!(table_focus, None);
    }

    #[test]
    fn status_message_tracks_updates_and_clears() {
        let mut table = StatusTable::new();
        table.apply_snapshot(vec![terminal("t1", "live", Some("working"))]);
        assert_eq!(
            table
                .focus_with(|_| true)
                .and_then(|agent| agent.status_msg()),
            None
        );

        let update = json!({
            "type": "status",
            "terminal": "t1",
            "data": {"status": {"state": "working", "msg": "Running tests"}}
        });
        table.apply_event(parse_event(&update));
        assert_eq!(
            table
                .focus_with(|_| true)
                .and_then(|agent| agent.status_msg()),
            Some("Running tests")
        );

        let clear = json!({
            "type": "status",
            "terminal": "t1",
            "data": {"status": null}
        });
        table.apply_event(parse_event(&clear));
        assert_eq!(table.focus_with(|_| true), None);
        assert_eq!(table.aggregate().status, None);
    }

    #[test]
    fn finished_event_json_is_recognized() {
        let finished = json!({
            "type": "finished",
            "terminal": "t1",
            "seq": 3,
            "data": {"exit_code": 0, "finished_ms": 123, "reason": "exited"}
        });
        assert!(matches!(
            parse_event(&finished),
            Event::Finished { ref terminal } if terminal == "t1"
        ));
    }
}
