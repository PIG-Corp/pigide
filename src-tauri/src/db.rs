use crate::error::{Error, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::path::PathBuf;

pub type DbPool = Pool<SqliteConnectionManager>;

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
            "PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )
    });
    let pool = Pool::builder().max_size(8).build(manager)?;
    Ok(pool)
}

/// Idempotent schema migration on a single connection.
fn migrate_one(conn: &rusqlite::Connection) -> Result<()> {
    let current: i64 =
        conn.query_row("PRAGMA user_version;", [], |r| r.get(0)).unwrap_or(0);
    let target = 12;
    if current >= target {
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
    conn.pragma_update(None, "user_version", target)?;
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
