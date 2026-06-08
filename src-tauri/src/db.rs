use crate::error::{Error, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::path::PathBuf;

pub type DbPool = Pool<SqliteConnectionManager>;

/// Run a blocking closure on the tokio blocking pool, returning its
/// result as an `async` future. Use this for any synchronous I/O (rusqlite,
/// std::fs, regex) called from an `async fn` — leaving such calls inline
/// parks the entire multi-threaded runtime on the current worker thread,
/// which freezes the Tauri event loop and any other in-flight commands.
///
/// On a sync context (no tokio runtime, e.g. watcher / architect threads)
/// the closure is invoked inline.
pub async fn spawn_blocking<F, R>(f: F) -> Result<R>
where
    F: FnOnce() -> Result<R> + Send + 'static,
    R: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(_) => match tokio::task::spawn_blocking(move || f()).await {
            Ok(r) => r,
            Err(e) => Err(Error::Other(format!("blocking task join: {}", e))),
        },
        Err(_) => f(),
    }
}

/// Resolve the path to the SQLite database (creating parent dirs).
pub fn db_path() -> Result<PathBuf> {
    let base = dirs::config_dir().ok_or_else(|| Error::Other("config dir unavailable".into()))?;
    let dir = base.join("pigide");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("db.sqlite"))
}

pub fn init_pool() -> Result<DbPool> {
    let path = db_path()?;
    tracing::info!("opening sqlite at {}", path.display());

    // Run migrations on a single, dedicated connection BEFORE building the pool
    // — r2d2 with `with_init` can race the WAL switch when several connections
    // come up at once.
    {
        let conn = rusqlite::Connection::open(&path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )?;
        migrate_one(&conn)?;
    }

    let manager = SqliteConnectionManager::file(&path).with_init(|c| {
        c.execute_batch(
            // WAL + foreign keys + 5s busy-wait for the multi-writer case.
            // `synchronous=NORMAL` is the sweet spot: durable across
            // application crashes (not power loss — WAL is replayed),
            // and ~10x faster than FULL on fsync-heavy workloads.
            // `temp_store=MEMORY` keeps large sorts inside the process.
            // `cache_size` is in KiB; -64000 = 64 MiB.
            // `mmap_size` enables memory-mapped I/O for read-heavy paths
            // (memory_fts, agents list) on Linux.
            "PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             PRAGMA cache_size = -64000;
             PRAGMA mmap_size = 268435456;",
        )
    });
    // Keep at least 1 connection warm so the first command after idle
    // doesn't pay the connection-establish cost (rusqlite is cheap to
    // init, but the WAL checkpoint + mmap handshake on cold start is
    // ~10ms — visible on a 240 FPS UI as a microstutter).
    let pool = Pool::builder()
        .max_size(8)
        .min_idle(Some(1))
        .build(manager)?;
    Ok(pool)
}

/// Idempotent schema migration on a single connection.
pub(crate) fn migrate_one(conn: &rusqlite::Connection) -> Result<()> {
    let current: i64 = conn
        .query_row("PRAGMA user_version;", [], |r| r.get(0))
        .unwrap_or(0);
    let target = 18;
    if current >= target {
        // Self-heal: an earlier build bumped user_version to 16+ without
        // actually applying the ALTER TABLE statements for memory_notes.
        // Re-add the columns/index defensively — all idempotent.
        heal_memory_notes_kind(conn)?;
        heal_memory_fts(conn)?;
        return Ok(());
    }
    if current < 1 {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS workspaces (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                layout_json TEXT NOT NULL DEFAULT '{\"type\":\"empty\"}',
                paths_json  TEXT NOT NULL DEFAULT '[]'
             );

             CREATE TABLE IF NOT EXISTS agents (
                id           TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                type         TEXT NOT NULL,
                cwd          TEXT,
                status       TEXT NOT NULL DEFAULT 'exited',
                created_at   TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_agents_ws ON agents(workspace_id);

             CREATE TABLE IF NOT EXISTS chat_messages (
                id              TEXT PRIMARY KEY,
                workspace_id    TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                role            TEXT NOT NULL,
                content         TEXT NOT NULL,
                tool_calls_json TEXT,
                tool_call_id    TEXT,
                created_at      TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_chat_ws ON chat_messages(workspace_id);

             CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
             );
             COMMIT;",
        )?;
    }
    if current < 2 {
        // Global orchestrator chat (not tied to any workspace).
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS orchestrator_chat (
                id              TEXT PRIMARY KEY,
                role            TEXT NOT NULL,
                content         TEXT NOT NULL,
                tool_calls_json TEXT,
                tool_call_id    TEXT,
                created_at      TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_orch_chat_created
                ON orchestrator_chat(created_at);
             COMMIT;",
        )?;
    }
    if current < 3 {
        // The original per-workspace chat_messages table is unused — the
        // orchestrator chat is global. Drop the dead schema.
        conn.execute_batch(
            "BEGIN;
             DROP INDEX IF EXISTS idx_chat_ws;
             DROP TABLE IF EXISTS chat_messages;
             COMMIT;",
        )?;
    }
    if current < 4 {
        // Tasks: first-class units of work.
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS tasks (
                id            TEXT PRIMARY KEY,
                workspace_id  TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                agent_id      TEXT REFERENCES agents(id) ON DELETE SET NULL,
                parent_id     TEXT REFERENCES tasks(id) ON DELETE SET NULL,
                title         TEXT NOT NULL,
                instructions  TEXT NOT NULL DEFAULT '',
                knowledge     TEXT NOT NULL DEFAULT '',
                status        TEXT NOT NULL DEFAULT 'todo'
                              CHECK(status IN
                                ('todo','in_progress','in_review','complete','cancelled')),
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_tasks_ws     ON tasks(workspace_id);
             CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
             CREATE INDEX IF NOT EXISTS idx_tasks_agent  ON tasks(agent_id);
             COMMIT;",
        )?;
    }
    if current < 5 {
        // PigMemory: notes + wikilink edges + FTS5 full-text index.
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS memory_notes (
                id              TEXT PRIMARY KEY,
                workspace_root  TEXT NOT NULL,
                slug            TEXT NOT NULL,
                title           TEXT NOT NULL,
                path            TEXT NOT NULL,
                tags_json       TEXT NOT NULL DEFAULT '[]',
                aliases_json    TEXT NOT NULL DEFAULT '[]',
                body            TEXT NOT NULL,
                mtime           INTEGER NOT NULL,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL,
                UNIQUE(workspace_root, slug)
             );
             CREATE INDEX IF NOT EXISTS idx_notes_root ON memory_notes(workspace_root);

             CREATE TABLE IF NOT EXISTS memory_links (
                src_id      TEXT NOT NULL REFERENCES memory_notes(id) ON DELETE CASCADE,
                dst_id      TEXT,
                dst_text    TEXT NOT NULL,
                display     TEXT,
                ambiguous   INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(src_id, dst_text)
             );
             CREATE INDEX IF NOT EXISTS idx_links_dst ON memory_links(dst_id);

             CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
                title, body, tags, aliases,
                content='memory_notes', content_rowid='rowid',
                tokenize='unicode61 remove_diacritics 2'
             );

             -- Keep FTS5 in sync with the content table.
             CREATE TRIGGER IF NOT EXISTS memory_notes_ai AFTER INSERT ON memory_notes BEGIN
               INSERT INTO memory_fts(rowid, title, body, tags, aliases)
               VALUES (new.rowid, new.title, new.body, new.tags_json, new.aliases_json);
             END;
             CREATE TRIGGER IF NOT EXISTS memory_notes_ad AFTER DELETE ON memory_notes BEGIN
               INSERT INTO memory_fts(memory_fts, rowid, title, body, tags, aliases)
               VALUES('delete', old.rowid, old.title, old.body, old.tags_json, old.aliases_json);
             END;
             CREATE TRIGGER IF NOT EXISTS memory_notes_au AFTER UPDATE ON memory_notes BEGIN
               INSERT INTO memory_fts(memory_fts, rowid, title, body, tags, aliases)
               VALUES('delete', old.rowid, old.title, old.body, old.tags_json, old.aliases_json);
               INSERT INTO memory_fts(rowid, title, body, tags, aliases)
               VALUES (new.rowid, new.title, new.body, new.tags_json, new.aliases_json);
             END;
             COMMIT;",
        )?;
    }
    if current < 6 {
        // PigVoice: per-user dictionary of word-boundary replacements +
        // transcription history with FTS5.
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS voice_dictionary (
                id            TEXT PRIMARY KEY,
                pattern       TEXT NOT NULL,
                replacement   TEXT NOT NULL,
                case_sense    INTEGER NOT NULL DEFAULT 0,
                enabled       INTEGER NOT NULL DEFAULT 1,
                created_at    TEXT NOT NULL
             );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_voice_dict_pattern
                ON voice_dictionary(pattern, case_sense);

             CREATE TABLE IF NOT EXISTS voice_transcripts (
                id            TEXT PRIMARY KEY,
                text          TEXT NOT NULL,
                text_raw      TEXT NOT NULL,
                language      TEXT,
                model_id      TEXT NOT NULL,
                source        TEXT NOT NULL,
                duration_ms   INTEGER NOT NULL,
                word_count    INTEGER NOT NULL,
                created_at    TEXT NOT NULL,
                injected      INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS idx_voice_tr_created
                ON voice_transcripts(created_at DESC);

             CREATE VIRTUAL TABLE IF NOT EXISTS voice_transcripts_fts
                USING fts5(
                    text,
                    content='voice_transcripts',
                    content_rowid='rowid'
                );
             CREATE TRIGGER IF NOT EXISTS voice_tr_ai
                AFTER INSERT ON voice_transcripts BEGIN
                    INSERT INTO voice_transcripts_fts(rowid, text)
                    VALUES (new.rowid, new.text);
                END;
             CREATE TRIGGER IF NOT EXISTS voice_tr_ad
                AFTER DELETE ON voice_transcripts BEGIN
                    INSERT INTO voice_transcripts_fts(voice_transcripts_fts, rowid, text)
                    VALUES('delete', old.rowid, old.text);
                END;
             CREATE TRIGGER IF NOT EXISTS voice_tr_au
                AFTER UPDATE ON voice_transcripts BEGIN
                    INSERT INTO voice_transcripts_fts(voice_transcripts_fts, rowid, text)
                    VALUES('delete', old.rowid, old.text);
                    INSERT INTO voice_transcripts_fts(rowid, text)
                    VALUES (new.rowid, new.text);
                END;
             COMMIT;",
        )?;
    }
    if current < 7 {
        // PigSwarm: roles + mailbox + side-chat threads + roll-call.
        conn.execute_batch(
            "BEGIN;
             ALTER TABLE agents ADD COLUMN role TEXT NOT NULL DEFAULT 'builder'
                CHECK(role IN ('coordinator','builder','reviewer','scout'));
             CREATE INDEX IF NOT EXISTS idx_agents_role ON agents(role);

             CREATE TABLE IF NOT EXISTS mailbox (
                id            TEXT PRIMARY KEY,
                from_agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
                to_addr       TEXT NOT NULL,
                body          TEXT NOT NULL,
                thread_id     TEXT,
                created_at    TEXT NOT NULL,
                read_at       TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_mbox_to_unread
                ON mailbox(to_addr, read_at);
             CREATE INDEX IF NOT EXISTS idx_mbox_thread ON mailbox(thread_id);

             CREATE TABLE IF NOT EXISTS rollcalls (
                id          TEXT PRIMARY KEY,
                role        TEXT NOT NULL,
                prompt      TEXT NOT NULL,
                created_at  TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS rollcall_responses (
                rollcall_id  TEXT NOT NULL REFERENCES rollcalls(id) ON DELETE CASCADE,
                agent_id     TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
                body         TEXT NOT NULL,
                created_at   TEXT NOT NULL,
                PRIMARY KEY(rollcall_id, agent_id)
             );
             COMMIT;",
        )?;
    }
    if current < 8 {
        // PigMCP: api keys + audit log.
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS mcp_api_keys (
                id           TEXT PRIMARY KEY,
                label        TEXT NOT NULL,
                key_hash     TEXT NOT NULL,
                scopes       TEXT NOT NULL DEFAULT 'read,mutate',
                created_at   TEXT NOT NULL,
                last_used_at TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_mcp_keys_hash ON mcp_api_keys(key_hash);

             CREATE TABLE IF NOT EXISTS mcp_audit (
                id            TEXT PRIMARY KEY,
                key_id        TEXT,
                tool          TEXT NOT NULL,
                args_json     TEXT,
                result_status TEXT NOT NULL,
                created_at    TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_mcp_audit_created
                ON mcp_audit(created_at DESC);
             COMMIT;",
        )?;
    }
    if current < 9 {
        // Multi-chat sessions: each chat has a session_id. Existing rows
        // get migrated into a default "Main" session so history is not lost.
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS chat_sessions (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL
             );
             ALTER TABLE orchestrator_chat ADD COLUMN session_id TEXT
                 REFERENCES chat_sessions(id) ON DELETE CASCADE;
             CREATE INDEX IF NOT EXISTS idx_orch_chat_session
                ON orchestrator_chat(session_id, created_at);
             COMMIT;",
        )?;
        // Backfill: seed a default session and adopt all existing rows.
        let default_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO chat_sessions(id,name,created_at,updated_at) VALUES(?1,?2,?3,?3)",
            rusqlite::params![&default_id, "Main", &now],
        )?;
        conn.execute(
            "UPDATE orchestrator_chat SET session_id=?1 WHERE session_id IS NULL",
            [&default_id],
        )?;
    }
    if current < 10 {
        // PigSwarm extras + cross-cutting tables ported from BridgeSpace 3:
        //   - file_ownership: exclusive per-task lock on a workspace-relative path
        //   - review_gates:   PASS/FAIL gate that blocks task completion
        //   - prompts:        reusable prompt library (#18)
        //   - role_prompts:   custom system prompts per role/workspace (#19)
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS file_ownership (
                workspace_id TEXT NOT NULL,
                path         TEXT NOT NULL,
                task_id      TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                agent_id     TEXT REFERENCES agents(id) ON DELETE SET NULL,
                acquired_at  TEXT NOT NULL,
                PRIMARY KEY (workspace_id, path)
             );
             CREATE INDEX IF NOT EXISTS idx_fown_task ON file_ownership(task_id);

             CREATE TABLE IF NOT EXISTS review_gates (
                id            TEXT PRIMARY KEY,
                task_id       TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                reviewer_id   TEXT REFERENCES agents(id) ON DELETE SET NULL,
                verdict       TEXT NOT NULL DEFAULT 'pending'
                              CHECK(verdict IN ('pending','pass','fail')),
                reason        TEXT NOT NULL DEFAULT '',
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_review_task ON review_gates(task_id);

             CREATE TABLE IF NOT EXISTS prompts (
                id            TEXT PRIMARY KEY,
                workspace_id  TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
                name          TEXT NOT NULL,
                body          TEXT NOT NULL,
                tags_json     TEXT NOT NULL DEFAULT '[]',
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_prompts_ws ON prompts(workspace_id);
             CREATE UNIQUE INDEX IF NOT EXISTS idx_prompts_ws_name
                ON prompts(COALESCE(workspace_id,''), name);

             CREATE TABLE IF NOT EXISTS role_prompts (
                workspace_id TEXT NOT NULL,
                agent_type   TEXT NOT NULL DEFAULT '',
                role         TEXT NOT NULL,
                prompt       TEXT NOT NULL,
                updated_at   TEXT NOT NULL,
                PRIMARY KEY (workspace_id, agent_type, role)
             );
             COMMIT;",
        )?;
    }
    if current < 11 {
        // SSH presets (#14): named connection profiles for the `ssh` agent
        // type. The `args_json` column holds the full argv we pass to ssh
        // (host, -p PORT, -i KEY, -L FORWARD…) so the user can shape the
        // command exactly without us having to enumerate every ssh flag.
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS ssh_presets (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL UNIQUE,
                host        TEXT NOT NULL,
                user        TEXT,
                port        INTEGER,
                identity    TEXT,
                args_json   TEXT NOT NULL DEFAULT '[]',
                cwd         TEXT,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL
             );
             COMMIT;",
        )?;
    }
    if current < 12 {
        // Chat message queue: persists user messages waiting for the
        // orchestrator to free up. See `chat_queue.rs` for the contract.
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS chat_queue (
                id          TEXT PRIMARY KEY,
                session_id  TEXT NOT NULL,
                text        TEXT NOT NULL,
                status      TEXT NOT NULL DEFAULT 'queued',
                position    INTEGER NOT NULL,
                created_at  TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_chat_queue_session_pos
                ON chat_queue(session_id, position);
             CREATE INDEX IF NOT EXISTS idx_chat_queue_status
                ON chat_queue(status);
             COMMIT;",
        )?;
    }
    if current < 13 {
        // @-mention path attachments (Architect chat). Stored as a
        // JSON-encoded `Vec<Attachment>` alongside the queued message so
        // the orchestrator can surface them in `[WORLD STATE]` for the
        // turn that consumes the row. NULL/missing → no attachments
        // (backwards-compatible with v12 rows).
        conn.execute_batch(
            "BEGIN;
             ALTER TABLE chat_queue ADD COLUMN attachments_json TEXT;
             COMMIT;",
        )?;
    }
    if current < 14 {
        // Fix FTS5 column name mismatch: FTS columns must match content table
        // column names for snippet()/highlight() to work. Previously FTS had
        // `tags, aliases` but content table has `tags_json, aliases_json`.
        conn.execute_batch(
            "BEGIN;
             DROP TRIGGER IF EXISTS memory_notes_ai;
             DROP TRIGGER IF EXISTS memory_notes_ad;
             DROP TRIGGER IF EXISTS memory_notes_au;
             DROP TABLE IF EXISTS memory_fts;

             CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
                title, body, tags_json, aliases_json,
                content='memory_notes', content_rowid='rowid',
                tokenize='unicode61 remove_diacritics 2'
             );

             CREATE TRIGGER IF NOT EXISTS memory_notes_ai AFTER INSERT ON memory_notes BEGIN
               INSERT INTO memory_fts(rowid, title, body, tags_json, aliases_json)
               VALUES (new.rowid, new.title, new.body, new.tags_json, new.aliases_json);
             END;
             CREATE TRIGGER IF NOT EXISTS memory_notes_ad AFTER DELETE ON memory_notes BEGIN
               INSERT INTO memory_fts(memory_fts, rowid, title, body, tags_json, aliases_json)
               VALUES('delete', old.rowid, old.title, old.body, old.tags_json, old.aliases_json);
             END;
             CREATE TRIGGER IF NOT EXISTS memory_notes_au AFTER UPDATE ON memory_notes BEGIN
               INSERT INTO memory_fts(memory_fts, rowid, title, body, tags_json, aliases_json)
               VALUES('delete', old.rowid, old.title, old.body, old.tags_json, old.aliases_json);
               INSERT INTO memory_fts(rowid, title, body, tags_json, aliases_json)
               VALUES (new.rowid, new.title, new.body, new.tags_json, new.aliases_json);
             END;

             INSERT INTO memory_fts(memory_fts) VALUES('rebuild');
             COMMIT;",
        )?;
    }
    if current < 15 {
        // Per-workspace chat scope: each chat_sessions row can be either
        // global (workspace_id IS NULL — back-compat with v9) or scoped to
        // a specific workspace (workspace_id NOT NULL, cascade-deleted with
        // its workspace). Existing rows stay global by leaving the column
        // NULL.
        conn.execute_batch(
            "BEGIN;
             ALTER TABLE chat_sessions ADD COLUMN workspace_id TEXT
                 REFERENCES workspaces(id) ON DELETE CASCADE;
             CREATE INDEX IF NOT EXISTS idx_chat_sessions_ws
                ON chat_sessions(workspace_id);
             COMMIT;",
        )?;
    }
    if current < 16 {
        // PigMemory Phase 0: add `kind` and `ingest_json` columns to
        // memory_notes. Existing rows get kind='source' so legacy notes
        // are still findable. ingest_json defaults NULL (only set for
        // notes written by the ingest pipeline).
        conn.execute_batch(
            "BEGIN;
             ALTER TABLE memory_notes ADD COLUMN kind TEXT NOT NULL DEFAULT 'source';
             ALTER TABLE memory_notes ADD COLUMN ingest_json TEXT;
             CREATE INDEX IF NOT EXISTS idx_notes_kind ON memory_notes(workspace_root, kind);
             COMMIT;",
        )?;
    }
    if current < 17 {
        // PigMemory Phase 2: queue of items the smart-lane worker should
        // enrich with concepts/entities. Populated by the fast-lane on
        // task→complete and chat-rotation; drained by smart::SmartIngestWorker.
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS ingest_queue (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                workspace_id    TEXT    NOT NULL,
                kind            TEXT    NOT NULL,
                payload_json    TEXT    NOT NULL,
                created_at      TEXT    NOT NULL,
                processed_at    TEXT,
                last_error      TEXT,
                smart_attempts  INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS idx_ingest_pending
                ON ingest_queue(workspace_id, processed_at);
             COMMIT;",
        )?;
    }
    if current < 18 {
        // Custom API providers: user-registered OpenAI-compatible / Anthropic
        // endpoints. The API key is NOT stored here — it lives in the
        // encrypted secret store (crate::secrets), keyed by provider id.
        // `model` is the user-selected model used by the gateway.
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS custom_providers (
                id          TEXT PRIMARY KEY,
                label       TEXT NOT NULL,
                kind        TEXT NOT NULL,
                base_url    TEXT NOT NULL,
                model       TEXT NOT NULL DEFAULT '',
                created_at  TEXT NOT NULL
             );
             COMMIT;",
        )?;
    }
    conn.pragma_update(None, "user_version", target)?;
    Ok(())
}

/// Defensive backfill for v16: some installs bumped user_version without
/// applying the ALTER TABLE statements (the batch failed mid-flight on a
/// stale schema). Re-running ALTER for an existing column raises a
/// "duplicate column name" error, so probe the schema first.
fn heal_memory_notes_kind(conn: &rusqlite::Connection) -> Result<()> {
    let has_kind: bool = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('memory_notes') WHERE name='kind'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|_| true)
        .unwrap_or(false);
    let has_ingest: bool = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('memory_notes') WHERE name='ingest_json'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|_| true)
        .unwrap_or(false);
    if !has_kind {
        conn.execute_batch(
            "ALTER TABLE memory_notes ADD COLUMN kind TEXT NOT NULL DEFAULT 'source';",
        )?;
    }
    if !has_ingest {
        conn.execute_batch("ALTER TABLE memory_notes ADD COLUMN ingest_json TEXT;")?;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_notes_kind ON memory_notes(workspace_root, kind);",
    )?;
    Ok(())
}

/// Defensive backfill for v14: some installs bumped user_version past 14
/// without applying the FTS5 column rename, so `memory_fts` still has
/// columns `tags, aliases` while `memory_notes` has `tags_json,
/// aliases_json`. Any FTS read that touches the content table (count,
/// snippet on tag/alias columns, integrity-check, rebuild from triggers
/// firing on UPDATE/DELETE) then errors with "no such column: T.tags".
/// Probe the FTS schema and rebuild it when the column names are wrong.
fn heal_memory_fts(conn: &rusqlite::Connection) -> Result<()> {
    let has_tags_json: bool = conn
        .query_row(
            "SELECT 1 FROM pragma_table_xinfo('memory_fts') WHERE name='tags_json'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|_| true)
        .unwrap_or(false);
    if has_tags_json {
        return Ok(());
    }
    tracing::warn!(
        "memory_fts has stale columns (tags/aliases); rebuilding with tags_json/aliases_json"
    );
    conn.execute_batch(
        "BEGIN;
         DROP TRIGGER IF EXISTS memory_notes_ai;
         DROP TRIGGER IF EXISTS memory_notes_ad;
         DROP TRIGGER IF EXISTS memory_notes_au;
         DROP TABLE IF EXISTS memory_fts;

         CREATE VIRTUAL TABLE memory_fts USING fts5(
            title, body, tags_json, aliases_json,
            content='memory_notes', content_rowid='rowid',
            tokenize='unicode61 remove_diacritics 2'
         );

         CREATE TRIGGER memory_notes_ai AFTER INSERT ON memory_notes BEGIN
           INSERT INTO memory_fts(rowid, title, body, tags_json, aliases_json)
           VALUES (new.rowid, new.title, new.body, new.tags_json, new.aliases_json);
         END;
         CREATE TRIGGER memory_notes_ad AFTER DELETE ON memory_notes BEGIN
           INSERT INTO memory_fts(memory_fts, rowid, title, body, tags_json, aliases_json)
           VALUES('delete', old.rowid, old.title, old.body, old.tags_json, old.aliases_json);
         END;
         CREATE TRIGGER memory_notes_au AFTER UPDATE ON memory_notes BEGIN
           INSERT INTO memory_fts(memory_fts, rowid, title, body, tags_json, aliases_json)
           VALUES('delete', old.rowid, old.title, old.body, old.tags_json, old.aliases_json);
           INSERT INTO memory_fts(rowid, title, body, tags_json, aliases_json)
           VALUES (new.rowid, new.title, new.body, new.tags_json, new.aliases_json);
         END;

         INSERT INTO memory_fts(memory_fts) VALUES('rebuild');
         COMMIT;",
    )?;
    Ok(())
}

/// Tiny KV settings helpers.
pub fn get_setting(pool: &DbPool, key: &str) -> Result<Option<String>> {
    let conn = pool.get()?;
    let mut stmt = conn.prepare("SELECT value FROM settings WHERE key = ?1")?;
    let mut rows = stmt.query([key])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row.get(0)?))
    } else {
        Ok(None)
    }
}

pub fn set_setting(pool: &DbPool, key: &str, value: &str) -> Result<()> {
    let conn = pool.get()?;
    conn.execute(
        "INSERT INTO settings(key,value) VALUES(?1,?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [key, value],
    )?;
    Ok(())
}

/// A user-registered custom API provider row. The API key is NOT here — it
/// lives in the encrypted secret store keyed by `id`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CustomProviderRow {
    pub id: String,
    pub label: String,
    /// "openai" | "anthropic".
    pub kind: String,
    pub base_url: String,
    /// User-selected model used by the gateway. Empty until chosen.
    pub model: String,
    pub created_at: String,
}

fn row_to_provider(row: &rusqlite::Row) -> rusqlite::Result<CustomProviderRow> {
    Ok(CustomProviderRow {
        id: row.get(0)?,
        label: row.get(1)?,
        kind: row.get(2)?,
        base_url: row.get(3)?,
        model: row.get(4)?,
        created_at: row.get(5)?,
    })
}

pub fn list_custom_providers(pool: &DbPool) -> Result<Vec<CustomProviderRow>> {
    let conn = pool.get()?;
    let mut stmt = conn.prepare(
        "SELECT id, label, kind, base_url, model, created_at
         FROM custom_providers ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([], row_to_provider)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub fn get_custom_provider(pool: &DbPool, id: &str) -> Result<Option<CustomProviderRow>> {
    let conn = pool.get()?;
    let mut stmt = conn.prepare(
        "SELECT id, label, kind, base_url, model, created_at
         FROM custom_providers WHERE id = ?1",
    )?;
    let mut rows = stmt.query([id])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row_to_provider(row)?))
    } else {
        Ok(None)
    }
}

pub fn insert_custom_provider(pool: &DbPool, p: &CustomProviderRow) -> Result<()> {
    let conn = pool.get()?;
    conn.execute(
        "INSERT INTO custom_providers(id,label,kind,base_url,model,created_at)
         VALUES(?1,?2,?3,?4,?5,?6)",
        rusqlite::params![p.id, p.label, p.kind, p.base_url, p.model, p.created_at],
    )?;
    Ok(())
}

/// Update the mutable fields (label, base_url, model). `kind` and `id` are
/// immutable once created.
pub fn update_custom_provider(
    pool: &DbPool,
    id: &str,
    label: &str,
    base_url: &str,
    model: &str,
) -> Result<()> {
    let conn = pool.get()?;
    conn.execute(
        "UPDATE custom_providers SET label=?2, base_url=?3, model=?4 WHERE id=?1",
        rusqlite::params![id, label, base_url, model],
    )?;
    Ok(())
}

pub fn set_custom_provider_model(pool: &DbPool, id: &str, model: &str) -> Result<()> {
    let conn = pool.get()?;
    conn.execute(
        "UPDATE custom_providers SET model=?2 WHERE id=?1",
        rusqlite::params![id, model],
    )?;
    Ok(())
}

pub fn delete_custom_provider(pool: &DbPool, id: &str) -> Result<()> {
    let conn = pool.get()?;
    conn.execute("DELETE FROM custom_providers WHERE id = ?1", [id])?;
    Ok(())
}
