//! herdr-core JSON-RPC client.
//!
//! Runs on a background thread with its own tokio runtime and forwards
//! snapshots, parsed events, and disconnect notices to the UI thread over a
//! std channel. The UI thread never blocks on this module.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
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

/// How the JSON-RPC session reaches herdr-core.
#[derive(Debug, Clone)]
enum Transport {
    /// Unix domain socket, discovered from the environment.
    #[cfg(unix)]
    Unix(PathBuf),
    /// `hc connect --session <session>` over stdio.
    #[cfg(windows)]
    HcConnect { program: PathBuf, session: String },
}

/// Start the background client. Failures are logged and retried; nothing here
/// panics or exits the process.
pub fn spawn(sender: Sender<HerdrMessage>) {
    spawn_thread(sender, RECONNECT_DELAY, discover_transport);
}

fn spawn_thread<D>(sender: Sender<HerdrMessage>, reconnect_delay: Duration, discover: D)
where
    D: Fn() -> Result<Transport> + Send + 'static,
{
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
                let result = discover()
                    .and_then(|transport| runtime.block_on(run_once(&transport, &sender)));
                if let Err(error) = result {
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

fn discover_transport() -> Result<Transport> {
    #[cfg(unix)]
    {
        Ok(Transport::Unix(discover_socket()?))
    }
    #[cfg(windows)]
    {
        discover_hc_connect()
    }
    #[cfg(not(any(unix, windows)))]
    {
        bail!("no herdr-core transport for this platform")
    }
}

/// Unix socket discovery: `HC_TERMINAL_SOCKET`, else the per-session socket
/// under `$XDG_STATE_HOME` (default `~/.local/state`).
#[cfg(unix)]
fn discover_socket() -> Result<PathBuf> {
    if let Some(path) = non_empty(std::env::var_os("HC_TERMINAL_SOCKET")) {
        return Ok(PathBuf::from(path));
    }
    let session = session_name();
    let state_dir = non_empty(std::env::var_os("XDG_STATE_HOME"))
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".local").join("state")))
        .context("cannot locate an XDG state directory")?;
    Ok(socket_path(&state_dir, &session))
}

/// Windows discovery: `hc.exe` / `herdr-core.exe` on PATH, then the default
/// herdr-core install under `%LOCALAPPDATA%\Programs`.
#[cfg(windows)]
fn discover_hc_connect() -> Result<Transport> {
    let program = find_hc_program().context(
        "cannot find hc.exe or herdr-core.exe (tried PATH and %LOCALAPPDATA%\\Programs\\herdr-core)",
    )?;
    Ok(Transport::HcConnect {
        program,
        session: session_name(),
    })
}

#[cfg(windows)]
fn find_hc_program() -> Option<PathBuf> {
    const NAMES: [&str; 2] = ["hc.exe", "herdr-core.exe"];
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in NAMES {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    let programs = PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
        .join("Programs")
        .join("herdr-core");
    NAMES
        .iter()
        .map(|name| programs.join(name))
        .find(|path| path.is_file())
}

fn session_name() -> String {
    non_empty(std::env::var_os("HC_SESSION"))
        .map(|session| session.to_string_lossy().into_owned())
        .unwrap_or_else(|| DEFAULT_SESSION.to_owned())
}

fn non_empty(value: Option<OsString>) -> Option<OsString> {
    value.filter(|value| !value.is_empty())
}

#[cfg(unix)]
fn socket_path(state_dir: &std::path::Path, session: &str) -> PathBuf {
    state_dir
        .join("herdr-core")
        .join(session)
        .join(format!("{session}.sock"))
}

async fn run_once(transport: &Transport, sender: &Sender<HerdrMessage>) -> Result<()> {
    match transport {
        #[cfg(unix)]
        Transport::Unix(path) => {
            let stream = tokio::net::UnixStream::connect(path)
                .await
                .with_context(|| format!("connecting to {}", path.display()))?;
            let (read, write) = stream.into_split();
            run_session(read, write, sender).await
        }
        #[cfg(windows)]
        Transport::HcConnect { program, session } => {
            use windows::Win32::System::Threading::CREATE_NO_WINDOW;

            let mut command = tokio::process::Command::new(program);
            command
                .arg("connect")
                .arg("--session")
                .arg(session)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .creation_flags(CREATE_NO_WINDOW.0);
            let mut child = command
                .spawn()
                .with_context(|| format!("starting {} connect", program.display()))?;
            let stdin = child.stdin.take().context("hc connect exposes no stdin")?;
            let stdout = child
                .stdout
                .take()
                .context("hc connect exposes no stdout")?;
            let result = run_session(stdout, stdin, sender).await;
            let _ = child.start_kill();
            result
        }
    }
}

async fn run_session<R, W>(read: R, mut write: W, sender: &Sender<HerdrMessage>) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
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

async fn read_response<R>(reader: &mut R, id: u64) -> Result<Value>
where
    R: AsyncBufRead + Unpin,
{
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

async fn read_line<R>(reader: &mut R) -> Result<String>
where
    R: AsyncBufRead + Unpin,
{
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

async fn write_json<W>(write: &mut W, value: &Value) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
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

#[cfg(all(test, unix))]
fn spawn_with(sender: Sender<HerdrMessage>, transport: Transport, reconnect_delay: Duration) {
    spawn_thread(sender, reconnect_delay, move || Ok(transport.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    use serde_json::{Value, json};

    #[cfg(unix)]
    use std::sync::mpsc::Receiver;
    #[cfg(unix)]
    use std::time::Duration;

    fn live_terminal(address: &str, state: &str) -> Value {
        json!({
            "address": address,
            "state": "live",
            "title": address,
            "status": {"state": state}
        })
    }

    #[cfg(unix)]
    fn recv(rx: &Receiver<HerdrMessage>) -> HerdrMessage {
        rx.recv_timeout(Duration::from_secs(2)).unwrap()
    }

    /// Read one non-empty JSON line from an async peer.
    async fn read_json<R>(reader: &mut R) -> Value
    where
        R: AsyncBufRead + Unpin,
    {
        loop {
            let mut line = String::new();
            let read = reader.read_line(&mut line).await.unwrap();
            assert!(read > 0, "peer closed before the handshake finished");
            let line = line.trim();
            if !line.is_empty() {
                return serde_json::from_str(line).unwrap();
            }
        }
    }

    async fn write_json_line<W>(write: &mut W, value: Value)
    where
        W: AsyncWrite + Unpin,
    {
        let mut line = serde_json::to_vec(&value).unwrap();
        line.push(b'\n');
        write.write_all(&line).await.unwrap();
        write.flush().await.unwrap();
    }

    /// The same handshake runs over any transport; this one uses an in-memory
    /// duplex so it also compiles and runs on Windows.
    #[test]
    fn duplex_handshake_snapshot_and_events() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (tx, rx) = channel();
        runtime.block_on(async {
            let (client, server) = tokio::io::duplex(64 * 1024);
            let (read, write) = tokio::io::split(client);
            let server = tokio::spawn(async move {
                let (read, mut write) = tokio::io::split(server);
                let mut reader = BufReader::new(read);
                let hello = read_json(&mut reader).await;
                assert_eq!(hello["method"], "hello");
                write_json_line(
                    &mut write,
                    json!({"jsonrpc": "2.0", "id": 1, "result": {"generation": 1}}),
                )
                .await;

                let subscribe = read_json(&mut reader).await;
                assert_eq!(subscribe["method"], "events.subscribe");
                write_json_line(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": 2,
                        "result": {"seq": 7, "terminals": [live_terminal("t1", "working")]}
                    }),
                )
                .await;
                write_json_line(
                    &mut write,
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
                )
                .await;
                // Dropping the server half ends the session with an EOF.
            });
            let result = run_session(read, write, &tx).await;
            assert!(result.is_err(), "the closed duplex must end the session");
            server.await.unwrap();
        });

        let mut messages = rx.try_iter();
        let HerdrMessage::Snapshot { terminals } = messages.next().unwrap() else {
            panic!("expected a snapshot");
        };
        let mut table = status::StatusTable::new();
        table.apply_snapshot(terminals);
        assert_eq!(table.aggregate().status, Some(status::AgentStatus::Working));

        let HerdrMessage::Event(event) = messages.next().unwrap() else {
            panic!("expected an event");
        };
        let change = table.apply_event(*event).unwrap();
        assert_eq!(change.status, status::AgentStatus::Blocked);
        assert_eq!(change.text, "t1");
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::{UnixListener, UnixStream as StdUnixStream};
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU32, Ordering};

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

        fn write_std_json_line(stream: &mut StdUnixStream, value: Value) {
            writeln!(stream, "{value}").unwrap();
            stream.flush().unwrap();
        }

        fn handshake(stream: &mut StdUnixStream, terminals: Value) {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let hello = read_json_line(&mut reader);
            assert_eq!(hello["method"], "hello");
            write_std_json_line(
                stream,
                json!({"jsonrpc": "2.0", "id": 1, "result": {"generation": 1}}),
            );

            let subscribe = read_json_line(&mut reader);
            assert_eq!(subscribe["method"], "events.subscribe");
            write_std_json_line(
                stream,
                json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "result": {"seq": 7, "terminals": terminals}
                }),
            );
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
                write_std_json_line(
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
                write_std_json_line(
                    &mut stream,
                    json!({"jsonrpc": "2.0", "method": "heartbeat", "params": {}}),
                );
                std::thread::sleep(Duration::from_millis(200));
            });

            let (tx, rx) = channel();
            spawn_with(
                tx,
                Transport::Unix(socket.path().to_path_buf()),
                Duration::from_millis(50),
            );

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
                    write_std_json_line(
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
            spawn_with(
                tx,
                Transport::Unix(socket.path().to_path_buf()),
                Duration::from_millis(20),
            );

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
}
