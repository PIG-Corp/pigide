//! `AgentManager` — broker-driven facade.
//!
//! This is the v2 implementation. It keeps the v1 public API
//! (`spawn`/`write`/`kill`/`list`/etc.) so the 80+ call sites in
//! commands.rs / orchestrator / swarm / watcher / architect compile
//! unchanged, but internally it proxies every operation to a long-lived
//! `pigide-agentd` broker process via [`crate::agentd::client::AgentClient`].
//!
//! Why: the v1 implementation owned the PTYs in-process. When PigIDE
//! quit (Cmd+Q) or crashed, the agents went with it. The v2 manager
//! lets the broker outlive PigIDE — Cmd+Q closes the UI but keeps every
//! agent's conversation state intact, so reopening PigIDE reattaches to
//! the same `claude` / `kiro-cli` processes with their full chat history.
//!
//! Key behaviour differences from v1:
//!
//!   - `kill_all()` is now **no-op**. Quitting PigIDE must NOT kill the
//!     agents — that's the whole point of the broker. Old call sites
//!     stay (`RunEvent::ExitRequested` etc.) but they no-op.
//!   - `restore_session()` is now **reattach**, not respawn. We ask the
//!     broker for `list_all`, ensure the SQLite mirror is in sync, and
//!     emit `agent.spawned` events so the frontend can mount tiles for
//!     each surviving agent.
//!   - `read_log_tail()` now reads via the broker (which owns the log
//!     file). The path is the same as before so existing log files
//!     migrate without copying.
//!   - `write()` retains the readiness-gate semantics — implemented
//!     broker-side now, but the wait timeout still comes from
//!     `readiness.timeout_ms`.
//!
//! Sync façade over async client: every method drives the async
//! `AgentClient` through [`block_on_safely`], which picks the right
//! strategy based on the calling context:
//!
//!   - On a tokio worker thread (Tauri commands run here): uses
//!     `tokio::task::block_in_place` + `Handle::block_on`. Re-entering
//!     the same multi-threaded runtime via plain `block_on` panics with
//!     "Cannot start a runtime from within a runtime" on tokio 1.x —
//!     `block_in_place` is the supported escape hatch.
//!   - Off-runtime sync threads (watcher, architect supervisor): just
//!     uses `Handle::block_on` against the captured tauri runtime
//!     handle, no `block_in_place` needed.

use crate::agentd::client::{AgentClient, ClientError, EventReceiver};
use crate::agentd::proto::{default_socket_path, AgentInfo as ProtoAgent, ErrorCode, Event};
use crate::agentd::resolve;
use crate::agentd::supervisor::{connect_or_spawn, SupervisorError};
use crate::db::DbPool;
use crate::error::{Error, Result};
use crate::events::{EV_AGENT_EXIT, EV_AGENT_SPAWNED, EV_AGENT_STDOUT};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex as AsyncMutex;

/// Drive an async future to completion from a sync method, no matter
/// whether the caller is on a tokio worker (Tauri commands) or a plain
/// OS thread (watcher / architect supervisor). Naïvely calling
/// `tauri::async_runtime::block_on` from a tokio worker panics with
/// "Cannot start a runtime from within a runtime" on tokio 1.x, which
/// is the entire bug class this helper exists to avoid.
fn block_on_safely<F: std::future::Future>(fut: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(h) => tokio::task::block_in_place(|| h.block_on(fut)),
        Err(_) => tauri::async_runtime::block_on(fut),
    }
}

/// Default readiness wait, mirrored from v1. The broker enforces this
/// per-write — currently hard-coded broker-side. Kept here for future
/// reintroduction of the `readiness.timeout_ms` setting once the broker
/// gains a per-spawn override.
#[allow(dead_code)]
const DEFAULT_READINESS_TIMEOUT_MS: u64 = 1500;

/// How long to wait for the broker to come up on first connect.
const BROKER_BOOT_TIMEOUT: Duration = Duration::from_secs(5);

/// Mirror of `crate::agentd::proto::AgentInfo` shaped for
/// backward-compat with v1's serde wire format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub workspace_id: String,
    pub agent_type: String,
    pub cwd: Option<String>,
    pub status: String,
    pub created_at: String,
}

impl From<ProtoAgent> for Agent {
    fn from(a: ProtoAgent) -> Self {
        Self {
            id: a.id,
            workspace_id: a.workspace_id,
            agent_type: a.agent_type,
            cwd: a.cwd,
            status: a.status,
            created_at: a.created_at,
        }
    }
}

/// Same enum as v1 — call sites import this directly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AgentType {
    KiroCli,
    Claude,
    Aider,
    Goose,
    #[serde(rename = "opencode")]
    OpenCode,
    Devin,
    Agy,
    Codex,
    Ssh,
}

impl AgentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentType::KiroCli => "kiro-cli",
            AgentType::Claude => "claude",
            AgentType::Aider => "aider",
            AgentType::Goose => "goose",
            AgentType::OpenCode => "opencode",
            AgentType::Devin => "devin",
            AgentType::Agy => "agy",
            AgentType::Codex => "codex",
            AgentType::Ssh => "ssh",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "kiro-cli" | "kiro" => Some(AgentType::KiroCli),
            "claude" | "claude-code" => Some(AgentType::Claude),
            "aider" => Some(AgentType::Aider),
            "goose" => Some(AgentType::Goose),
            "opencode" | "oc" => Some(AgentType::OpenCode),
            "devin" | "devin-cli" => Some(AgentType::Devin),
            "agy" | "antigravity" => Some(AgentType::Agy),
            "codex" | "openai-codex" => Some(AgentType::Codex),
            "ssh" => Some(AgentType::Ssh),
            _ => None,
        }
    }
}

/// Manager state populated lazily on first method call.
struct ConnectedState {
    client: AgentClient,
    /// Last-stdout cache, populated by the event-pump task. The broker
    /// can answer this via Op::LastStdoutAge, but PigIDE callers
    /// (orchestrator wait_for_agent_idle) want a sync answer; caching
    /// in-process is the cheapest way.
    last_stdout: Arc<Mutex<HashMap<String, Instant>>>,
}

pub struct AgentManager {
    db: DbPool,
    connected: Mutex<Option<ConnectedState>>,
    app: Mutex<Option<AppHandle>>,
    mcp: Mutex<Option<Arc<crate::mcp::server::McpServerHandle>>>,
    socket_path: Mutex<PathBuf>,
    /// Serialises the first-connect path so concurrent callers all wait
    /// on the *same* in-flight connect rather than racing or returning
    /// a misleading "contention" error. Once `connected` is populated,
    /// `ensure_connected` returns without ever taking this lock.
    connect_lock: AsyncMutex<()>,
    /// Marked true once the event-pump task has been spawned. We don't
    /// re-spawn it on reconnect (broker death + revive is a future
    /// concern; for now a hard failure ends the session).
    pump_started: AtomicBool,
}

impl AgentManager {
    pub fn new(db: DbPool) -> Self {
        Self {
            db,
            connected: Mutex::new(None),
            app: Mutex::new(None),
            mcp: Mutex::new(None),
            socket_path: Mutex::new(default_socket_path()),
            connect_lock: AsyncMutex::new(()),
            pump_started: AtomicBool::new(false),
        }
    }

    pub fn set_app_handle(&self, app: AppHandle) {
        *self.app.lock() = Some(app);
    }

    /// Override the broker socket path. Tests use this to avoid the
    /// default `$XDG_RUNTIME_DIR/pigide/agentd.sock`. Production code
    /// should leave it at the default.
    pub fn set_socket_path(&self, path: PathBuf) {
        *self.socket_path.lock() = path;
    }

    pub fn set_mcp_handle(&self, handle: Arc<crate::mcp::server::McpServerHandle>) {
        *self.mcp.lock() = Some(handle);
    }

    /// Cached "how long ago did this agent emit anything" answer.
    /// Populated by the event-pump task in [`AgentManager::start_event_pump`].
    pub fn last_stdout_age(&self, agent_id: &str) -> Option<Duration> {
        let state = self.connected.lock();
        let s = state.as_ref()?;
        let result = s.last_stdout.lock().get(agent_id).map(|t| t.elapsed());
        result
    }

    /// V1 compat: marks all DB rows as exited. The broker holds the
    /// real truth; the SQLite mirror is just denormalised metadata that
    /// gets re-synced on `restore_session`. Called once on PigIDE boot
    /// before `restore_session`, exactly as in v1.
    pub fn reset_statuses(&self) -> Result<()> {
        let conn = self.db.get()?;
        conn.execute("UPDATE agents SET status='exited'", [])?;
        Ok(())
    }

    /// Per-agent log path, owned by the broker. PigIDE reads this file
    /// directly (we don't go through the broker) — it's just a tail of
    /// stdout, and direct fs access is cheaper than an RPC round-trip
    /// for the 64 KiB scrollback replay on tile mount.
    pub fn read_log_tail(&self, agent_id: &str, max_bytes: usize) -> Result<Vec<u8>> {
        let path = log_dir().join(format!("{}.log", agent_id));
        if !path.exists() {
            return Ok(Vec::new());
        }
        let meta = std::fs::metadata(&path)?;
        let size = meta.len() as usize;
        let start = size.saturating_sub(max_bytes);
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(&path)?;
        f.seek(SeekFrom::Start(start as u64))?;
        let mut buf = Vec::with_capacity(size - start);
        f.read_to_end(&mut buf)?;
        Ok(buf)
    }

    /// V1 compat. Same query, returned shape unchanged. The broker
    /// doesn't care; this is purely the SQLite mirror.
    pub fn list_persisted_running(&self) -> Result<Vec<Agent>> {
        let conn = self.db.get()?;
        let mut stmt = conn.prepare(
            "SELECT id,workspace_id,type,cwd,status,created_at
             FROM agents WHERE status='running' ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Agent {
                id: r.get(0)?,
                workspace_id: r.get(1)?,
                agent_type: r.get(2)?,
                cwd: r.get(3)?,
                status: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// V1 compat. Used to respawn an agent under a fixed id (for
    /// session restore). In v2 this is implemented as
    /// `client.spawn(reuse_id=Some(id))`: if the broker already holds
    /// that id, the call is idempotent and returns the existing agent;
    /// otherwise the broker spawns fresh under that id.
    pub fn respawn_persisted(self: &Arc<Self>, agent_id: &str) -> Result<bool> {
        let conn = self.db.get()?;
        let mut stmt = conn.prepare(
            "SELECT id,workspace_id,type,cwd FROM agents WHERE id=?1",
        )?;
        let mut rows = stmt.query([agent_id])?;
        let row = match rows.next()? {
            Some(r) => r,
            None => return Ok(false),
        };
        let id: String = row.get(0)?;
        let workspace_id: String = row.get(1)?;
        let type_str: String = row.get(2)?;
        let cwd: Option<String> = row.get(3)?;
        drop(rows);
        drop(stmt);
        drop(conn);

        let agent_type = AgentType::parse(&type_str)
            .ok_or_else(|| Error::Invalid(format!("unknown agent_type {}", type_str)))?;
        self.spawn_internal(&workspace_id, agent_type, cwd, Some(id), None)?;
        Ok(true)
    }

    /// Reattach to broker-owned agents. Steps:
    ///
    ///   1. Connect to (or auto-spawn) the broker.
    ///   2. Ask `list_all` — these are the agents that survived the
    ///      previous PigIDE process (or were started by another
    ///      PigIDE on the same machine).
    ///   3. UPSERT each into the SQLite mirror with `status='running'`.
    ///   4. Mark any DB rows the broker DOESN'T know about as `exited`
    ///      (broker is the source of truth).
    ///   5. Emit `agent.spawned` so the frontend mounts tiles.
    ///
    /// Returns `(restored, failed)` for v1 compat. `failed` is always 0
    /// here — there's no per-agent respawn that can fail.
    pub fn restore_session(self: &Arc<Self>) -> Result<(usize, usize)> {
        self.ensure_connected()?;

        let live = {
            let state = self.connected.lock();
            let client = state.as_ref().expect("connected").client.clone();
            block_on_safely(async move { client.list_all().await })
                .map_err(client_to_error)?
        };

        let conn = self.db.get()?;
        let live_ids: Vec<&str> = live.iter().map(|a| a.id.as_str()).collect();
        // Mark anything the broker doesn't know about as exited.
        // Use a single UPDATE with NOT IN (...). For empty sets we just
        // mark everything 'running' as exited.
        if live_ids.is_empty() {
            conn.execute(
                "UPDATE agents SET status='exited' WHERE status='running'",
                [],
            )?;
        } else {
            let placeholders = std::iter::repeat("?")
                .take(live_ids.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "UPDATE agents SET status='exited'
                 WHERE status='running' AND id NOT IN ({})",
                placeholders
            );
            let params: Vec<&dyn rusqlite::ToSql> = live_ids
                .iter()
                .map(|s| s as &dyn rusqlite::ToSql)
                .collect();
            conn.execute(&sql, &*params)?;
        }
        // UPSERT broker's view of every live agent.
        for a in &live {
            conn.execute(
                "INSERT INTO agents(id,workspace_id,type,cwd,status,created_at)
                 VALUES(?1,?2,?3,?4,'running',?5)
                 ON CONFLICT(id) DO UPDATE SET
                    workspace_id=excluded.workspace_id,
                    type=excluded.type,
                    cwd=excluded.cwd,
                    status='running'",
                rusqlite::params![&a.id, &a.workspace_id, &a.agent_type, &a.cwd, &a.created_at],
            )?;
        }
        drop(conn);

        // Tell the frontend.
        if let Some(app) = self.app.lock().as_ref() {
            for a in &live {
                let agent: Agent = a.clone().into();
                let _ = app.emit(EV_AGENT_SPAWNED, &agent);
            }
        }
        Ok((live.len(), 0))
    }

    pub fn spawn(
        self: &Arc<Self>,
        workspace_id: &str,
        agent_type: AgentType,
        cwd: Option<String>,
    ) -> Result<Agent> {
        self.spawn_internal(workspace_id, agent_type, cwd, None, None)
    }

    pub fn spawn_with_args(
        self: &Arc<Self>,
        workspace_id: &str,
        agent_type: AgentType,
        cwd: Option<String>,
        args: Vec<String>,
    ) -> Result<Agent> {
        self.spawn_internal(workspace_id, agent_type, cwd, None, Some(args))
    }

    pub(crate) fn spawn_internal(
        self: &Arc<Self>,
        workspace_id: &str,
        agent_type: AgentType,
        cwd: Option<String>,
        reuse_id: Option<String>,
        args_override: Option<Vec<String>>,
    ) -> Result<Agent> {
        self.ensure_connected()?;

        let mcp = self.mcp.lock().clone();
        let mcp_ref = mcp.as_ref();
        let args = resolve::resolve_spawn(
            &self.db,
            workspace_id,
            agent_type.clone(),
            cwd.clone(),
            args_override,
            reuse_id,
            mcp_ref,
        );

        let client = {
            let state = self.connected.lock();
            state.as_ref().expect("connected").client.clone()
        };
        let info = block_on_safely(async move { client.spawn(args).await })
            .map_err(client_to_error)?;

        // UPSERT mirror.
        let conn = self.db.get()?;
        let created_at = info.created_at.clone();
        conn.execute(
            "INSERT INTO agents(id,workspace_id,type,cwd,status,created_at)
             VALUES(?1,?2,?3,?4,'running',?5)
             ON CONFLICT(id) DO UPDATE SET
                workspace_id=excluded.workspace_id,
                type=excluded.type,
                cwd=excluded.cwd,
                status='running'",
            rusqlite::params![
                &info.id,
                &info.workspace_id,
                &info.agent_type,
                &info.cwd,
                &created_at
            ],
        )?;
        drop(conn);

        let agent: Agent = info.into();
        if let Some(app) = self.app.lock().as_ref() {
            let _ = app.emit(EV_AGENT_SPAWNED, &agent);
        }
        Ok(agent)
    }

    pub fn write(&self, agent_id: &str, data: &[u8]) -> Result<usize> {
        let client = self.client_or_err()?;
        let bytes = data.to_vec();
        let id = agent_id.to_string();
        let n = block_on_safely(async move { client.write(&id, &bytes).await })
            .map_err(client_to_error)?;
        // Reset quiet-timer locally so wait_for_agent_idle sees the
        // write. The broker also resets it on its side; we keep both
        // in sync to avoid a stale read.
        let conn = self.connected.lock();
        if let Some(state) = conn.as_ref() {
            state
                .last_stdout
                .lock()
                .insert(agent_id.into(), Instant::now());
        }
        Ok(n)
    }

    pub fn resize(&self, agent_id: &str, cols: u16, rows: u16) -> Result<()> {
        let client = self.client_or_err()?;
        let id = agent_id.to_string();
        block_on_safely(async move { client.resize(&id, cols, rows).await })
            .map_err(client_to_error)?;
        Ok(())
    }

    pub fn kill(&self, agent_id: &str) -> Result<()> {
        let client = self.client_or_err()?;
        let id = agent_id.to_string();
        block_on_safely(async move { client.kill(&id).await })
            .map_err(client_to_error)?;
        let conn = self.db.get()?;
        conn.execute("UPDATE agents SET status='exited' WHERE id=?1", [agent_id])?;
        Ok(())
    }

    pub fn list(&self, workspace_id: &str) -> Result<Vec<Agent>> {
        // Source of truth for "what's actually running" = broker.
        // Source of truth for "what we know about" = SQLite mirror.
        // For UI list we want the broker's view (a stale row in the
        // mirror would lie about a dead agent), so always go through
        // the client when connected. If broker is unreachable,
        // fall back to the mirror so the UI still has *something*.
        match self.client_or_err() {
            Ok(client) => {
                let ws = workspace_id.to_string();
                let live = block_on_safely(async move { client.list(&ws).await })
                    .map_err(client_to_error)?;
                Ok(live.into_iter().map(Agent::from).collect())
            }
            Err(_) => {
                let conn = self.db.get()?;
                let mut stmt = conn.prepare(
                    "SELECT id,workspace_id,type,cwd,status,created_at
                     FROM agents WHERE workspace_id=?1 ORDER BY created_at ASC",
                )?;
                let rows = stmt.query_map([workspace_id], |r| {
                    Ok(Agent {
                        id: r.get(0)?,
                        workspace_id: r.get(1)?,
                        agent_type: r.get(2)?,
                        cwd: r.get(3)?,
                        status: r.get(4)?,
                        created_at: r.get(5)?,
                    })
                })?;
                let mut out = Vec::new();
                for row in rows {
                    out.push(row?);
                }
                Ok(out)
            }
        }
    }

    /// In v1 this killed every PTY on Cmd+Q. In v2 the broker holds
    /// the PTYs precisely so they survive Cmd+Q — so this is a no-op.
    /// Old call sites (`RunEvent::ExitRequested`, `WindowEvent::Destroyed`)
    /// keep calling it; we just log.
    pub fn kill_all(&self) {
        tracing::info!(
            "kill_all: no-op (broker holds the PTYs; agents survive PigIDE quit)"
        );
    }

    // --- internals ---

    fn client_or_err(&self) -> Result<AgentClient> {
        self.ensure_connected()?;
        let state = self.connected.lock();
        Ok(state.as_ref().expect("connected").client.clone())
    }

    /// First-call boot: connects (or auto-spawns) the broker, then
    /// starts the event-pump that fans broker events out as Tauri
    /// events. Idempotent and safe under concurrent calls — all
    /// callers wait on the same in-flight connect rather than racing.
    fn ensure_connected(&self) -> Result<()> {
        // Fast path: already connected.
        if self.connected.lock().is_some() {
            return Ok(());
        }

        let socket_path = self.socket_path.lock().clone();

        // Serialise the connect itself. The async mutex guarantees that
        // concurrent first-call attempts queue up behind one connect
        // rather than each spawning their own broker. Once the leader
        // populates `connected`, every other waiter sees it in the
        // double-check below and returns immediately.
        block_on_safely(async move {
            let _guard = self.connect_lock.lock().await;

            // Double-check: another caller may have completed while
            // we were queued on the lock.
            if self.connected.lock().is_some() {
                return Ok(());
            }

            let (client, _spawned) = connect_or_spawn(socket_path, BROKER_BOOT_TIMEOUT)
                .await
                .map_err(supervisor_to_error)?;

            // Subscribe BEFORE we hand the client to the manager state,
            // so the event-pump task is wired up first.
            let events = client.subscribe().await.map_err(client_to_error)?;
            let last_stdout = Arc::new(Mutex::new(HashMap::new()));
            self.start_event_pump(events, last_stdout.clone());

            *self.connected.lock() = Some(ConnectedState {
                client,
                last_stdout,
            });
            Ok(())
        })
    }

    /// Spawn the long-running task that consumes broker events and
    /// re-emits them as Tauri `EV_AGENT_STDOUT` / `EV_AGENT_EXIT`
    /// events. Also keeps the per-agent `last_stdout` cache fresh.
    fn start_event_pump(
        &self,
        mut events: EventReceiver,
        last_stdout: Arc<Mutex<HashMap<String, Instant>>>,
    ) {
        if self
            .pump_started
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        let app = self.app.lock().clone();
        let db = self.db.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(Event::Stdout { agent_id, data_b64 }) => {
                        last_stdout.lock().insert(agent_id.clone(), Instant::now());
                        if let Some(app) = &app {
                            let _ = app.emit(
                                EV_AGENT_STDOUT,
                                serde_json::json!({
                                    "agent_id": agent_id,
                                    "data_b64": data_b64,
                                }),
                            );
                        }
                    }
                    Ok(Event::Exit { agent_id }) => {
                        if let Ok(c) = db.get() {
                            let _ = c.execute(
                                "UPDATE agents SET status='exited' WHERE id=?1",
                                [&agent_id],
                            );
                        }
                        if let Some(app) = &app {
                            let _ = app.emit(
                                EV_AGENT_EXIT,
                                serde_json::json!({ "agent_id": agent_id }),
                            );
                        }
                    }
                    Ok(Event::BrokerShutdown { reason }) => {
                        tracing::warn!("broker shutdown: {}", reason);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            "agent event pump lagged by {} events; UI scrollback rebuilds from log",
                            n
                        );
                        continue;
                    }
                    Err(_) => break,
                }
            }
        });
    }
}

/// Path to the broker's per-agent stdout log directory. Must match
/// `bin/pigide-agentd.rs::default_log_dir` so we read what the broker
/// writes.
fn log_dir() -> PathBuf {
    if let Ok(d) = std::env::var("PIGIDE_AGENTD_LOG_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("pigide")
        .join("agents")
}

fn client_to_error(e: ClientError) -> Error {
    match e {
        ClientError::Broker { code, message } => match code {
            ErrorCode::NotFound => Error::NotFound(message),
            ErrorCode::Invalid => Error::Invalid(message),
            _ => Error::Agent(message),
        },
        other => Error::Agent(other.to_string()),
    }
}

fn supervisor_to_error(e: SupervisorError) -> Error {
    Error::Agent(format!("broker: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_type_round_trip() {
        let cases = [
            ("kiro-cli", AgentType::KiroCli),
            ("claude", AgentType::Claude),
            ("aider", AgentType::Aider),
            ("goose", AgentType::Goose),
            ("opencode", AgentType::OpenCode),
            ("devin", AgentType::Devin),
            ("agy", AgentType::Agy),
            ("codex", AgentType::Codex),
            ("ssh", AgentType::Ssh),
        ];
        for (s, t) in &cases {
            assert_eq!(AgentType::parse(s), Some(t.clone()));
            assert_eq!(t.as_str(), *s);
        }
    }

    #[test]
    fn agent_type_aliases() {
        assert_eq!(AgentType::parse("kiro"), Some(AgentType::KiroCli));
        assert_eq!(AgentType::parse("claude-code"), Some(AgentType::Claude));
        assert_eq!(AgentType::parse("oc"), Some(AgentType::OpenCode));
        assert_eq!(AgentType::parse("devin-cli"), Some(AgentType::Devin));
        assert_eq!(AgentType::parse("antigravity"), Some(AgentType::Agy));
        assert_eq!(AgentType::parse("openai-codex"), Some(AgentType::Codex));
        assert_eq!(AgentType::parse("unknown"), None);
    }

    fn test_pool() -> DbPool {
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(2).build(manager).unwrap();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE workspaces (id TEXT PRIMARY KEY);
             CREATE TABLE agents (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL,
                type TEXT NOT NULL,
                cwd TEXT,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                role TEXT NOT NULL DEFAULT 'builder');
             CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO workspaces(id) VALUES('w1');",
        )
        .unwrap();
        pool
    }

    #[test]
    fn read_log_tail_returns_empty_when_missing() {
        let p = test_pool();
        let mgr = AgentManager::new(p);
        let bytes = mgr.read_log_tail("does-not-exist", 1024).unwrap();
        assert!(bytes.is_empty());
    }

    #[test]
    fn list_persisted_running_only_picks_running_rows() {
        let p = test_pool();
        let conn = p.get().unwrap();
        conn.execute(
            "INSERT INTO agents(id,workspace_id,type,cwd,status,created_at)
             VALUES('a','w1','claude',NULL,'running','2026-05-22T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agents(id,workspace_id,type,cwd,status,created_at)
             VALUES('b','w1','claude',NULL,'exited','2026-05-22T00:00:01Z')",
            [],
        )
        .unwrap();
        let mgr = AgentManager::new(p);
        let live = mgr.list_persisted_running().unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].id, "a");
    }

    #[test]
    fn reset_statuses_marks_every_row_exited() {
        let p = test_pool();
        let conn = p.get().unwrap();
        conn.execute(
            "INSERT INTO agents(id,workspace_id,type,cwd,status,created_at)
             VALUES('a','w1','claude',NULL,'running','2026-05-22T00:00:00Z')",
            [],
        )
        .unwrap();
        let mgr = AgentManager::new(p.clone());
        mgr.reset_statuses().unwrap();
        let s: String = conn
            .query_row("SELECT status FROM agents WHERE id='a'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(s, "exited");
    }

    #[test]
    fn kill_all_is_noop() {
        let p = test_pool();
        let conn = p.get().unwrap();
        conn.execute(
            "INSERT INTO agents(id,workspace_id,type,cwd,status,created_at)
             VALUES('a','w1','claude',NULL,'running','2026-05-22T00:00:00Z')",
            [],
        )
        .unwrap();
        let mgr = AgentManager::new(p.clone());
        mgr.kill_all();
        // Row still says running — that's the whole point.
        let s: String = conn
            .query_row("SELECT status FROM agents WHERE id='a'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(s, "running");
    }
}

// Suppress unused imports during the transitional period — Utc, Engine,
// and the brand new `std::sync::Mutex` from parking_lot share names with
// std versions in some call sites; tooling is still picking up the v1
// imports.
