use crate::db::DbPool;
use crate::error::{Error, Result};
use crate::events::{EV_AGENT_EXIT, EV_AGENT_SPAWNED, EV_AGENT_STDOUT};
use base64::Engine;
use chrono::Utc;
use parking_lot::{Condvar, Mutex};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

/// How long `write()` will block waiting for the agent to become "ready"
/// (i.e. the PTY has produced its first stdout chunk). After this, we send
/// anyway — better to deliver to a slow CLI than to fail the orchestrator
/// turn. Configurable per-agent via setting `readiness.timeout_ms`.
const DEFAULT_READINESS_TIMEOUT_MS: u64 = 1500;

/// Resolve the per-agent log file under XDG_DATA_HOME/pigide/agents/.
fn agent_log_path(agent_id: &str) -> Option<PathBuf> {
    let base = dirs::data_local_dir()?;
    let dir = base.join("pigide").join("agents");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(format!("{}.log", agent_id)))
}

/// Locate `wsl.exe` on Windows. Tries `%SystemRoot%\System32\wsl.exe` (the
/// canonical install path; visible even when PATH was clobbered by a parent
/// launcher), then falls back to a PATH lookup.
#[cfg(target_os = "windows")]
fn resolve_wsl_exe() -> Option<String> {
    if let Ok(root) = std::env::var("SystemRoot") {
        let p = std::path::PathBuf::from(root).join("System32").join("wsl.exe");
        if p.exists() {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let p = dir.join("wsl.exe");
            if p.exists() {
                return Some(p.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// Decide whether `agent_type` should be launched inside WSL on Windows.
/// Driven by the per-agent setting `wsl.<type>=true`. On non-Windows the
/// answer is always None.
fn wsl_config(
    db: &crate::db::DbPool,
    agent_type: &AgentType,
) -> Option<WslConfig> {
    #[cfg(target_os = "windows")]
    {
        let on = crate::db::get_setting(db, &format!("wsl.{}", agent_type.as_str()))
            .ok()
            .flatten()
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if !on {
            return None;
        }
        let exe = resolve_wsl_exe()?;
        let distro = crate::db::get_setting(db, "wsl.distro")
            .ok()
            .flatten()
            .filter(|s| !s.trim().is_empty());
        Some(WslConfig { exe, distro })
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (db, agent_type);
        None
    }
}

#[allow(dead_code)]
struct WslConfig {
    exe: String,
    distro: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AgentType {
    KiroCli,
    Claude,
    Aider,
    Goose,
    #[serde(rename = "opencode")]
    OpenCode,
    /// Devin for Terminal — Cognition's local coding agent. The published
    /// install script (https://cli.devin.ai/install.sh) symlinks the binary
    /// to `~/.local/bin/devin`; running `devin` with no args launches the
    /// interactive TUI, mirroring the `claude` / `opencode` pattern.
    Devin,
    /// SSH session — spawned via the system `ssh` binary with arguments
    /// supplied per-spawn (typically from an `ssh_presets` row).
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
            "ssh" => Some(AgentType::Ssh),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub workspace_id: String,
    pub agent_type: String,
    pub cwd: Option<String>,
    pub status: String,
    pub created_at: String,
}

/// Per-agent runtime handle.
///
/// `writer` lives behind its own `Arc<Mutex<...>>` so concurrent
/// `send_to_agent` calls to **different** agents don't fight over a single
/// global lock, while concurrent calls to the **same** agent are serialised
/// (preventing interleaved bytes mid-prompt).
///
/// `readiness` is set to `true` by the reader thread on the first non-empty
/// stdout chunk (or by `write()` itself once the grace period has elapsed).
/// `write()` waits on this before pushing bytes — fixes the race where a
/// prompt sent immediately after `spawn_agent` is consumed by the CLI's
/// startup banner instead of its input loop.
struct AgentRuntime {
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    readiness: Arc<(Mutex<bool>, Condvar)>,
    spawned_at: Instant,
    cols: u16,
    rows: u16,
}

pub struct AgentManager {
    db: DbPool,
    handles: Arc<Mutex<HashMap<String, AgentRuntime>>>,
    /// Last time each agent emitted any stdout. Used by `wait_for_agent_idle`
    /// (orchestrator tool) to detect when the agent is ready for the next
    /// prompt. Wrapped in Arc so reader-threads can update it after the
    /// `AgentManager` Mutex has been released.
    last_stdout: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    app: Mutex<Option<AppHandle>>,
    /// Set after boot. When present and the server is running, Claude tiles
    /// are launched with `--mcp-config <json>` so they auto-register the
    /// `pigide` MCP server without touching the user's `~/.claude.json`.
    mcp: Mutex<Option<Arc<crate::mcp::server::McpServerHandle>>>,
}

/// Pure write path — extracted so it can be unit-tested without spawning a
/// real PTY. `is_dead()` is the liveness check (in production: child
/// `try_wait` + handles-map lookup; in tests: a flag on a shared bool).
///
/// Order of operations is the contract:
///   1. Liveness check → fail fast on stale handle.
///   2. Bounded readiness wait (Condvar) — capped by spawn-relative grace.
///   3. Liveness re-check (agent may have died during the wait).
///   4. Acquire per-writer lock → `write_all` + `flush`.
///
/// Returns bytes actually written. Because `write_all` retries short
/// writes internally, on success this always equals `data.len()`.
fn write_runtime<F: Fn() -> bool>(
    writer: &Arc<Mutex<Box<dyn Write + Send>>>,
    readiness: &Arc<(Mutex<bool>, Condvar)>,
    spawned_at: Instant,
    readiness_timeout: Duration,
    data: &[u8],
    agent_id: &str,
    is_dead: F,
) -> Result<usize> {
    if is_dead() {
        return Err(Error::NotFound(format!("agent {} not running", agent_id)));
    }

    // Wait for the agent's first stdout chunk (= "I'm in input-mode")
    // OR for the grace period to elapse, whichever comes first. The
    // grace is *relative to spawn*, not to call-time, so a send issued
    // 5s after spawn won't pointlessly wait another 1.5s.
    let already_waited = spawned_at.elapsed();
    let max_wait = readiness_timeout.saturating_sub(already_waited);
    if !max_wait.is_zero() {
        let (lock, cvar) = &**readiness;
        let mut ready = lock.lock();
        if !*ready {
            let _ = cvar.wait_for(&mut ready, max_wait);
        }
    }

    if is_dead() {
        return Err(Error::NotFound(format!("agent {} not running", agent_id)));
    }

    // Per-agent send-mutex. Concurrent writes to the SAME agent
    // serialise here; writes to DIFFERENT agents do not contend
    // because each agent has its own writer Arc.
    let mut w = writer.lock();
    w.write_all(data)
        .map_err(|e| Error::Agent(format!("write: {}", e)))?;
    w.flush()
        .map_err(|e| Error::Agent(format!("flush: {}", e)))?;
    Ok(data.len())
}

impl AgentManager {
    pub fn new(db: DbPool) -> Self {
        Self {
            db,
            handles: Arc::new(Mutex::new(HashMap::new())),
            last_stdout: Arc::new(Mutex::new(HashMap::new())),
            app: Mutex::new(None),
            mcp: Mutex::new(None),
        }
    }

    pub fn set_app_handle(&self, app: AppHandle) {
        *self.app.lock() = Some(app);
    }

    /// Hand the manager a reference to the MCP server so spawned Claude
    /// tiles can be auto-registered with `pigide`. Idempotent — calling
    /// this more than once just replaces the stored handle.
    pub fn set_mcp_handle(&self, handle: Arc<crate::mcp::server::McpServerHandle>) {
        *self.mcp.lock() = Some(handle);
    }

    /// How long ago did this agent last produce output?
    pub fn last_stdout_age(&self, agent_id: &str) -> Option<std::time::Duration> {
        self.last_stdout.lock().get(agent_id).map(|t| t.elapsed())
    }

    /// Mark all agents as exited on app start (PTYs don't survive restarts).
    pub fn reset_statuses(&self) -> Result<()> {
        let conn = self.db.get()?;
        conn.execute("UPDATE agents SET status='exited'", [])?;
        Ok(())
    }

    /// Read the tail of an agent's PTY log. Used by the frontend to replay
    /// scrollback after a session restore so xterm comes up with history.
    pub fn read_log_tail(&self, agent_id: &str, max_bytes: usize) -> Result<Vec<u8>> {
        let path = agent_log_path(agent_id)
            .ok_or_else(|| Error::Other("data dir unavailable".into()))?;
        if !path.exists() {
            return Ok(Vec::new());
        }
        let meta = std::fs::metadata(&path)?;
        let size = meta.len() as usize;
        let start = size.saturating_sub(max_bytes);
        let mut f = std::fs::File::open(&path)?;
        use std::io::{Read as _, Seek as _, SeekFrom};
        f.seek(SeekFrom::Start(start as u64))?;
        let mut buf = Vec::with_capacity(size - start);
        f.read_to_end(&mut buf)?;
        Ok(buf)
    }

    /// Snapshot rows for `restore_session`: every agent the DB still believes
    /// is running. Returned even when the PTY is dead — caller decides what to
    /// re-spawn.
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
        for row in rows { out.push(row?); }
        Ok(out)
    }

    /// Re-spawn an agent under its existing DB id. Used by session restore so
    /// the workspace layout (which references agent ids) stays valid.
    /// Returns Ok(false) when the row does not exist anymore.
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
        // Borrow `Arc<Self>` so the spawn helper has access to last_stdout etc.
        self.spawn_internal(&workspace_id, agent_type, cwd, Some(id), None)?;
        Ok(true)
    }

    /// Restore every persisted-running agent. Returns (restored, failed).
    /// Failures fall back to marking the row exited so the layout can prune it.
    pub fn restore_session(self: &Arc<Self>) -> Result<(usize, usize)> {
        let snapshot = self.list_persisted_running()?;
        let mut restored = 0;
        let mut failed = 0;
        for a in snapshot {
            match self.respawn_persisted(&a.id) {
                Ok(true) => restored += 1,
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!("session restore: {} -> {}", a.id, e);
                    failed += 1;
                    if let Ok(c) = self.db.get() {
                        let _ = c.execute(
                            "UPDATE agents SET status='exited' WHERE id=?1",
                            [&a.id],
                        );
                    }
                }
            }
        }
        Ok((restored, failed))
    }

    fn resolve_binary(&self, agent_type: &AgentType) -> Result<String> {
        // Check settings override first.
        let key = format!("bin.{}", agent_type.as_str());
        if let Ok(Some(v)) = crate::db::get_setting(&self.db, &key) {
            if !v.trim().is_empty() {
                return Ok(v);
            }
        }
        // Defaults: try common install locations and PATH.
        let home = std::env::var("HOME").unwrap_or_default();
        let candidates = match agent_type {
            AgentType::KiroCli => vec![
                format!("{}/.local/bin/kiro-cli", home),
                "/usr/local/bin/kiro-cli".to_string(),
                "/usr/bin/kiro-cli".to_string(),
                "kiro-cli".to_string(),
            ],
            AgentType::Claude => vec![
                format!("{}/.local/bin/claude", home),
                "/usr/bin/claude".to_string(),
                "/usr/local/bin/claude".to_string(),
                "claude".to_string(),
            ],
            AgentType::Aider => vec![
                format!("{}/.local/bin/aider", home),
                "/usr/local/bin/aider".to_string(),
                "/usr/bin/aider".to_string(),
                "aider".to_string(),
            ],
            AgentType::Goose => vec![
                format!("{}/.local/bin/goose", home),
                "/usr/local/bin/goose".to_string(),
                "/usr/bin/goose".to_string(),
                "goose".to_string(),
            ],
            AgentType::OpenCode => vec![
                format!("{}/.local/bin/opencode", home),
                "/usr/local/bin/opencode".to_string(),
                "/usr/bin/opencode".to_string(),
                "opencode".to_string(),
            ],
            // The official install script symlinks `devin` into
            // `~/.local/bin/`, so try that first.
            AgentType::Devin => vec![
                format!("{}/.local/bin/devin", home),
                "/usr/local/bin/devin".to_string(),
                "/usr/bin/devin".to_string(),
                "devin".to_string(),
            ],
            AgentType::Ssh => vec![
                "/usr/bin/ssh".to_string(),
                "/usr/local/bin/ssh".to_string(),
                "ssh".to_string(),
            ],
        };
        for c in candidates {
            if c.is_empty() { continue; }
            if std::path::Path::new(&c).exists() {
                return Ok(c);
            }
        }
        // Fallback: rely on PATH lookup by command name.
        Ok(match agent_type {
            AgentType::KiroCli => "kiro-cli".into(),
            AgentType::Claude => "claude".into(),
            AgentType::Aider => "aider".into(),
            AgentType::Goose => "goose".into(),
            AgentType::OpenCode => "opencode".into(),
            AgentType::Devin => "devin".into(),
            AgentType::Ssh => "ssh".into(),
        })
    }

    pub fn spawn(
        self: &Arc<Self>,
        workspace_id: &str,
        agent_type: AgentType,
        cwd: Option<String>,
    ) -> Result<Agent> {
        self.spawn_internal(workspace_id, agent_type, cwd, None, None)
    }

    /// Spawn with an explicit argv that bypasses both the default args and
    /// any `args.<type>` override. Used by `crate::ssh::spawn_preset` so each
    /// SSH session can carry its own (host, port, identity, …) without
    /// trampling the global `args.ssh` knob.
    pub fn spawn_with_args(
        self: &Arc<Self>,
        workspace_id: &str,
        agent_type: AgentType,
        cwd: Option<String>,
        args: Vec<String>,
    ) -> Result<Agent> {
        self.spawn_internal(workspace_id, agent_type, cwd, None, Some(args))
    }

    /// Internal: when `reuse_id` is set, the agent row is UPSERT'd at that id
    /// (used by `restore_session`). Otherwise a fresh UUID is allocated.
    /// `args_override`, when present, supersedes both the per-type setting
    /// and the built-in default args.
    pub(crate) fn spawn_internal(
        self: &Arc<Self>,
        workspace_id: &str,
        agent_type: AgentType,
        cwd: Option<String>,
        reuse_id: Option<String>,
        args_override: Option<Vec<String>>,
    ) -> Result<Agent> {
        let id = reuse_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let bin = self.resolve_binary(&agent_type)?;
        let wsl = wsl_config(&self.db, &agent_type);
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| Error::Agent(format!("openpty: {}", e)))?;

        // When WSL is configured, swap the executable for wsl.exe and prefix
        // the agent name so the original binary runs inside the distro.
        // Resulting argv:  wsl.exe [-d <distro>] -- <bin> [agent args]
        let mut cmd = match wsl.as_ref() {
            Some(w) => {
                let mut c = CommandBuilder::new(&w.exe);
                if let Some(d) = &w.distro {
                    c.arg("-d");
                    c.arg(d);
                }
                c.arg("--");
                c.arg(&bin);
                c
            }
            None => CommandBuilder::new(&bin),
        };
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        if let Ok(home) = std::env::var("HOME") {
            cmd.env("HOME", &home);
        }
        if let Ok(path) = std::env::var("PATH") {
            cmd.env("PATH", &path);
        }
        if let Ok(lang) = std::env::var("LANG") {
            cmd.env("LANG", &lang);
        }
        // Per-agent default arguments. Settings keys `args.kiro-cli` /
        // `args.claude` (whitespace-separated) override these.
        let default_args: &[&str] = match agent_type {
            AgentType::KiroCli => &["chat", "--trust-all-tools"],
            AgentType::Claude => &[],
            AgentType::Aider => &["--no-auto-commits"],
            AgentType::Goose => &["session"],
            AgentType::OpenCode => &[],
            // `devin` with no arguments launches the interactive TUI; see
            // https://cli.devin.ai/docs/reference/commands. Users who want a
            // specific permission mode (e.g. `--permission-mode bypass`) can
            // set `args.devin` to override.
            AgentType::Devin => &[],
            // SSH always needs a target host — caller supplies args via the
            // per-spawn override (e.g. crate::ssh::spawn_preset).
            AgentType::Ssh => &[],
        };
        let arg_key = format!("args.{}", agent_type.as_str());
        let args_override_setting = crate::db::get_setting(&self.db, &arg_key)
            .ok()
            .flatten();
        if let Some(explicit) = args_override.as_ref() {
            for tok in explicit {
                cmd.arg(tok);
            }
        } else if let Some(s) = args_override_setting {
            for tok in s.split_whitespace() {
                cmd.arg(tok);
            }
        } else {
            for a in default_args {
                cmd.arg(*a);
            }
        }
        // Auto-register the in-process PigMCP server with new Claude Code
        // tiles. We pass the config inline via `--mcp-config <json>` so we
        // never have to write to (and risk clobbering) the user's
        // `~/.claude.json`. No-ops if the server isn't running yet — the
        // tile launches without `pigide`, and a later
        // `mcp_register_cwd` call (or relaunch) will wire it up.
        if matches!(agent_type, AgentType::Claude) {
            if let Some(handle) = self.mcp.lock().clone() {
                match crate::mcp::launcher::build_mcp_config_arg(&self.db, &handle) {
                    Ok(Some(cfg)) => {
                        cmd.arg("--mcp-config");
                        cmd.arg(cfg);
                    }
                    Ok(None) => {
                        tracing::info!(
                            "claude tile {}: MCP server not running, skipping auto-register",
                            id
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            "claude tile {}: failed to build --mcp-config: {}",
                            id,
                            e
                        );
                    }
                }
            }
        }
        let working_dir = cwd.clone().unwrap_or_else(|| {
            std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string())
        });
        cmd.cwd(&working_dir);

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| Error::Agent(format!("spawn: {}", e)))?;

        // We no longer need the slave PTY end (child holds it).
        drop(pair.slave);

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| Error::Agent(format!("writer: {}", e)))?;
        let writer = Arc::new(Mutex::new(writer));
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| Error::Agent(format!("reader: {}", e)))?;

        let agent_id = id.clone();
        let app_clone = self.app.lock().clone();
        let log_path = agent_log_path(&agent_id);
        let last_stdout_map = self.last_stdout.clone();
        let readiness = Arc::new((Mutex::new(false), Condvar::new()));
        let readiness_for_reader = readiness.clone();
        let mgr_for_reader: Arc<AgentManager> = self.clone();
        // Reader thread: pump bytes -> emit events + append to per-agent log.
        // First non-empty read flips `readiness` to true so any pending
        // `write()` call (blocked on the readiness gate) can proceed. On
        // EOF / read error we tear the handle down so subsequent writes
        // fail fast with NotFound instead of "succeeding" into a dead PTY.
        std::thread::spawn(move || {
            let mut log_file = log_path
                .as_ref()
                .and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Some(f) = log_file.as_mut() {
                            let _ = f.write_all(&buf[..n]);
                        }
                        last_stdout_map
                            .lock()
                            .insert(agent_id.clone(), std::time::Instant::now());
                        // Mark agent ready on first stdout so blocked
                        // writers can proceed.
                        let (lock, cvar) = &*readiness_for_reader;
                        let mut ready = lock.lock();
                        if !*ready {
                            *ready = true;
                            cvar.notify_all();
                        }
                        drop(ready);
                        if let Some(app) = &app_clone {
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
                            let _ = app.emit(
                                EV_AGENT_STDOUT,
                                serde_json::json!({ "agent_id": agent_id, "data_b64": b64 }),
                            );
                        }
                    }
                    Err(_) => break,
                }
            }
            // PTY closed. Wake any writer still parked on the readiness
            // gate so it can observe the now-missing handle and return Err.
            {
                let (lock, cvar) = &*readiness_for_reader;
                let mut ready = lock.lock();
                *ready = true;
                cvar.notify_all();
            }
            // Drop the handle and update DB so subsequent send_to_agent
            // returns a real error instead of writing into the void.
            mgr_for_reader.handles.lock().remove(&agent_id);
            if let Ok(c) = mgr_for_reader.db.get() {
                let _ = c.execute(
                    "UPDATE agents SET status='exited' WHERE id=?1",
                    [&agent_id],
                );
            }
            if let Some(app) = &app_clone {
                let _ = app.emit(
                    EV_AGENT_EXIT,
                    serde_json::json!({ "agent_id": agent_id }),
                );
            }
        });

        // Persist row (UPSERT to support reuse_id from session restore).
        let conn = self.db.get()?;
        let created_at = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO agents(id,workspace_id,type,cwd,status,created_at)
             VALUES(?1,?2,?3,?4,'running',?5)
             ON CONFLICT(id) DO UPDATE SET
                workspace_id=excluded.workspace_id,
                type=excluded.type,
                cwd=excluded.cwd,
                status='running'",
            rusqlite::params![&id, workspace_id, agent_type.as_str(), &cwd, &created_at],
        )?;

        // Store handle.
        self.handles.lock().insert(
            id.clone(),
            AgentRuntime {
                master: pair.master,
                writer,
                child: Arc::new(Mutex::new(child)),
                readiness,
                spawned_at: Instant::now(),
                cols: 80,
                rows: 24,
            },
        );

        let agent = Agent {
            id: id.clone(),
            workspace_id: workspace_id.to_string(),
            agent_type: agent_type.as_str().to_string(),
            cwd: cwd.clone(),
            status: "running".into(),
            created_at: created_at.clone(),
        };

        // Notify frontend so it can update its agents map even when spawn was
        // initiated by the orchestrator (not via the frontend itself).
        if let Some(app) = self.app.lock().as_ref() {
            let _ = app.emit(EV_AGENT_SPAWNED, &agent);
        }

        Ok(agent)
    }

    /// Write bytes to the agent's stdin.
    ///
    /// Behaviour (fixes the "send_to_agent silently drops bytes" bug):
    /// 1. **Readiness gate** — if the PTY hasn't yet produced its first
    ///    stdout chunk, block up to `readiness.timeout_ms` (default 1500ms)
    ///    waiting for it. Without this, prompts sent immediately after
    ///    `spawn_agent` get fed to the CLI's startup banner and lost.
    /// 2. **Stale-handle guard** — if the child has already exited
    ///    (reader thread cleaned up the handle, OR `try_wait` reports a
    ///    status), return `Error::NotFound` instead of writing into a
    ///    dead PTY and pretending success.
    /// 3. **Full write loop** — `write_all` retries short writes until the
    ///    whole buffer is delivered or a real I/O error fires.
    /// 4. **Per-agent send-mutex** — concurrent calls to the same agent
    ///    serialise on the per-writer Mutex; calls to *different* agents
    ///    do not contend (the global handles lock is released before the
    ///    write).
    ///
    /// Returns the number of bytes actually written (always equals
    /// `data.len()` on success because we use `write_all`).
    pub fn write(&self, agent_id: &str, data: &[u8]) -> Result<usize> {
        // Snapshot the per-agent handles we need *without* holding the
        // global lock across the I/O. If the agent is unknown -> error.
        let (writer, child, readiness, spawned_at) = {
            let handles = self.handles.lock();
            let h = handles
                .get(agent_id)
                .ok_or_else(|| Error::NotFound(format!("agent {}", agent_id)))?;
            (
                h.writer.clone(),
                h.child.clone(),
                h.readiness.clone(),
                h.spawned_at,
            )
        };

        let timeout_ms = crate::db::get_setting(&self.db, "readiness.timeout_ms")
            .ok()
            .flatten()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_READINESS_TIMEOUT_MS);
        let handles_arc = self.handles.clone();
        let agent_id_owned = agent_id.to_string();
        let child_for_liveness = child.clone();
        write_runtime(
            &writer,
            &readiness,
            spawned_at,
            Duration::from_millis(timeout_ms),
            data,
            agent_id,
            move || {
                // Dead if try_wait reports any exit status, OR the
                // reader thread already removed the entry on EOF.
                if let Ok(Some(_)) = child_for_liveness.lock().try_wait() {
                    return true;
                }
                !handles_arc.lock().contains_key(&agent_id_owned)
            },
        )
    }

    pub fn resize(&self, agent_id: &str, cols: u16, rows: u16) -> Result<()> {
        let mut handles = self.handles.lock();
        let h = handles
            .get_mut(agent_id)
            .ok_or_else(|| Error::NotFound(format!("agent {}", agent_id)))?;
        h.master
            .resize(PtySize {
                cols,
                rows,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| Error::Agent(format!("resize: {}", e)))?;
        h.cols = cols;
        h.rows = rows;
        Ok(())
    }

    pub fn kill(&self, agent_id: &str) -> Result<()> {
        let mut handles = self.handles.lock();
        if let Some(h) = handles.remove(agent_id) {
            let mut child = h.child.lock();
            let _ = child.kill();
            let _ = child.wait();
        }
        let conn = self.db.get()?;
        conn.execute(
            "UPDATE agents SET status='exited' WHERE id=?1",
            [agent_id],
        )?;
        Ok(())
    }

    pub fn list(&self, workspace_id: &str) -> Result<Vec<Agent>> {
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
        for row in rows { out.push(row?); }
        Ok(out)
    }

    /// Kill all running PTYs (called on app shutdown).
    pub fn kill_all(&self) {
        let mut handles = self.handles.lock();
        for (_id, h) in handles.drain() {
            let mut child = h.child.lock();
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Ok(c) = self.db.get() {
            let _ = c.execute("UPDATE agents SET status='exited'", []);
        }
    }
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
        ];
        for (s, expected) in cases {
            let parsed = AgentType::parse(s).expect("parse");
            assert_eq!(parsed, expected);
            assert_eq!(parsed.as_str(), s);
        }
    }

    #[test]
    fn agent_type_aliases() {
        assert_eq!(AgentType::parse("kiro"), Some(AgentType::KiroCli));
        assert_eq!(AgentType::parse("claude-code"), Some(AgentType::Claude));
        assert_eq!(AgentType::parse("oc"), Some(AgentType::OpenCode));
        assert_eq!(AgentType::parse("devin-cli"), Some(AgentType::Devin));
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
             INSERT INTO workspaces(id) VALUES('w1');",
        )
        .unwrap();
        pool
    }

    #[test]
    fn list_persisted_running_only_picks_running_rows() {
        let p = test_pool();
        let conn = p.get().unwrap();
        conn.execute(
            "INSERT INTO agents(id,workspace_id,type,cwd,status,created_at)
             VALUES('a1','w1','claude','/tmp','running','2026-01-01T00:00:00Z'),
                    ('a2','w1','aider',NULL,'exited','2026-01-01T00:00:01Z')",
            [],
        )
        .unwrap();
        let mgr = AgentManager::new(p);
        let rows = mgr.list_persisted_running().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "a1");
        assert_eq!(rows[0].agent_type, "claude");
    }

    #[test]
    fn read_log_tail_returns_empty_when_missing() {
        let p = test_pool();
        let mgr = AgentManager::new(p);
        let bytes = mgr.read_log_tail("does-not-exist", 1024).unwrap();
        assert!(bytes.is_empty());
    }

    #[test]
    fn wsl_config_off_when_setting_absent() {
        let p = test_pool();
        // No `wsl.claude` row at all → must return None on every platform.
        assert!(wsl_config(&p, &AgentType::Claude).is_none());
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn wsl_config_ignored_on_non_windows() {
        let p = test_pool();
        // Even with the flag set, non-Windows builds skip the WSL path so
        // the existing Linux/macOS spawn behaviour is unchanged.
        let conn = p.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO settings(key,value) VALUES('wsl.claude','true');",
        )
        .unwrap();
        assert!(wsl_config(&p, &AgentType::Claude).is_none());
    }

    // ----- write_runtime: send_to_agent reliability -----------------

    /// Test writer that simulates `portable-pty`'s `Write` impl with a
    /// configurable short-write count. `write()` returns up to
    /// `chunk_size` bytes per call so the inner `write_all` loop has to
    /// iterate. We track the **total** bytes that landed and the number
    /// of underlying `write()` calls, so a passing test proves the loop
    /// drained the whole buffer and didn't drop a tail.
    struct ChunkedWriter {
        sink: Arc<Mutex<Vec<u8>>>,
        chunk_size: usize,
        write_calls: Arc<Mutex<usize>>,
    }
    impl Write for ChunkedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            *self.write_calls.lock() += 1;
            let n = buf.len().min(self.chunk_size);
            self.sink.lock().extend_from_slice(&buf[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }

    fn make_writer(chunk_size: usize) -> (Arc<Mutex<Box<dyn Write + Send>>>, Arc<Mutex<Vec<u8>>>, Arc<Mutex<usize>>) {
        let sink = Arc::new(Mutex::new(Vec::<u8>::new()));
        let calls = Arc::new(Mutex::new(0usize));
        let w = ChunkedWriter {
            sink: sink.clone(),
            chunk_size,
            write_calls: calls.clone(),
        };
        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(Box::new(w)));
        (writer, sink, calls)
    }

    #[test]
    fn write_runtime_loops_through_short_writes() {
        // 5 KiB payload, writer hands back only 4 bytes per call →
        // write_all must iterate ~1280 times, never dropping the tail.
        let (writer, sink, calls) = make_writer(4);
        let readiness = Arc::new((Mutex::new(true), Condvar::new()));
        let payload: Vec<u8> = (0..5120).map(|i| (i % 251) as u8).collect();

        let n = write_runtime(
            &writer,
            &readiness,
            Instant::now(),
            Duration::from_millis(0),
            &payload,
            "a1",
            || false,
        )
        .expect("write should succeed");

        assert_eq!(n, payload.len(), "honest bytes count");
        assert_eq!(*sink.lock(), payload, "every byte landed in order");
        assert!(*calls.lock() >= payload.len() / 4, "loop ran enough iterations");
    }

    #[test]
    fn write_runtime_returns_err_on_stale_handle() {
        // Liveness check returns true (= dead) before we ever touch the
        // writer. Old behaviour silently returned Ok and lied about
        // bytes; new behaviour must Err with NotFound.
        let (writer, sink, _calls) = make_writer(4096);
        let readiness = Arc::new((Mutex::new(true), Condvar::new()));

        let res = write_runtime(
            &writer,
            &readiness,
            Instant::now(),
            Duration::from_millis(0),
            b"hello",
            "ghost",
            || true, // is_dead
        );

        assert!(matches!(res, Err(Error::NotFound(_))), "expected NotFound, got {:?}", res);
        assert!(sink.lock().is_empty(), "must NOT touch a dead PTY");
    }

    #[test]
    fn write_runtime_blocks_until_ready_then_proceeds() {
        // Spawn a reader thread that flips readiness after 50ms. Writer
        // is called before then; the readiness gate must hold it.
        let (writer, sink, _calls) = make_writer(4096);
        let readiness = Arc::new((Mutex::new(false), Condvar::new()));
        let r2 = readiness.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            let (lock, cvar) = &*r2;
            *lock.lock() = true;
            cvar.notify_all();
        });

        let started = Instant::now();
        let n = write_runtime(
            &writer,
            &readiness,
            Instant::now(),
            Duration::from_millis(500), // generous grace
            b"prompt",
            "a1",
            || false,
        )
        .expect("write should succeed once ready");
        let elapsed = started.elapsed();

        assert_eq!(n, 6);
        assert_eq!(&*sink.lock(), b"prompt");
        assert!(elapsed >= Duration::from_millis(40), "must have waited for readiness");
        assert!(elapsed < Duration::from_millis(450), "must not have hit the timeout");
    }

    #[test]
    fn write_runtime_proceeds_after_grace_when_never_ready() {
        // CLI never echoes — readiness stays false — gate must time out
        // and let the write through (better delivered than wedged).
        let (writer, sink, _calls) = make_writer(4096);
        let readiness = Arc::new((Mutex::new(false), Condvar::new()));

        let started = Instant::now();
        let n = write_runtime(
            &writer,
            &readiness,
            Instant::now(),
            Duration::from_millis(80),
            b"x",
            "a1",
            || false,
        )
        .expect("write should still go through after grace");
        let elapsed = started.elapsed();

        assert_eq!(n, 1);
        assert_eq!(&*sink.lock(), b"x");
        assert!(elapsed >= Duration::from_millis(60), "must have waited the grace");
    }

    #[test]
    fn write_runtime_serialises_concurrent_writes_to_same_agent() {
        // Two threads racing into the same writer: bytes must not
        // interleave (Per-agent send-mutex guarantees A1's bytes land
        // contiguous, then A2's, or vice versa — never AABBABAB).
        let (writer, sink, _calls) = make_writer(64);
        let readiness = Arc::new((Mutex::new(true), Condvar::new()));
        let blob_a = vec![b'A'; 256];
        let blob_b = vec![b'B'; 256];

        let w1 = writer.clone();
        let r1 = readiness.clone();
        let a = blob_a.clone();
        let h1 = std::thread::spawn(move || {
            write_runtime(&w1, &r1, Instant::now(), Duration::from_millis(0), &a, "a", || false)
        });
        let w2 = writer.clone();
        let r2 = readiness.clone();
        let b = blob_b.clone();
        let h2 = std::thread::spawn(move || {
            write_runtime(&w2, &r2, Instant::now(), Duration::from_millis(0), &b, "a", || false)
        });
        h1.join().unwrap().unwrap();
        h2.join().unwrap().unwrap();

        let s = sink.lock().clone();
        assert_eq!(s.len(), 512);
        // Find the boundary: the first run of A's must be exactly 256
        // (not interleaved with B's), or the first run of B's must be 256.
        let first = s[0];
        let run_len = s.iter().take_while(|&&c| c == first).count();
        assert_eq!(run_len, 256, "writes must not interleave; got {:?}…", &s[..16]);
    }

    #[test]
    fn write_runtime_handles_utf8_payload_intact() {
        // A small writer chunk size could only corrupt UTF-8 if we cut
        // the input into chunks ourselves (we don't — we call
        // `write_all` and let the loop drive). This locks that contract
        // in: cyrillic + emoji round-trips byte-for-byte through a
        // 3-byte chunk writer that lands in the middle of multi-byte
        // sequences.
        let (writer, sink, _calls) = make_writer(3);
        let readiness = Arc::new((Mutex::new(true), Condvar::new()));
        let s = "Привет 🐷 мир";
        let payload = s.as_bytes();

        let n = write_runtime(
            &writer,
            &readiness,
            Instant::now(),
            Duration::from_millis(0),
            payload,
            "a1",
            || false,
        ).expect("write");

        assert_eq!(n, payload.len());
        let sink_bytes = sink.lock().clone();
        assert_eq!(sink_bytes, payload);
        assert_eq!(std::str::from_utf8(&sink_bytes).unwrap(), s);
    }
}
