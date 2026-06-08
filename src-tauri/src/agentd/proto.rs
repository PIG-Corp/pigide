//! Wire protocol between PigIDE (client) and `pigide-agentd` (broker).
//!
//! Transport: unix domain socket at `$XDG_RUNTIME_DIR/pigide/agentd.sock`
//! (or `$TMPDIR/pigide-agentd-$UID.sock` as fallback). Each connection is a
//! bidirectional stream of newline-delimited JSON frames (NDJSON). One frame
//! per line. Empty lines and lines starting with `#` are ignored.
//!
//! Two frame kinds flow over the same connection:
//!
//!   1. **Request / Response** — client sends [`Request`] with a unique `id`,
//!      broker replies with [`Response`] carrying the same `id`. Pipelined:
//!      multiple in-flight requests are allowed, the client matches replies
//!      by id.
//!
//!   2. **Event** — broker pushes [`Event`] frames (no `id`) for stdout
//!      chunks, agent exits, etc. Events are only emitted to a connection
//!      after it issues a `Subscribe` request.
//!
//! The protocol is intentionally tiny and forward-compatible: unknown fields
//! are ignored on both sides, unknown opcodes return `Error::UnknownOp`. New
//! opcodes can be added without bumping `PROTOCOL_VERSION` as long as old
//! ones keep their shape.

use serde::{Deserialize, Serialize};

/// Bumped only on **incompatible** wire changes. The broker advertises its
/// supported version in [`HelloResponse`]; clients refuse to talk if the
/// major part differs.
pub const PROTOCOL_VERSION: u32 = 1;

/// Maximum size of a single NDJSON frame, in bytes. Frames larger than this
/// are dropped with a connection-level error. Sized for stdout chunks of up
/// to 64 KiB (with base64 overhead) plus protocol envelope.
pub const MAX_FRAME_BYTES: usize = 256 * 1024;

/// Default socket path resolver. Mirrors the convention used by `gpg-agent`
/// / `ssh-agent`: prefer `$XDG_RUNTIME_DIR` (per-user, tmpfs, auto-cleaned
/// on logout) and fall back to `$TMPDIR` so the broker still works on
/// systems where the runtime dir is missing (some CI containers).
pub fn default_socket_path() -> std::path::PathBuf {
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        if !xdg.is_empty() {
            return std::path::PathBuf::from(xdg)
                .join("pigide")
                .join("agentd.sock");
        }
    }
    let tmp = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let uid = unsafe { libc_uid() };
    std::path::PathBuf::from(tmp).join(format!("pigide-agentd-{}.sock", uid))
}

#[cfg(unix)]
unsafe fn libc_uid() -> u32 {
    // SAFETY: getuid is always-safe per POSIX.
    extern "C" {
        fn getuid() -> u32;
    }
    getuid()
}

#[cfg(not(unix))]
unsafe fn libc_uid() -> u32 {
    0
}

/// Client → broker. Tagged by `op`. `id` is opaque to the broker and is
/// echoed back on the matching [`Response`]. Use UUIDs or a monotonically
/// increasing counter — the only constraint is uniqueness within a single
/// connection's in-flight window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub op: Op,
}

/// Broker → client (request/response leg). The `id` matches the
/// originating [`Request`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    #[serde(flatten)]
    pub result: ResponseBody,
}

/// Broker → client (event leg). Pushed after the connection issues a
/// `Subscribe` op. No `id` — events are not solicited.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// New PTY output. `data_b64` is base64-encoded raw bytes.
    Stdout { agent_id: String, data_b64: String },
    /// Agent's PTY closed (child exited or was killed). After this, the
    /// `agent_id` is no longer valid for `Write` / `Resize`. The DB row
    /// will be marked `exited` unless the broker is in shutdown mode.
    Exit { agent_id: String },
    /// Broker is shutting down. All connections should disconnect; agents
    /// remain alive (broker is supposed to outlive any single client).
    /// Currently informational only — the broker never voluntarily exits.
    BrokerShutdown { reason: String },
}

/// All client→broker opcodes. Tagged by `op`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// First frame on every connection. Negotiates protocol version and
    /// returns broker metadata. Required before any other op.
    Hello { client_version: u32 },

    /// Spawn a new PTY-backed agent. Returns the canonical [`AgentInfo`]
    /// with the broker-assigned `id`. When `reuse_id` is set the broker
    /// upserts under that id (used by the legacy DB-driven restore path
    /// during migration; once broker-owned state is the source of truth,
    /// this field is unused).
    ///
    /// `bin_path`, `env_overrides`, and `default_args` are resolved on the
    /// PigIDE side (where DB settings live) and passed through verbatim.
    /// The broker has no DB access — it's a pure PTY supervisor.
    Spawn {
        workspace_id: String,
        agent_type: String,
        cwd: Option<String>,
        /// Resolved absolute binary path. PigIDE looks this up via
        /// `bin.<type>` setting → install candidates → PATH lookup.
        bin_path: String,
        /// Argv to pass after `bin_path`. PigIDE resolves the precedence
        /// (caller override → `args.<type>` setting → built-in default).
        argv: Vec<String>,
        /// Extra env vars to inject (HOME, PATH, TERM, COLORTERM, LANG,
        /// MCP config — PigIDE assembles them, broker just forwards).
        env: Vec<(String, String)>,
        /// Optional explicit id (legacy `reuse_id` / `respawn_persisted`).
        reuse_id: Option<String>,
    },

    /// Push bytes into the agent's PTY stdin. `data_b64` is base64 raw
    /// bytes (the protocol is JSON, so binary needs to be encoded).
    Write { agent_id: String, data_b64: String },

    /// Tell the PTY about a new terminal size.
    Resize {
        agent_id: String,
        cols: u16,
        rows: u16,
    },

    /// SIGKILL + DB row update. Use [`Op::Detach`] to drop the connection
    /// reference without killing the child.
    Kill { agent_id: String },

    /// All agents in a workspace.
    List { workspace_id: String },

    /// Last `max_bytes` of the per-agent PTY log (xterm scrollback replay).
    LogTail { agent_id: String, max_bytes: usize },

    /// How long ago did this agent last produce stdout? Used by the
    /// orchestrator's `wait_for_agent_idle` heuristic. Returns `None`-shaped
    /// response if the agent has never produced stdout.
    LastStdoutAge { agent_id: String },

    /// Mark all DB rows as exited. Called by the legacy startup path; once
    /// the broker is the source of truth this becomes a no-op.
    ResetStatuses,

    /// All agents the DB still believes are running. Used during migration
    /// of the restore path; eventually replaced by [`Op::ListAll`] which
    /// asks the broker (not the DB) what's actually live.
    ListPersistedRunning,

    /// All agents currently held by the broker. The broker is the source
    /// of truth — DB is just denormalised metadata.
    ListAll,

    /// Migration helper: re-spawn the row at `agent_id` if it's not
    /// currently held by the broker. No-op if it is.
    RespawnPersisted { agent_id: String },

    /// Subscribe this connection to stdout/exit events for ALL agents.
    /// Events from before the subscription are NOT replayed — clients
    /// rebuild scrollback via [`Op::LogTail`].
    Subscribe,

    /// Drop this connection. The client process is going away (Cmd+Q) but
    /// the agents must keep running. Equivalent to closing the socket;
    /// included as an explicit op for symmetry.
    Detach,

    /// Health probe. Returns broker uptime and live-agent count. Cheap.
    Ping,
}

/// Tagged success/error union for [`Response`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponseBody {
    Hello {
        hello: HelloResponse,
    },
    Spawn {
        agent: AgentInfo,
    },
    Write {
        bytes_written: usize,
    },
    Resize,
    Kill,
    List {
        agents: Vec<AgentInfo>,
    },
    ListAll {
        agents: Vec<AgentInfo>,
    },
    ListPersistedRunning {
        agents: Vec<AgentInfo>,
    },
    LogTail {
        data_b64: String,
    },
    LastStdoutAge {
        age_ms: Option<u64>,
    },
    ResetStatuses,
    RespawnPersisted {
        ok: bool,
    },
    Subscribe,
    Detach,
    Pong {
        uptime_secs: u64,
        live_agents: usize,
    },
    /// Any failure. Op-specific failure modes share this single type so
    /// the client can map them to `Error::*` uniformly.
    Error {
        code: ErrorCode,
        message: String,
    },
}

/// Coarse-grained error classifier. The detailed reason is in
/// [`ResponseBody::Error::message`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Agent id not known to the broker.
    NotFound,
    /// Agent existed but its PTY has closed.
    Gone,
    /// Op rejected for shape reasons (bad workspace_id, unknown agent_type).
    Invalid,
    /// PTY/IO failure during spawn/write/resize.
    Io,
    /// Op opcode not understood (forward-compat).
    UnknownOp,
    /// Connection has not issued `Hello` yet.
    NoHello,
    /// Protocol version mismatch.
    VersionMismatch,
    /// Catch-all for unexpected internal failures.
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloResponse {
    pub broker_version: u32,
    /// Human-readable broker build info (`pigide-agentd 0.1.0`).
    pub broker_build: String,
    /// PID of the broker process. Useful for "is this the broker I just
    /// spawned?" sanity checks during auto-spawn.
    pub broker_pid: u32,
}

/// Mirror of `crate::agent::Agent` over the wire. Kept in this crate (not
/// re-exported) so the protocol module stays freestanding — the broker
/// crate links it without pulling in the full PigIDE state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub id: String,
    pub workspace_id: String,
    pub agent_type: String,
    pub cwd: Option<String>,
    pub status: String,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T: Serialize + for<'de> Deserialize<'de>>(v: T) -> T {
        let s = serde_json::to_string(&v).expect("encode");
        serde_json::from_str(&s).expect("decode")
    }

    #[test]
    fn op_spawn_serializes_with_op_tag() {
        let req = Request {
            id: 7,
            op: Op::Spawn {
                workspace_id: "ws1".into(),
                agent_type: "claude".into(),
                cwd: None,
                bin_path: "/usr/local/bin/claude".into(),
                argv: vec![],
                env: vec![("HOME".into(), "/home/u".into())],
                reuse_id: None,
            },
        };
        let s = serde_json::to_string(&req).unwrap();
        // Tag must be the discriminator the broker dispatches on.
        assert!(s.contains("\"op\":\"spawn\""), "got: {}", s);
        // Round-trips cleanly.
        let back: Request = serde_json::from_str(&s).unwrap();
        match back.op {
            Op::Spawn {
                workspace_id,
                bin_path,
                env,
                ..
            } => {
                assert_eq!(workspace_id, "ws1");
                assert_eq!(bin_path, "/usr/local/bin/claude");
                assert_eq!(env.len(), 1);
            }
            other => panic!("wrong op: {:?}", other),
        }
    }

    #[test]
    fn response_error_keeps_id_and_code() {
        let r = Response {
            id: 42,
            result: ResponseBody::Error {
                code: ErrorCode::NotFound,
                message: "agent xyz".into(),
            },
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"id\":42"));
        assert!(s.contains("\"kind\":\"error\""));
        assert!(s.contains("\"code\":\"not_found\""));
    }

    #[test]
    fn event_uses_event_tag_not_op_tag() {
        let ev = Event::Stdout {
            agent_id: "a1".into(),
            data_b64: "aGk=".into(),
        };
        let s = serde_json::to_string(&ev).unwrap();
        // Discriminator differs from Request — broker can't accidentally
        // dispatch an Event as an Op.
        assert!(s.contains("\"event\":\"stdout\""));
        assert!(!s.contains("\"op\":"));
    }

    #[test]
    fn unknown_op_field_rejected_at_decode() {
        // An unknown opcode produces a serde error rather than silently
        // dispatching. The broker translates it to `ErrorCode::UnknownOp`
        // at the framer layer; the proto itself just refuses to decode.
        let s = r#"{"id":1,"op":"frobnicate"}"#;
        let r: std::result::Result<Request, _> = serde_json::from_str(s);
        assert!(r.is_err());
    }

    #[test]
    fn hello_round_trips_through_response() {
        let original = Response {
            id: 1,
            result: ResponseBody::Hello {
                hello: HelloResponse {
                    broker_version: PROTOCOL_VERSION,
                    broker_build: "pigide-agentd 0.1.0".into(),
                    broker_pid: 12345,
                },
            },
        };
        let s = serde_json::to_string(&original).unwrap();
        let back: Response = serde_json::from_str(&s).unwrap();
        match back.result {
            ResponseBody::Hello { hello } => {
                assert_eq!(hello.broker_version, PROTOCOL_VERSION);
                assert_eq!(hello.broker_pid, 12345);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn spawn_response_carries_full_agent_info() {
        let info = AgentInfo {
            id: "a1".into(),
            workspace_id: "ws1".into(),
            agent_type: "claude".into(),
            cwd: Some("/tmp".into()),
            status: "running".into(),
            created_at: "2026-05-21T19:00:00Z".into(),
        };
        let r = Response {
            id: 9,
            result: ResponseBody::Spawn {
                agent: info.clone(),
            },
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: Response = serde_json::from_str(&s).unwrap();
        match back.result {
            ResponseBody::Spawn { agent } => {
                assert_eq!(agent.id, info.id);
                assert_eq!(agent.cwd.as_deref(), Some("/tmp"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn list_response_round_trips_vec() {
        let info = AgentInfo {
            id: "a1".into(),
            workspace_id: "ws1".into(),
            agent_type: "claude".into(),
            cwd: None,
            status: "running".into(),
            created_at: "2026-05-21T19:00:00Z".into(),
        };
        let r = Response {
            id: 3,
            result: ResponseBody::List {
                agents: vec![info.clone()],
            },
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: Response = serde_json::from_str(&s).unwrap();
        match back.result {
            ResponseBody::List { agents } => {
                assert_eq!(agents.len(), 1);
                assert_eq!(agents[0].id, "a1");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn write_op_carries_base64_payload() {
        let req = roundtrip(Request {
            id: 1,
            op: Op::Write {
                agent_id: "a1".into(),
                data_b64: "aGVsbG8=".into(),
            },
        });
        match req.op {
            Op::Write { data_b64, .. } => assert_eq!(data_b64, "aGVsbG8="),
            _ => panic!(),
        }
    }

    #[test]
    fn default_socket_path_uses_xdg_when_set() {
        let saved = std::env::var("XDG_RUNTIME_DIR").ok();
        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        let p = default_socket_path();
        assert_eq!(
            p,
            std::path::PathBuf::from("/run/user/1000/pigide/agentd.sock")
        );
        match saved {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
    }

    #[test]
    fn default_socket_path_falls_back_to_tmpdir_when_xdg_empty() {
        let saved_xdg = std::env::var("XDG_RUNTIME_DIR").ok();
        let saved_tmp = std::env::var("TMPDIR").ok();
        std::env::set_var("XDG_RUNTIME_DIR", "");
        std::env::set_var("TMPDIR", "/tmp");
        let p = default_socket_path();
        assert!(p.starts_with("/tmp"));
        let name = p.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("pigide-agentd-"), "got: {}", name);
        match saved_xdg {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
        match saved_tmp {
            Some(v) => std::env::set_var("TMPDIR", v),
            None => std::env::remove_var("TMPDIR"),
        }
    }

    #[test]
    fn frame_size_constant_fits_64kib_b64_chunk() {
        // 64 KiB raw bytes = 87381 chars base64 + JSON envelope overhead.
        // MAX_FRAME_BYTES must comfortably hold that.
        let raw = 64 * 1024;
        let b64 = (raw + 2) / 3 * 4;
        let envelope = 200; // generous estimate for `{"id":...,"op":...,"data_b64":"..."}`
        assert!(b64 + envelope < MAX_FRAME_BYTES);
    }
}
