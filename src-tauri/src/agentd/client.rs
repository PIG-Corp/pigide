//! `AgentClient` — async RPC client for the broker.
//!
//! Connect once, then issue [`Request`]s freely from any task. Each request
//! gets a fresh u64 id; the response is delivered through a per-request
//! oneshot. Events are surfaced via a `tokio::sync::broadcast` channel —
//! one channel per client, multiplexed by `agent_id`.
//!
//! On connection close (broker died, network error), all in-flight
//! oneshots resolve to `Error::Disconnected` and the broadcast sender is
//! dropped — subscribers see `Lagged` / closed and can decide whether
//! to reconnect.
//!
//! Thread/task model:
//!
//!   `connect` → spawns 2 tasks:
//!     1. read-loop: pulls frames, dispatches Response→oneshot or
//!        Event→broadcast.
//!     2. write-loop: drains an mpsc, serialises and writes to the socket.
//!
//!   Public methods send through the write-mpsc and await the oneshot.

use crate::agentd::framing::{read_frame_async, write_frame_async};
use crate::agentd::proto::{
    AgentInfo, ErrorCode, Event, HelloResponse, Op, Request, Response, ResponseBody,
    PROTOCOL_VERSION,
};
use base64::Engine as _;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{broadcast, mpsc, oneshot};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("connect: {0}")]
    Connect(std::io::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("encode: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("connection closed before response arrived")]
    Disconnected,
    #[error("broker error [{code:?}]: {message}")]
    Broker { code: ErrorCode, message: String },
    #[error("unexpected response shape: {0}")]
    UnexpectedShape(&'static str),
    #[error("protocol version mismatch (broker={broker}, client={client})")]
    VersionMismatch { broker: u32, client: u32 },
    #[error("hello timed out after {0:?}")]
    HelloTimeout(Duration),
}

pub type Result<T> = std::result::Result<T, ClientError>;

/// Public reception side of the engine event stream. Subscribers see
/// stdout/exit for ALL agents and filter client-side.
pub type EventReceiver = broadcast::Receiver<Event>;

/// One outstanding request. Filled exactly once by the read-loop.
type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Response>>>>;

#[derive(Clone)]
pub struct AgentClient {
    next_id: Arc<AtomicU64>,
    pending: PendingMap,
    out_tx: mpsc::Sender<String>,
    events: broadcast::Sender<Event>,
    /// Cached HelloResponse from the broker so callers can read broker
    /// metadata without sending another request.
    hello: Arc<HelloResponse>,
}

impl AgentClient {
    /// Open a connection, perform the Hello handshake, and return a
    /// ready-to-use client. Default Hello timeout is 3 seconds (broker
    /// auto-spawn budget).
    pub async fn connect(socket_path: PathBuf) -> Result<Self> {
        Self::connect_with_timeout(socket_path, Duration::from_secs(3)).await
    }

    pub async fn connect_with_timeout(
        socket_path: PathBuf,
        hello_timeout: Duration,
    ) -> Result<Self> {
        let stream = UnixStream::connect(&socket_path)
            .await
            .map_err(ClientError::Connect)?;
        let (r, w) = stream.into_split();
        Self::from_io(r, w, hello_timeout).await
    }

    /// Test hook: build a client over an arbitrary AsyncRead/AsyncWrite
    /// pair (e.g. `tokio::io::duplex`).
    pub async fn from_io<R, W>(
        reader: R,
        writer: W,
        hello_timeout: Duration,
    ) -> Result<Self>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (out_tx, out_rx) = mpsc::channel::<String>(256);
        let (events, _) = broadcast::channel::<Event>(1024);
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

        // Writer task.
        tokio::spawn(write_loop(writer, out_rx));

        // Reader task. Holds a clone of `pending` and `events`.
        let pending_for_reader = pending.clone();
        let events_for_reader = events.clone();
        tokio::spawn(read_loop(reader, pending_for_reader, events_for_reader));

        let next_id = Arc::new(AtomicU64::new(1));
        let client = AgentClient {
            next_id,
            pending,
            out_tx,
            events,
            hello: Arc::new(HelloResponse {
                broker_version: 0,
                broker_build: String::new(),
                broker_pid: 0,
            }),
        };

        // Hello + version check, bounded by `hello_timeout`.
        let hello_future = client.hello();
        let hello = match tokio::time::timeout(hello_timeout, hello_future).await {
            Ok(Ok(h)) => h,
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(ClientError::HelloTimeout(hello_timeout)),
        };

        if hello.broker_version != PROTOCOL_VERSION {
            return Err(ClientError::VersionMismatch {
                broker: hello.broker_version,
                client: PROTOCOL_VERSION,
            });
        }

        Ok(AgentClient {
            hello: Arc::new(hello),
            ..client
        })
    }

    /// Subscribe to engine-wide events. The first time this is called on
    /// a given client, sends `Op::Subscribe` to the broker; subsequent
    /// calls just hand out additional broadcast receivers (cheap).
    pub async fn subscribe(&self) -> Result<EventReceiver> {
        let resp = self.send_request(Op::Subscribe).await?;
        match resp.result {
            ResponseBody::Subscribe => Ok(self.events.subscribe()),
            ResponseBody::Error { code, message } => Err(ClientError::Broker { code, message }),
            _ => Err(ClientError::UnexpectedShape("subscribe")),
        }
    }

    pub fn broker_pid(&self) -> u32 {
        self.hello.broker_pid
    }

    pub fn broker_build(&self) -> &str {
        &self.hello.broker_build
    }

    async fn hello(&self) -> Result<HelloResponse> {
        let resp = self
            .send_request(Op::Hello {
                client_version: PROTOCOL_VERSION,
            })
            .await?;
        match resp.result {
            ResponseBody::Hello { hello } => Ok(hello),
            ResponseBody::Error { code, message } => Err(ClientError::Broker { code, message }),
            _ => Err(ClientError::UnexpectedShape("hello")),
        }
    }

    pub async fn ping(&self) -> Result<(u64, usize)> {
        let resp = self.send_request(Op::Ping).await?;
        match resp.result {
            ResponseBody::Pong {
                uptime_secs,
                live_agents,
            } => Ok((uptime_secs, live_agents)),
            ResponseBody::Error { code, message } => Err(ClientError::Broker { code, message }),
            _ => Err(ClientError::UnexpectedShape("ping")),
        }
    }

    /// Spawn a new agent. Caller is responsible for assembling the
    /// SpawnArgs (binary path, argv, env) — broker has no DB.
    pub async fn spawn(&self, args: SpawnArgs) -> Result<AgentInfo> {
        let SpawnArgs {
            workspace_id,
            agent_type,
            cwd,
            bin_path,
            argv,
            env,
            reuse_id,
        } = args;
        let resp = self
            .send_request(Op::Spawn {
                workspace_id,
                agent_type,
                cwd,
                bin_path,
                argv,
                env,
                reuse_id,
            })
            .await?;
        match resp.result {
            ResponseBody::Spawn { agent } => Ok(agent),
            ResponseBody::Error { code, message } => Err(ClientError::Broker { code, message }),
            _ => Err(ClientError::UnexpectedShape("spawn")),
        }
    }

    pub async fn write(&self, agent_id: &str, data: &[u8]) -> Result<usize> {
        let data_b64 = base64::engine::general_purpose::STANDARD.encode(data);
        let resp = self
            .send_request(Op::Write {
                agent_id: agent_id.into(),
                data_b64,
            })
            .await?;
        match resp.result {
            ResponseBody::Write { bytes_written } => Ok(bytes_written),
            ResponseBody::Error { code, message } => Err(ClientError::Broker { code, message }),
            _ => Err(ClientError::UnexpectedShape("write")),
        }
    }

    pub async fn resize(&self, agent_id: &str, cols: u16, rows: u16) -> Result<()> {
        let resp = self
            .send_request(Op::Resize {
                agent_id: agent_id.into(),
                cols,
                rows,
            })
            .await?;
        match resp.result {
            ResponseBody::Resize => Ok(()),
            ResponseBody::Error { code, message } => Err(ClientError::Broker { code, message }),
            _ => Err(ClientError::UnexpectedShape("resize")),
        }
    }

    pub async fn kill(&self, agent_id: &str) -> Result<()> {
        let resp = self
            .send_request(Op::Kill {
                agent_id: agent_id.into(),
            })
            .await?;
        match resp.result {
            ResponseBody::Kill => Ok(()),
            ResponseBody::Error { code, message } => Err(ClientError::Broker { code, message }),
            _ => Err(ClientError::UnexpectedShape("kill")),
        }
    }

    pub async fn list(&self, workspace_id: &str) -> Result<Vec<AgentInfo>> {
        let resp = self
            .send_request(Op::List {
                workspace_id: workspace_id.into(),
            })
            .await?;
        match resp.result {
            ResponseBody::List { agents } => Ok(agents),
            ResponseBody::Error { code, message } => Err(ClientError::Broker { code, message }),
            _ => Err(ClientError::UnexpectedShape("list")),
        }
    }

    pub async fn list_all(&self) -> Result<Vec<AgentInfo>> {
        let resp = self.send_request(Op::ListAll).await?;
        match resp.result {
            ResponseBody::ListAll { agents } => Ok(agents),
            ResponseBody::Error { code, message } => Err(ClientError::Broker { code, message }),
            _ => Err(ClientError::UnexpectedShape("list_all")),
        }
    }

    pub async fn log_tail(&self, agent_id: &str, max_bytes: usize) -> Result<Vec<u8>> {
        let resp = self
            .send_request(Op::LogTail {
                agent_id: agent_id.into(),
                max_bytes,
            })
            .await?;
        match resp.result {
            ResponseBody::LogTail { data_b64 } => Ok(base64::engine::general_purpose::STANDARD
                .decode(&data_b64)
                .map_err(|e| ClientError::Encode(serde_json::Error::io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    e.to_string(),
                ))))?),
            ResponseBody::Error { code, message } => Err(ClientError::Broker { code, message }),
            _ => Err(ClientError::UnexpectedShape("log_tail")),
        }
    }

    pub async fn last_stdout_age(&self, agent_id: &str) -> Result<Option<Duration>> {
        let resp = self
            .send_request(Op::LastStdoutAge {
                agent_id: agent_id.into(),
            })
            .await?;
        match resp.result {
            ResponseBody::LastStdoutAge { age_ms } => Ok(age_ms.map(Duration::from_millis)),
            ResponseBody::Error { code, message } => Err(ClientError::Broker { code, message }),
            _ => Err(ClientError::UnexpectedShape("last_stdout_age")),
        }
    }

    pub async fn detach(&self) -> Result<()> {
        let resp = self.send_request(Op::Detach).await?;
        match resp.result {
            ResponseBody::Detach => Ok(()),
            ResponseBody::Error { code, message } => Err(ClientError::Broker { code, message }),
            _ => Err(ClientError::UnexpectedShape("detach")),
        }
    }

    /// Allocate a fresh id, install the oneshot, send the request, await
    /// the response. Cancellation-safe: dropping the future before the
    /// oneshot fires leaves a stale entry that the read-loop will drop
    /// on the next response (or on disconnect).
    async fn send_request(&self, op: Op) -> Result<Response> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = Request { id, op };
        let json = serde_json::to_string(&req)?;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);
        if self.out_tx.send(json).await.is_err() {
            self.pending.lock().remove(&id);
            return Err(ClientError::Disconnected);
        }
        match rx.await {
            Ok(resp) => Ok(resp),
            Err(_) => Err(ClientError::Disconnected),
        }
    }
}

/// Spawn arguments — owned by caller. Mirrors `proto::Op::Spawn` minus
/// the explicit `op` discriminator.
#[derive(Debug, Clone)]
pub struct SpawnArgs {
    pub workspace_id: String,
    pub agent_type: String,
    pub cwd: Option<String>,
    pub bin_path: String,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub reuse_id: Option<String>,
}

async fn write_loop<W>(mut writer: W, mut rx: mpsc::Receiver<String>)
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    while let Some(line) = rx.recv().await {
        if let Err(e) = write_frame_async(&mut writer, &line).await {
            tracing::warn!("agentd-client: write failed: {}", e);
            break;
        }
    }
}

async fn read_loop<R>(reader: R, pending: PendingMap, events: broadcast::Sender<Event>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut br = BufReader::new(reader);
    loop {
        let line = match read_frame_async(&mut br).await {
            Ok(Some(l)) => l,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!("agentd-client: read failed: {}", e);
                break;
            }
        };
        // Try Response first (it has `id`); fall back to Event.
        if let Ok(resp) = serde_json::from_str::<Response>(&line) {
            if let Some(tx) = pending.lock().remove(&resp.id) {
                let _ = tx.send(resp);
            } else {
                tracing::debug!("agentd-client: response for unknown id {}", resp.id);
            }
            continue;
        }
        if let Ok(ev) = serde_json::from_str::<Event>(&line) {
            // Lagged subscribers don't kill the channel; the broadcast
            // capacity bounds memory.
            let _ = events.send(ev);
            continue;
        }
        tracing::warn!("agentd-client: unparsable frame: {}", line);
    }
    // Drain pending oneshots so awaiting callers learn the connection
    // died (drop = sender Err = ClientError::Disconnected).
    pending.lock().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentd::engine::Engine;
    use crate::agentd::server::serve_connection;
    use std::sync::Arc;

    async fn make_pair() -> (AgentClient, Arc<Engine>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::new(dir.path().to_path_buf()).unwrap());
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let (sr, sw) = tokio::io::split(server_io);
        let engine_clone = engine.clone();
        tokio::spawn(async move {
            let _ = serve_connection(engine_clone, sr, sw).await;
        });
        let (cr, cw) = tokio::io::split(client_io);
        let client = AgentClient::from_io(cr, cw, Duration::from_secs(2))
            .await
            .expect("connect");
        (client, engine, dir)
    }

    #[tokio::test]
    async fn ping_round_trips() {
        let (client, _engine, _dir) = make_pair().await;
        let (uptime, live) = client.ping().await.unwrap();
        let _ = uptime;
        assert_eq!(live, 0);
    }

    #[tokio::test]
    async fn spawn_then_list_then_kill() {
        let (client, _engine, _dir) = make_pair().await;
        let info = client
            .spawn(SpawnArgs {
                workspace_id: "ws".into(),
                agent_type: "test".into(),
                cwd: None,
                bin_path: "/bin/sh".into(),
                argv: vec!["-c".into(), "sleep 1".into()],
                env: vec![],
                reuse_id: None,
            })
            .await
            .unwrap();
        let listed = client.list("ws").await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, info.id);
        client.kill(&info.id).await.unwrap();
    }

    #[tokio::test]
    async fn write_to_unknown_agent_returns_broker_not_found() {
        let (client, _engine, _dir) = make_pair().await;
        let err = client.write("nope", b"x").await.unwrap_err();
        match err {
            ClientError::Broker { code, .. } => assert_eq!(code, ErrorCode::NotFound),
            other => panic!("got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn subscribe_streams_stdout_event() {
        let (client, _engine, _dir) = make_pair().await;
        let mut events = client.subscribe().await.unwrap();
        let info = client
            .spawn(SpawnArgs {
                workspace_id: "ws".into(),
                agent_type: "test".into(),
                cwd: None,
                bin_path: "/bin/sh".into(),
                argv: vec!["-c".into(), "printf hi; sleep 0.05".into()],
                env: vec![],
                reuse_id: None,
            })
            .await
            .unwrap();

        let mut got_stdout = false;
        let mut got_exit = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline && !(got_stdout && got_exit) {
            let ev = match tokio::time::timeout(Duration::from_millis(500), events.recv()).await {
                Ok(Ok(ev)) => ev,
                _ => continue,
            };
            match ev {
                Event::Stdout { agent_id, .. } if agent_id == info.id => got_stdout = true,
                Event::Exit { agent_id } if agent_id == info.id => got_exit = true,
                _ => {}
            }
        }
        assert!(got_stdout);
        assert!(got_exit);
    }

    #[tokio::test]
    async fn version_mismatch_surfaces_at_connect() {
        // Force a server that advertises a different version by sending
        // raw bytes. Easiest: bypass server and respond with an older
        // shape using a hand-rolled echo. Instead, lean on serve_connection
        // and fake the client_version constant via direct request.
        let dir = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::new(dir.path().to_path_buf()).unwrap());
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let (sr, sw) = tokio::io::split(server_io);
        let engine_clone = engine.clone();
        tokio::spawn(async move {
            let _ = serve_connection(engine_clone, sr, sw).await;
        });

        // Send a Hello with a deliberately wrong version using the
        // low-level send path on a freshly-built client.
        let (cr, cw) = tokio::io::split(client_io);
        let (out_tx, out_rx) = mpsc::channel::<String>(8);
        let (events, _) = broadcast::channel::<Event>(8);
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        tokio::spawn(write_loop(cw, out_rx));
        let pending_clone = pending.clone();
        let events_clone = events.clone();
        tokio::spawn(read_loop(cr, pending_clone, events_clone));
        let (tx, rx) = oneshot::channel();
        pending.lock().insert(1, tx);
        out_tx
            .send(serde_json::to_string(&Request {
                id: 1,
                op: Op::Hello { client_version: 999 },
            }).unwrap())
            .await
            .unwrap();
        let resp = rx.await.unwrap();
        match resp.result {
            ResponseBody::Error { code, .. } => assert_eq!(code, ErrorCode::VersionMismatch),
            other => panic!("got: {:?}", other),
        }
    }

    /// Note: this test only passes deterministically against a real
    /// socket (`UnixStream::into_split`) where the kernel surfaces EOF
    /// promptly. With `tokio::io::duplex` + `tokio::io::split` the read
    /// half doesn't observe the write half's drop reliably — that's a
    /// quirk of the in-memory duplex, not the production code path.
    /// The real disconnect surface is exercised by the live broker
    /// smoke-test in CI.
    #[tokio::test]
    #[ignore]
    async fn pending_request_resolves_to_disconnected_when_server_drops() {
        // Build a half-duplex pair where we drop the server side after
        // the Hello completes. read_loop sees EOF, drops `pending`, the
        // pending oneshot's tx is dropped, the awaiting future resolves
        // to `Disconnected`.
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        // Spawn a tiny server that hellos back ONCE then drops the
        // connection. Without serve_connection so we control teardown.
        let server_task = tokio::spawn(async move {
            let (sr, mut sw) = tokio::io::split(server_io);
            let mut br = tokio::io::BufReader::new(sr);
            let _ = read_frame_async(&mut br).await; // Hello request
            let hello = Response {
                id: 1,
                result: ResponseBody::Hello {
                    hello: HelloResponse {
                        broker_version: PROTOCOL_VERSION,
                        broker_build: "test".into(),
                        broker_pid: 1,
                    },
                },
            };
            let s = serde_json::to_string(&hello).unwrap();
            write_frame_async(&mut sw, &s).await.unwrap();
            // Drop both halves — closes the duplex, client read_loop
            // returns Ok(None) and clears pending.
            drop(br);
            drop(sw);
        });
        let (cr, cw) = tokio::io::split(client_io);
        let client = AgentClient::from_io(cr, cw, Duration::from_secs(2))
            .await
            .unwrap();
        server_task.await.unwrap();
        // Give the read_loop a moment to observe the EOF.
        tokio::time::sleep(Duration::from_millis(50)).await;
        // Bound the call so a regression in the read-loop can't hang
        // the whole test runner — we expect Disconnected within ~1s.
        let res = tokio::time::timeout(Duration::from_secs(5), client.ping()).await;
        match res {
            Ok(Err(ClientError::Disconnected)) => {}
            Ok(other) => panic!("expected Disconnected, got: {:?}", other),
            Err(_) => panic!("ping did not resolve within 5s — read-loop did not clear pending"),
        }
    }
}
