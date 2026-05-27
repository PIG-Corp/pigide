//! Fast-lane writer that buffers PTY stdout per agent and flushes a
//! `chats/<agent>/<yyyy-mm-dd>.md` chunk on threshold or agent exit.

use crate::db::DbPool;
use crate::error::Result;
use crate::memory::folders::Kind;
use crate::memory::note::{IngestRecord, Note};
use crate::memory::MemoryService;
use base64::Engine;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::AppHandle;

const CHAT_LINES_KEY: &str = "memory.chat_rotation.lines";
const FAST_INGEST_KEY: &str = "memory.fast_ingest.enabled";
const DEFAULT_LINES: usize = 120;

fn fast_ingest_enabled(db: &DbPool) -> bool {
    crate::db::get_setting(db, FAST_INGEST_KEY)
        .ok()
        .flatten()
        .map(|v| v.to_ascii_lowercase() != "false")
        .unwrap_or(true)
}

fn line_threshold(db: &DbPool) -> usize {
    crate::db::get_setting(db, CHAT_LINES_KEY)
        .ok()
        .flatten()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_LINES)
}

/// Per-agent rolling buffer. Holds raw decoded PTY output until the
/// line-count threshold trips; on flush we emit one note chunk and reset.
#[derive(Default)]
struct AgentBuffer {
    /// Workspace owning the agent — captured on first push so flushes
    /// know where to write.
    workspace_id: Option<String>,
    /// Human-readable agent name (e.g. "claude-tile-1"). Used in slugs.
    agent_name: Option<String>,
    /// Raw decoded text accumulated since the last flush.
    accumulated: String,
    /// Line count since last flush — cheap pre-computed counter.
    lines: usize,
    /// How many chunks we've already emitted today (for the `## chunk N`
    /// section header). Reset when the date rolls over.
    chunks_today: usize,
    /// ISO date (`yyyy-mm-dd`) of the last flush; bumps `chunks_today=0`
    /// on date change.
    last_date: Option<String>,
}

#[derive(Default)]
pub struct ChatBuffer {
    inner: Mutex<HashMap<String, AgentBuffer>>,
}

impl ChatBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register / update agent metadata. Called once on agent spawn so
    /// flushes know the workspace + agent name without a DB round-trip.
    pub fn register_agent(&self, agent_id: &str, workspace_id: &str, agent_name: &str) {
        let mut g = self.inner.lock();
        let entry = g.entry(agent_id.to_string()).or_default();
        entry.workspace_id = Some(workspace_id.to_string());
        entry.agent_name = Some(agent_name.to_string());
    }

    /// Push decoded PTY text into the buffer. Returns `true` when the
    /// threshold has tripped — caller should flush.
    pub fn push(&self, agent_id: &str, text: &str, threshold: usize) -> bool {
        let mut g = self.inner.lock();
        let entry = g.entry(agent_id.to_string()).or_default();
        entry.accumulated.push_str(text);
        entry.lines += text.matches('\n').count();
        entry.lines >= threshold
    }

    /// Drain the buffer for `agent_id`, returning the accumulated text +
    /// metadata needed to compose the chunk note. Returns `None` when
    /// the buffer is empty / the agent isn't registered.
    pub fn drain_for_flush(&self, agent_id: &str, now: DateTime<Utc>) -> Option<DrainedChunk> {
        let mut g = self.inner.lock();
        let entry = g.get_mut(agent_id)?;
        if entry.accumulated.is_empty() {
            return None;
        }
        let workspace_id = entry.workspace_id.clone()?;
        let agent_name = entry.agent_name.clone()?;
        let date = now.format("%Y-%m-%d").to_string();
        if entry.last_date.as_deref() != Some(date.as_str()) {
            entry.last_date = Some(date.clone());
            entry.chunks_today = 0;
        }
        entry.chunks_today += 1;
        let chunk_no = entry.chunks_today;
        let text = std::mem::take(&mut entry.accumulated);
        entry.lines = 0;
        Some(DrainedChunk {
            workspace_id,
            agent_name,
            date,
            chunk_no,
            text,
        })
    }
}

#[derive(Debug)]
pub struct DrainedChunk {
    pub workspace_id: String,
    pub agent_name: String,
    pub date: String,
    pub chunk_no: usize,
    pub text: String,
}

/// Compose the per-day chat chunk body. Each flush appends a new
/// `## chunk N — HH:MM:SS UTC` section so the file shows the agent's
/// timeline within a day.
pub fn render_chunk_body(existing: &str, chunk: &DrainedChunk, now: DateTime<Utc>) -> String {
    let mut out = String::from(existing.trim_end());
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "## chunk {} — {} UTC\n\n",
        chunk.chunk_no,
        now.format("%H:%M:%S")
    ));
    out.push_str("```\n");
    out.push_str(chunk.text.trim_end());
    out.push_str("\n```\n");
    out
}

/// Decode base64 PTY stdout and feed into the buffer. Triggers a flush
/// when the threshold trips. Errors are logged + swallowed.
pub fn on_pty_stdout(
    memory: Arc<MemoryService>,
    db: DbPool,
    app: Option<AppHandle>,
    buffer: Arc<ChatBuffer>,
    agent_id: String,
    data_b64: String,
) {
    if !fast_ingest_enabled(&db) {
        return;
    }
    let decoded = match base64::engine::general_purpose::STANDARD.decode(&data_b64) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) => {
            tracing::debug!(agent_id = %agent_id, "ingest: bad base64: {}", e);
            return;
        }
    };
    let threshold = line_threshold(&db);
    let trip = buffer.push(&agent_id, &decoded, threshold);
    if trip {
        flush_now(memory, db, app, buffer, &agent_id);
    }
}

/// Flush whatever the buffer holds for `agent_id`. Called on agent exit
/// (so we don't lose the tail) and from `on_pty_stdout` when the
/// threshold trips.
pub fn flush_now(
    memory: Arc<MemoryService>,
    db: DbPool,
    app: Option<AppHandle>,
    buffer: Arc<ChatBuffer>,
    agent_id: &str,
) {
    let chunk = match buffer.drain_for_flush(agent_id, Utc::now()) {
        Some(c) => c,
        None => return,
    };
    let slug = format!("chats/{}/{}", chunk.agent_name, chunk.date);
    // Read existing body so we append rather than overwrite.
    let existing = read_existing_body(&memory, &chunk.workspace_id, &slug).unwrap_or_default();
    let body = render_chunk_body(&existing, &chunk, Utc::now());
    let title = format!("{} — {}", chunk.agent_name, chunk.date);
    let ingest = IngestRecord {
        source_kind: "chat_chunk".into(),
        source_ref: Some(agent_id.to_string()),
        ingested_at: Utc::now().to_rfc3339(),
        smart_pass_at: None,
    };
    let res: Result<Note> = memory.upsert_by_slug(
        &chunk.workspace_id,
        &slug,
        &title,
        &body,
        Vec::new(),
        Kind::Chat,
        Some(ingest),
    );
    match res {
        Ok(note) => {
            let _ = super::queue::enqueue_chat(
                &db,
                &chunk.workspace_id,
                agent_id,
                &note.id,
                chunk.chunk_no,
            );
            if let Some(app) = app {
                super::events::emit_note_created(
                    &app,
                    &super::events::NoteCreatedPayload {
                        id: note.id.clone(),
                        slug: note.slug.clone(),
                        title: note.title.clone(),
                        kind: note.kind,
                        source_kind: "chat_chunk".into(),
                    },
                );
            }
        }
        Err(e) => {
            tracing::warn!(agent_id = %agent_id, "fast-lane chat flush failed: {}", e);
        }
    }
}

fn read_existing_body(memory: &MemoryService, workspace_id: &str, slug: &str) -> Option<String> {
    let list = memory.list(workspace_id, None, 500).ok()?;
    let summary = list.into_iter().find(|n| n.slug == slug)?;
    let note = memory.get(&summary.id).ok()?;
    Some(note.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixed_time(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn buffer_tracks_lines_and_trips_at_threshold() {
        let buf = ChatBuffer::new();
        buf.register_agent("a1", "ws-1", "claude-tile-1");
        assert!(!buf.push("a1", "first line\nsecond line\n", 5));
        assert!(!buf.push("a1", "third\nfourth\n", 5));
        // 4 newlines so far; one more line trips at 5.
        assert!(buf.push("a1", "fifth\n", 5));
    }

    #[test]
    fn drain_for_flush_returns_metadata_and_resets_buffer() {
        let buf = ChatBuffer::new();
        buf.register_agent("a1", "ws-1", "claude-tile-1");
        buf.push("a1", "hello\n", 1);
        let now = fixed_time("2026-05-27T12:30:00Z");
        let chunk = buf.drain_for_flush("a1", now).unwrap();
        assert_eq!(chunk.workspace_id, "ws-1");
        assert_eq!(chunk.agent_name, "claude-tile-1");
        assert_eq!(chunk.date, "2026-05-27");
        assert_eq!(chunk.chunk_no, 1);
        assert_eq!(chunk.text, "hello\n");
        // Subsequent drain on an empty buffer returns None.
        assert!(buf.drain_for_flush("a1", now).is_none());
    }

    #[test]
    fn chunk_no_resets_on_new_day() {
        let buf = ChatBuffer::new();
        buf.register_agent("a1", "ws-1", "claude-tile-1");
        buf.push("a1", "day1\n", 1);
        let _ = buf
            .drain_for_flush("a1", fixed_time("2026-05-27T12:00:00Z"))
            .unwrap();
        buf.push("a1", "day2-a\n", 1);
        let c1 = buf
            .drain_for_flush("a1", fixed_time("2026-05-28T08:00:00Z"))
            .unwrap();
        assert_eq!(c1.chunk_no, 1);
        buf.push("a1", "day2-b\n", 1);
        let c2 = buf
            .drain_for_flush("a1", fixed_time("2026-05-28T09:00:00Z"))
            .unwrap();
        assert_eq!(c2.chunk_no, 2);
    }

    #[test]
    fn drain_returns_none_for_unregistered_or_empty() {
        let buf = ChatBuffer::new();
        let now = fixed_time("2026-05-27T12:30:00Z");
        assert!(buf.drain_for_flush("ghost", now).is_none());
        buf.register_agent("a1", "ws", "n");
        assert!(buf.drain_for_flush("a1", now).is_none()); // registered but empty
    }

    #[test]
    fn render_chunk_body_appends_section() {
        let chunk = DrainedChunk {
            workspace_id: "ws".into(),
            agent_name: "n".into(),
            date: "2026-05-27".into(),
            chunk_no: 2,
            text: "line a\nline b\n".into(),
        };
        let now = Utc.with_ymd_and_hms(2026, 5, 27, 12, 34, 56).unwrap();
        let body = render_chunk_body(
            "# header\n\n## chunk 1 — 11:00:00 UTC\n\n```\nold\n```\n",
            &chunk,
            now,
        );
        assert!(body.contains("## chunk 1 — 11:00:00 UTC"));
        assert!(body.contains("## chunk 2 — 12:34:56 UTC"));
        assert!(body.contains("line a"));
        assert!(body.contains("line b"));
    }
}
