//! herdr-core JSON-RPC client.
//!
//! Runs on a background thread with its own tokio runtime and forwards
//! snapshots, parsed events, and disconnect notices to the UI thread over a
//! std channel. The UI thread never blocks on this module.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::time::timeout;

use crate::status::{self, Event, Terminal};

const READ_TIMEOUT: Duration = Duration::from_secs(45);
const RECONNECT_DELAY: Duration = Duration::from_secs(2);
const DEFAULT_SESSION: &str = "default";

#[derive(Debug, Clone)]
pub enum HerdrMessage {
    Snapshot { terminals: Vec<Terminal> },
    // A created terminal carries the full record; boxing keeps the enum
    // small enough for the channel without changing `Event`.
    Event(Box<Event>),
    Disconnected,
}

/// Start the background client. Failures are logged and retried; nothing here
/// panics or exits the process.
pub fn spawn(sender: Sender<HerdrMessage>) {
    let socket = match discover_socket() {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("herdr-pet: {error:#}");
            return;
        }
    };
    spawn_with(sender, socket, RECONNECT_DELAY);
}

fn spawn_with(sender: Sender<HerdrMessage>, socket: PathBuf, reconnect_delay: Duration) {
    let thread = std::thread::Builder::new()
        .name("herdr-rpc".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("herdr-pet: cannot start tokio runtime: {error}");
                    return;
                }
            };
            let mut last_error = String::new();
            loop {
                if let Err(error) = runtime.block_on(run_once(&socket, &sender)) {
                    let message = format!("{error:#}");
                    if message != last_error {
                        eprintln!("herdr-pet: {message}");
                        last_error = message;
                    }
                }
                if sender.send(HerdrMessage::Disconnected).is_err() {
                    return;
                }
                std::thread::sleep(reconnect_delay);
            }
        });
    if let Err(error) = thread {
        eprintln!("herdr-pet: cannot start herdr thread: {error}");
    }
}

/// Socket discovery: `HC_TERMINAL_SOCKET`, else the per-session socket under
/// `$XDG_STATE_HOME` (default `~/.local/state`).
pub fn discover_socket() -> Result<PathBuf> {
    if let Some(path) = non_empty(std::env::var_os("HC_TERMINAL_SOCKET")) {
        return Ok(PathBuf::from(path));
    }
    let session = non_empty(std::env::var_os("HC_SESSION"))
        .map(|session| session.to_string_lossy().into_owned())
        .unwrap_or_else(|| DEFAULT_SESSION.to_owned());
    let state_dir = non_empty(std::env::var_os("XDG_STATE_HOME"))
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".local").join("state")))
        .context("cannot locate an XDG state directory")?;
    Ok(socket_path(&state_dir, &session))
}

fn non_empty(value: Option<OsString>) -> Option<OsString> {
    value.filter(|value| !value.is_empty())
}

fn socket_path(state_dir: &Path, session: &str) -> PathBuf {
    state_dir
        .join("herdr-core")
        .join(session)
        .join(format!("{session}.sock"))
}

async fn run_once(socket: &Path, sender: &Sender<HerdrMessage>) -> Result<()> {
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connecting to {}", socket.display()))?;
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);

    write_json(
        &mut write,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "hello",
            "params": {"generation": 1}
        }),
    )
    .await?;
    read_response(&mut reader, 1).await?;

    write_json(
        &mut write,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "events.subscribe",
            "params": {}
        }),
    )
    .await?;
    let response = read_response(&mut reader, 2).await?;
    let result = response
        .get("result")
        .context("events.subscribe response has no result")?;
    let terminals: Vec<Terminal> = serde_json::from_value(
        result
            .get("terminals")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    )
    .context("parsing terminal snapshot")?;
    send(sender, HerdrMessage::Snapshot { terminals })?;

    loop {
        let line = timeout(READ_TIMEOUT, read_line(&mut reader))
            .await
            .context("timed out waiting for herdr-core")??;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let message: Value = serde_json::from_str(line).context("parsing herdr-core message")?;
        match message.get("method").and_then(Value::as_str) {
            Some("event") => {
                let params = message.get("params").unwrap_or(&Value::Null);
                let event = status::parse_event(params);
                if matches!(&event, Event::Missed) {
                    bail!("subscription fell behind");
                }
                send(sender, HerdrMessage::Event(Box::new(event)))?;
            }
            Some("missed") => bail!("subscription fell behind"),
            // `heartbeat` and unknown notifications only keep the socket warm.
            _ => {}
        }
    }
}

async fn read_response(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    id: u64,
) -> Result<Value> {
    loop {
        let line = timeout(READ_TIMEOUT, read_line(reader))
            .await
            .context("timed out during herdr-core handshake")??;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if message.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = message.get("error") {
            bail!("herdr-core rejected request {id}: {error}");
        }
        return Ok(message);
    }
}

async fn read_line(reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> Result<String> {
    let mut line = String::new();
    let read = reader
        .read_line(&mut line)
        .await
        .context("reading from herdr-core")?;
    if read == 0 {
        bail!("herdr-core closed the connection");
    }
    Ok(line)
}

async fn write_json(write: &mut OwnedWriteHalf, value: &Value) -> Result<()> {
    let mut line = serde_json::to_vec(value).context("serializing request")?;
    line.push(b'\n');
    write
        .write_all(&line)
        .await
        .context("writing to herdr-core")?;
    write.flush().await.context("flushing to herdr-core")?;
    Ok(())
}

fn send(sender: &Sender<HerdrMessage>, message: HerdrMessage) -> Result<()> {
    sender
        .send(message)
        .map_err(|_| anyhow!("UI channel closed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream as StdUnixStream};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::mpsc::{Receiver, channel};
    use std::time::Duration;

    use serde_json::json;

    struct TempSocket(PathBuf);

    impl TempSocket {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let path = PathBuf::from(format!(
                "/tmp/herdr-pet-test-{}-{}.sock",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_file(&path);
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn bind(&self) -> UnixListener {
            UnixListener::bind(&self.0).unwrap()
        }
    }

    impl Drop for TempSocket {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn read_json_line(reader: &mut BufReader<StdUnixStream>) -> Value {
        loop {
            let mut line = String::new();
            let read = reader.read_line(&mut line).unwrap();
            assert!(read > 0, "client closed before the handshake finished");
            let line = line.trim();
            if !line.is_empty() {
                return serde_json::from_str(line).unwrap();
            }
        }
    }

    fn write_json_line(stream: &mut StdUnixStream, value: Value) {
        writeln!(stream, "{value}").unwrap();
        stream.flush().unwrap();
    }

    fn handshake(stream: &mut StdUnixStream, terminals: Value) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let hello = read_json_line(&mut reader);
        assert_eq!(hello["method"], "hello");
        write_json_line(
            stream,
            json!({"jsonrpc": "2.0", "id": 1, "result": {"generation": 1}}),
        );

        let subscribe = read_json_line(&mut reader);
        assert_eq!(subscribe["method"], "events.subscribe");
        write_json_line(
            stream,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {"seq": 7, "terminals": terminals}
            }),
        );
    }

    fn live_terminal(address: &str, state: &str) -> Value {
        json!({
            "address": address,
            "state": "live",
            "title": address,
            "status": {"state": state}
        })
    }

    fn recv(rx: &Receiver<HerdrMessage>) -> HerdrMessage {
        rx.recv_timeout(Duration::from_secs(2)).unwrap()
    }

    #[test]
    fn socket_paths_follow_session_layout() {
        assert_eq!(
            socket_path(Path::new("/state"), "default"),
            PathBuf::from("/state/herdr-core/default/default.sock")
        );
        assert_eq!(
            socket_path(Path::new("/state"), "work"),
            PathBuf::from("/state/herdr-core/work/work.sock")
        );
    }

    #[test]
    fn handshake_snapshot_and_events() {
        let socket = TempSocket::new();
        let listener = socket.bind();

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            handshake(&mut stream, json!([live_terminal("t1", "working")]));
            write_json_line(
                &mut stream,
                json!({
                    "jsonrpc": "2.0",
                    "method": "event",
                    "params": {
                        "type": "status",
                        "terminal": "t1",
                        "seq": 8,
                        "data": {
                            "records": {},
                            "reported": true,
                            "status": {"state": "blocked", "app": "claude-code"}
                        }
                    }
                }),
            );
            write_json_line(
                &mut stream,
                json!({"jsonrpc": "2.0", "method": "heartbeat", "params": {}}),
            );
            std::thread::sleep(Duration::from_millis(200));
        });

        let (tx, rx) = channel();
        spawn_with(tx, socket.path().to_path_buf(), Duration::from_millis(50));

        let HerdrMessage::Snapshot { terminals } = recv(&rx) else {
            panic!("expected a snapshot");
        };
        let mut table = status::StatusTable::new();
        table.apply_snapshot(terminals);
        assert_eq!(table.aggregate().status, Some(status::AgentStatus::Working));

        let HerdrMessage::Event(event) = recv(&rx) else {
            panic!("expected an event");
        };
        let change = table.apply_event(*event).unwrap();
        assert_eq!(change.status, status::AgentStatus::Blocked);
        assert_eq!(change.text, "t1");

        server.join().unwrap();
    }

    #[test]
    fn missed_disconnects_and_resubscribes() {
        let socket = TempSocket::new();
        let listener = socket.bind();

        let server = std::thread::spawn(move || {
            {
                let (mut stream, _) = listener.accept().unwrap();
                handshake(&mut stream, json!([live_terminal("t1", "working")]));
                write_json_line(
                    &mut stream,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "event",
                        "params": {"type": "missed", "data": {}}
                    }),
                );
                std::thread::sleep(Duration::from_millis(50));
            }
            let (mut stream, _) = listener.accept().unwrap();
            handshake(&mut stream, json!([live_terminal("t1", "blocked")]));
            std::thread::sleep(Duration::from_millis(100));
        });

        let (tx, rx) = channel();
        spawn_with(tx, socket.path().to_path_buf(), Duration::from_millis(20));

        let HerdrMessage::Snapshot { terminals, .. } = recv(&rx) else {
            panic!("expected the first snapshot");
        };
        assert_eq!(
            terminals[0]
                .status
                .as_ref()
                .map(|status| status.state.as_str()),
            Some("working")
        );

        assert!(matches!(recv(&rx), HerdrMessage::Disconnected));

        let HerdrMessage::Snapshot { terminals, .. } = recv(&rx) else {
            panic!("expected the resubscribe snapshot");
        };
        assert_eq!(
            terminals[0]
                .status
                .as_ref()
                .map(|status| status.state.as_str()),
            Some("blocked")
        );

        server.join().unwrap();
    }
}
