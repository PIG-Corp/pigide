//! Pure PTY supervisor. Owns the live `HashMap<agent_id, AgentRuntime>`
//! and the per-agent log files. Has no socket, no DB, no Tauri dependency
//! — both the broker binary and (later) integration tests link against it.
//!
//! The engine speaks in proto types: callers pass [`SpawnRequest`] (mirrors
//! `Op::Spawn`) and receive [`AgentInfo`]. Stdout fan-out happens through
//! a tokio `broadcast::Sender` returned by [`Engine::subscribe`]. Multiple
//! subscribers see the same byte stream — required so a future
//! "two PigIDE windows attached to one broker" deployment Just Works.
//!
//! Lifecycle:
//!
//!   spawn  → opens PTY, forks child, starts a reader thread that pumps
//!            bytes into the broadcast channel + appends to the per-agent
//!            log + updates `last_stdout` + flips the `ready` Condvar.
//!   write  → bounded readiness wait → per-agent writer mutex → write_all.
//!   kill   → SIGKILL child, wait, drop master, broadcast Exit event.
//!   on EOF → reader thread removes the runtime, broadcasts Exit, exits.
//!
//! What lives in PigIDE (NOT here):
//!   - the SQLite `agents` table (rows are denormalised metadata)
//!   - the `bin.<type>` / `args.<type>` settings resolution
//!   - workspace / layout state
//!   - the readiness-timeout knob (`readiness.timeout_ms`) — passed in.

use crate::agentd::proto::{AgentInfo, ErrorCode};
use chrono::Utc;
use parking_lot::{Condvar, Mutex};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use uuid::Uuid;

/// Caller-supplied spawn parameters. PigIDE assembles this from its DB
/// settings and passes it across the wire as `Op::Spawn`. The engine
/// itself never touches a database.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    pub workspace_id: String,
    pub agent_type: String,
    pub cwd: Option<String>,
    /// Absolute path to the binary to execute.
    pub bin_path: String,
    /// Argv passed after `bin_path`. May be empty.
    pub argv: Vec<String>,
    /// Extra env vars (HOME, PATH, TERM, …). Inherited env still applies;
    /// these override on collision.
    pub env: Vec<(String, String)>,
    /// Optional fixed id. None → fresh UUID.
    pub reuse_id: Option<String>,
    /// Initial PTY size. Frontend will resize on mount; this is just so
    /// the child sees something sane during its first prints.
    pub cols: u16,
    pub rows: u16,
}

/// Stdout / Exit broadcast item. One channel per engine, multiplexed by
/// `agent_id` — subscribers filter client-side. (A per-agent channel map
/// would save a few cycles but adds a lot of removal-on-EOF complexity.)
#[derive(Debug, Clone)]
pub enum EngineEvent {
    Stdout {
        agent_id: String,
        data: Arc<Vec<u8>>,
    },
    Exit {
        agent_id: String,
    },
}

/// Coarse error type so the broker can map to [`ErrorCode`] without
/// re-classifying strings.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("agent {0} not found")]
    NotFound(String),
    #[error("agent {0} has exited")]
    Gone(String),
    #[error("invalid spawn: {0}")]
    Invalid(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("pty: {0}")]
    Pty(String),
}

impl EngineError {
    pub fn code(&self) -> ErrorCode {
        match self {
            EngineError::NotFound(_) => ErrorCode::NotFound,
            EngineError::Gone(_) => ErrorCode::Gone,
            EngineError::Invalid(_) => ErrorCode::Invalid,
            EngineError::Io(_) | EngineError::Pty(_) => ErrorCode::Io,
        }
    }
}

pub type Result<T> = std::result::Result<T, EngineError>;

/// True when `id` is safe to use as a single filename component. `reuse_id`
/// is supplied by the client and lands in `format!("{}.log", id)`, so a
/// crafted id like `../../etc/passwd` would traverse out of the log dir.
/// Require a non-empty, bounded id of `[A-Za-z0-9._-]` with no `..`.
pub fn is_safe_agent_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 128 {
        return false;
    }
    if id.contains("..") {
        return false;
    }
    id.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

struct Runtime {
    info: AgentInfo,
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    readiness: Arc<(Mutex<bool>, Condvar)>,
    spawned_at: Instant,
}

pub struct Engine {
    runtimes: Arc<Mutex<HashMap<String, Runtime>>>,
    last_stdout: Arc<Mutex<HashMap<String, Instant>>>,
    log_dir: PathBuf,
    /// Single broadcast for ALL agents. Capacity is generous — under
    /// burst, a slow subscriber will be marked lagged and lose events,
    /// which is fine: scrollback is rebuilt from the on-disk log.
    events: broadcast::Sender<EngineEvent>,
    /// Shutting-down flag — when raised, reader threads on EOF skip the
    /// `Exit` broadcast and runtime removal happens unconditionally.
    /// Currently broker-process never raises this (it outlives clients
    /// by design), but kept for parity with the old AgentManager and
    /// for future "controlled broker shutdown" support.
    shutting_down: Arc<AtomicBool>,
    started_at: Instant,
}

/// How long the reader thread blocks on each `read()` call. Doesn't
/// affect latency (kernel returns as soon as bytes are available); just
/// caps how long a dying PTY takes to be noticed if `read` somehow
/// hangs. PTYs in practice always return EOF promptly when the slave
/// is closed.
const READ_BUF_SIZE: usize = 8 * 1024;

/// Default readiness wait: how long `write()` will block waiting for
/// the agent's first stdout chunk before sending anyway. Mirrors the
/// old `DEFAULT_READINESS_TIMEOUT_MS` from `agent.rs`.
pub const DEFAULT_READINESS_TIMEOUT: Duration = Duration::from_millis(1500);

impl Engine {
    pub fn new(log_dir: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&log_dir)?;
        let (events, _) = broadcast::channel(1024);
        Ok(Engine {
            runtimes: Arc::new(Mutex::new(HashMap::new())),
            last_stdout: Arc::new(Mutex::new(HashMap::new())),
            log_dir,
            events,
            shutting_down: Arc::new(AtomicBool::new(false)),
            started_at: Instant::now(),
        })
    }

    /// Subscribe to the engine-wide event stream. Subscribers see events
    /// for ALL agents and filter by `agent_id` themselves.
    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.events.subscribe()
    }

    /// Engine uptime in whole seconds — populates `Pong`.
    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    pub fn live_agents(&self) -> usize {
        self.runtimes.lock().len()
    }

    pub fn list_all(&self) -> Vec<AgentInfo> {
        self.runtimes
            .lock()
            .values()
            .map(|r| r.info.clone())
            .collect()
    }

    pub fn list_workspace(&self, workspace_id: &str) -> Vec<AgentInfo> {
        self.runtimes
            .lock()
            .values()
            .filter(|r| r.info.workspace_id == workspace_id)
            .map(|r| r.info.clone())
            .collect()
    }

    pub fn last_stdout_age(&self, agent_id: &str) -> Option<Duration> {
        self.last_stdout.lock().get(agent_id).map(|t| t.elapsed())
    }

    /// Path of the per-agent stdout log (created on first read/write).
    pub fn log_path(&self, agent_id: &str) -> PathBuf {
        self.log_dir.join(format!("{}.log", agent_id))
    }

    /// Tail of the per-agent stdout log. Returns up to `max_bytes` from
    /// the END of the file. Empty Vec when the log doesn't exist yet
    /// (agent freshly spawned, no stdout yet).
    pub fn log_tail(&self, agent_id: &str, max_bytes: usize) -> Result<Vec<u8>> {
        let path = self.log_path(agent_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let meta = std::fs::metadata(&path)?;
        let size = meta.len() as usize;
        let start = size.saturating_sub(max_bytes);
        let mut f = std::fs::File::open(&path)?;
        f.seek(SeekFrom::Start(start as u64))?;
        let mut buf = Vec::with_capacity(size - start);
        f.read_to_end(&mut buf)?;
        Ok(buf)
    }

    pub fn spawn(self: &Arc<Self>, req: SpawnRequest) -> Result<AgentInfo> {
        if req.bin_path.trim().is_empty() {
            return Err(EngineError::Invalid("bin_path empty".into()));
        }
        // `reuse_id` is caller-controlled and lands in the on-disk log path
        // (`format!("{}.log", id)`). Reject anything that isn't a safe single
        // filename component so it can't traverse out of the log dir.
        if let Some(rid) = req.reuse_id.as_deref() {
            if !is_safe_agent_id(rid) {
                return Err(EngineError::Invalid(format!("invalid reuse_id: {}", rid)));
            }
        }
        let id = req.reuse_id.unwrap_or_else(|| Uuid::new_v4().to_string());

        // Idempotent on reuse_id: if we already host this id, return the
        // current AgentInfo without spawning again.
        {
            if let Some(rt) = self.runtimes.lock().get(&id) {
                return Ok(rt.info.clone());
            }
        }

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: req.rows.max(1),
                cols: req.cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| EngineError::Pty(format!("openpty: {}", e)))?;

        let mut cmd = CommandBuilder::new(&req.bin_path);
        for arg in &req.argv {
            cmd.arg(arg);
        }
        for (k, v) in &req.env {
            cmd.env(k, v);
        }
        let cwd = req
            .cwd
            .clone()
            .unwrap_or_else(|| std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()));
        cmd.cwd(&cwd);

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| EngineError::Pty(format!("spawn: {}", e)))?;
        drop(pair.slave);

        let writer_raw = pair
            .master
            .take_writer()
            .map_err(|e| EngineError::Pty(format!("writer: {}", e)))?;
        let writer = Arc::new(Mutex::new(writer_raw));
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| EngineError::Pty(format!("reader: {}", e)))?;

        let info = AgentInfo {
            id: id.clone(),
            workspace_id: req.workspace_id.clone(),
            agent_type: req.agent_type.clone(),
            cwd: req.cwd.clone(),
            status: "running".into(),
            created_at: Utc::now().to_rfc3339(),
        };

        let readiness = Arc::new((Mutex::new(false), Condvar::new()));
        let runtime = Runtime {
            info: info.clone(),
            master: pair.master,
            writer,
            child: Arc::new(Mutex::new(child)),
            readiness: readiness.clone(),
            spawned_at: Instant::now(),
        };

        // Reader thread: pump bytes → log + broadcast. On EOF: cleanup.
        let log_path = self.log_path(&id);
        let agent_id_for_reader = id.clone();
        let events = self.events.clone();
        let last_stdout = self.last_stdout.clone();
        let runtimes = self.runtimes.clone();
        let shutting_down = self.shutting_down.clone();
        let readiness_for_reader = readiness.clone();

        std::thread::spawn(move || {
            let mut log_file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .ok();
            let mut buf = vec![0u8; READ_BUF_SIZE];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Some(f) = log_file.as_mut() {
                            let _ = f.write_all(&buf[..n]);
                        }
                        last_stdout
                            .lock()
                            .insert(agent_id_for_reader.clone(), Instant::now());
                        // Flip readiness on first non-empty read so any
                        // pending write() can proceed.
                        let (lock, cvar) = &*readiness_for_reader;
                        let mut ready = lock.lock();
                        if !*ready {
                            *ready = true;
                            cvar.notify_all();
                        }
                        drop(ready);
                        let chunk = Arc::new(buf[..n].to_vec());
                        let _ = events.send(EngineEvent::Stdout {
                            agent_id: agent_id_for_reader.clone(),
                            data: chunk,
                        });
                    }
                    Err(_) => break,
                }
            }
            // EOF cleanup. Wake any writer parked on readiness so it
            // observes the now-missing runtime and bails.
            {
                let (lock, cvar) = &*readiness_for_reader;
                let mut ready = lock.lock();
                *ready = true;
                cvar.notify_all();
            }
            runtimes.lock().remove(&agent_id_for_reader);
            if !shutting_down.load(Ordering::SeqCst) {
                let _ = events.send(EngineEvent::Exit {
                    agent_id: agent_id_for_reader,
                });
            }
        });

        self.runtimes.lock().insert(id.clone(), runtime);
        Ok(info)
    }

    /// Write bytes to the agent's PTY stdin. Returns bytes written.
    /// Same readiness gate semantics as the old AgentManager::write.
    pub fn write(&self, agent_id: &str, data: &[u8], readiness_timeout: Duration) -> Result<usize> {
        let (writer, child, readiness, spawned_at) = {
            let runtimes = self.runtimes.lock();
            let rt = runtimes
                .get(agent_id)
                .ok_or_else(|| EngineError::NotFound(agent_id.into()))?;
            (
                rt.writer.clone(),
                rt.child.clone(),
                rt.readiness.clone(),
                rt.spawned_at,
            )
        };

        let already_waited = spawned_at.elapsed();
        let max_wait = readiness_timeout.saturating_sub(already_waited);
        if !max_wait.is_zero() {
            let (lock, cvar) = &*readiness;
            let mut ready = lock.lock();
            if !*ready {
                let _ = cvar.wait_for(&mut ready, max_wait);
            }
        }

        // Liveness re-check: child may have died during the wait.
        if let Ok(Some(_)) = child.lock().try_wait() {
            return Err(EngineError::Gone(agent_id.into()));
        }
        if !self.runtimes.lock().contains_key(agent_id) {
            return Err(EngineError::Gone(agent_id.into()));
        }

        let mut w = writer.lock();
        w.write_all(data)
            .map_err(|e| EngineError::Pty(format!("write: {}", e)))?;
        w.flush()
            .map_err(|e| EngineError::Pty(format!("flush: {}", e)))?;
        drop(w);
        // Reset quiet-timer so a follow-up `last_stdout_age` doesn't
        // false-positive on the previous stdout's timestamp.
        self.last_stdout
            .lock()
            .insert(agent_id.into(), Instant::now());
        Ok(data.len())
    }

    pub fn resize(&self, agent_id: &str, cols: u16, rows: u16) -> Result<()> {
        let mut runtimes = self.runtimes.lock();
        let rt = runtimes
            .get_mut(agent_id)
            .ok_or_else(|| EngineError::NotFound(agent_id.into()))?;
        rt.master
            .resize(PtySize {
                cols,
                rows,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| EngineError::Pty(format!("resize: {}", e)))?;
        Ok(())
    }

    /// SIGKILL + reap. Drops the runtime under the lock so the master fd
    /// closes promptly and the reader thread sees EOF.
    pub fn kill(&self, agent_id: &str) -> Result<()> {
        let runtime = {
            let mut runtimes = self.runtimes.lock();
            runtimes.remove(agent_id)
        };
        let rt = runtime.ok_or_else(|| EngineError::NotFound(agent_id.into()))?;
        {
            let mut child = rt.child.lock();
            let _ = child.kill();
            let _ = child.wait();
        }
        {
            let (lock, cvar) = &*rt.readiness;
            let mut ready = lock.lock();
            *ready = true;
            cvar.notify_all();
        }
        drop(rt.writer);
        drop(rt.master);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Minimal helper: build an engine in a tempdir.
    fn engine_in_temp() -> (Arc<Engine>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let eng = Arc::new(Engine::new(dir.path().to_path_buf()).expect("engine"));
        (eng, dir)
    }

    fn spawn_echo(engine: &Arc<Engine>, body: &str) -> AgentInfo {
        engine
            .spawn(SpawnRequest {
                workspace_id: "ws".into(),
                agent_type: "test".into(),
                cwd: Some("/tmp".into()),
                bin_path: "/bin/sh".into(),
                argv: vec!["-c".into(), format!("printf %s '{}'; sleep 0.05", body)],
                env: vec![],
                reuse_id: None,
                cols: 80,
                rows: 24,
            })
            .expect("spawn")
    }

    #[test]
    fn spawn_echo_emits_stdout_and_exit() {
        let (eng, _dir) = engine_in_temp();
        let mut rx = eng.subscribe();
        let info = spawn_echo(&eng, "hello-world");
        // Drain events until we see Exit for our agent.
        let mut got_data = Vec::new();
        let mut got_exit = false;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            match rx.try_recv() {
                Ok(EngineEvent::Stdout { agent_id, data }) if agent_id == info.id => {
                    got_data.extend_from_slice(&data);
                }
                Ok(EngineEvent::Exit { agent_id }) if agent_id == info.id => {
                    got_exit = true;
                    break;
                }
                Ok(_) => {}
                Err(broadcast::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
        assert!(got_exit, "did not see Exit event in time");
        let s = String::from_utf8_lossy(&got_data);
        assert!(s.contains("hello-world"), "stdout was: {:?}", s);
    }

    #[test]
    fn list_workspace_filters_by_id() {
        let (eng, _dir) = engine_in_temp();
        let a = eng
            .spawn(SpawnRequest {
                workspace_id: "ws-a".into(),
                agent_type: "t".into(),
                cwd: None,
                bin_path: "/bin/sh".into(),
                argv: vec!["-c".into(), "sleep 0.5".into()],
                env: vec![],
                reuse_id: None,
                cols: 80,
                rows: 24,
            })
            .unwrap();
        let _b = eng
            .spawn(SpawnRequest {
                workspace_id: "ws-b".into(),
                agent_type: "t".into(),
                cwd: None,
                bin_path: "/bin/sh".into(),
                argv: vec!["-c".into(), "sleep 0.5".into()],
                env: vec![],
                reuse_id: None,
                cols: 80,
                rows: 24,
            })
            .unwrap();
        let in_a = eng.list_workspace("ws-a");
        assert_eq!(in_a.len(), 1);
        assert_eq!(in_a[0].id, a.id);
        assert_eq!(eng.list_all().len(), 2);
    }

    #[test]
    fn write_after_kill_returns_not_found_or_gone() {
        let (eng, _dir) = engine_in_temp();
        let info = eng
            .spawn(SpawnRequest {
                workspace_id: "ws".into(),
                agent_type: "t".into(),
                cwd: None,
                bin_path: "/bin/sh".into(),
                argv: vec!["-c".into(), "sleep 5".into()],
                env: vec![],
                reuse_id: None,
                cols: 80,
                rows: 24,
            })
            .unwrap();
        eng.kill(&info.id).expect("kill");
        // Either NotFound (already removed) or Gone (caught between
        // remove and try_wait) — both are acceptable terminal states.
        let err = eng
            .write(&info.id, b"x", Duration::from_millis(100))
            .unwrap_err();
        assert!(
            matches!(err, EngineError::NotFound(_) | EngineError::Gone(_)),
            "got: {:?}",
            err
        );
    }

    #[test]
    fn log_tail_returns_empty_for_unknown_agent() {
        let (eng, _dir) = engine_in_temp();
        let bytes = eng.log_tail("does-not-exist", 1024).unwrap();
        assert!(bytes.is_empty());
    }

    #[test]
    fn reuse_id_makes_spawn_idempotent() {
        let (eng, _dir) = engine_in_temp();
        let id = "fixed-id";
        let a = eng
            .spawn(SpawnRequest {
                workspace_id: "ws".into(),
                agent_type: "t".into(),
                cwd: None,
                bin_path: "/bin/sh".into(),
                argv: vec!["-c".into(), "sleep 1".into()],
                env: vec![],
                reuse_id: Some(id.into()),
                cols: 80,
                rows: 24,
            })
            .unwrap();
        // Calling again with the same id returns the existing info, not
        // a new spawn.
        let b = eng
            .spawn(SpawnRequest {
                workspace_id: "ws".into(),
                agent_type: "t".into(),
                cwd: None,
                bin_path: "/bin/sh".into(),
                argv: vec!["-c".into(), "sleep 1".into()],
                env: vec![],
                reuse_id: Some(id.into()),
                cols: 80,
                rows: 24,
            })
            .unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(eng.list_all().len(), 1);
    }

    #[test]
    fn empty_bin_path_rejected() {
        let (eng, _dir) = engine_in_temp();
        let err = eng
            .spawn(SpawnRequest {
                workspace_id: "ws".into(),
                agent_type: "t".into(),
                cwd: None,
                bin_path: "".into(),
                argv: vec![],
                env: vec![],
                reuse_id: None,
                cols: 80,
                rows: 24,
            })
            .unwrap_err();
        assert!(matches!(err, EngineError::Invalid(_)));
    }

    #[test]
    fn engine_error_to_code_mapping() {
        assert_eq!(
            EngineError::NotFound("a".into()).code(),
            ErrorCode::NotFound
        );
        assert_eq!(EngineError::Gone("a".into()).code(), ErrorCode::Gone);
        assert_eq!(EngineError::Invalid("a".into()).code(), ErrorCode::Invalid);
        assert_eq!(EngineError::Pty("a".into()).code(), ErrorCode::Io);
    }

    #[test]
    fn unsafe_reuse_id_rejected() {
        let (eng, _dir) = engine_in_temp();
        let err = eng
            .spawn(SpawnRequest {
                workspace_id: "ws".into(),
                agent_type: "t".into(),
                cwd: None,
                bin_path: "/bin/true".into(),
                argv: vec![],
                env: vec![],
                reuse_id: Some("../../etc/passwd".into()),
                cols: 80,
                rows: 24,
            })
            .unwrap_err();
        assert!(matches!(err, EngineError::Invalid(_)));
    }

    #[test]
    fn is_safe_agent_id_basics() {
        assert!(is_safe_agent_id("550e8400-e29b-41d4-a716-446655440000"));
        assert!(!is_safe_agent_id("../x"));
        assert!(!is_safe_agent_id("a/b"));
        assert!(!is_safe_agent_id(""));
    }

    /// Helper used by the broker binary (and re-exported to ensure the
    /// engine's bytes-to-frame path is exercised in tests).
    fn b64_chunk(bytes: &[u8]) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn b64_helper_round_trips() {
        use base64::Engine as _;
        let raw = b"\x00\x01\xfe\xff hello";
        let s = b64_chunk(raw);
        let back = base64::engine::general_purpose::STANDARD
            .decode(&s)
            .unwrap();
        assert_eq!(back, raw);
    }
}
