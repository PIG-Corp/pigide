use crate::db::DbPool;
use crate::error::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String, // "function"
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String, // raw JSON string per OpenAI spec
}

/// A single message in the global orchestrator chat.
///
/// Messages are scoped by `session_id`; sessions are managed in the
/// `chat_sessions` table. Migration v9 backfills existing rows into a
/// default "Main" session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub session_id: String,
    pub role: String, // "user" | "assistant" | "tool" | "system"
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    pub created_at: String,
}

impl ChatMessage {
    pub fn user(session_id: String, content: String) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            session_id,
            role: "user".into(),
            content,
            tool_calls: None,
            tool_call_id: None,
            created_at: Utc::now().to_rfc3339(),
        }
    }
    pub fn assistant(
        session_id: String,
        content: String,
        tool_calls: Option<Vec<ToolCall>>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            session_id,
            role: "assistant".into(),
            content,
            tool_calls,
            tool_call_id: None,
            created_at: Utc::now().to_rfc3339(),
        }
    }
    pub fn tool(session_id: String, tool_call_id: &str, content: String) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            session_id,
            role: "tool".into(),
            content,
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_string()),
            created_at: Utc::now().to_rfc3339(),
        }
    }
    pub fn system(session_id: String, content: String) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            session_id,
            role: "system".into(),
            content,
            tool_calls: None,
            tool_call_id: None,
            created_at: Utc::now().to_rfc3339(),
        }
    }
}

pub fn list(db: &DbPool, session_id: &str, limit: i64) -> Result<Vec<ChatMessage>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id,session_id,role,content,tool_calls_json,tool_call_id,created_at
         FROM orchestrator_chat
         WHERE session_id = ?1
         ORDER BY created_at ASC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![session_id, limit], |r| {
        let tcj: Option<String> = r.get(4)?;
        let tool_calls = tcj.and_then(|s| serde_json::from_str::<Vec<ToolCall>>(&s).ok());
        Ok(ChatMessage {
            id: r.get(0)?,
            session_id: r.get(1)?,
            role: r.get(2)?,
            content: r.get(3)?,
            tool_calls,
            tool_call_id: r.get(5)?,
            created_at: r.get(6)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn insert(db: &DbPool, m: &ChatMessage) -> Result<()> {
    let tcj: Option<String> = match &m.tool_calls {
        Some(tc) => Some(serde_json::to_string(tc)?),
        None => None,
    };
    let conn = db.get()?;
    conn.execute(
        "INSERT INTO orchestrator_chat(id,session_id,role,content,tool_calls_json,tool_call_id,created_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7)",
        rusqlite::params![
            &m.id,
            &m.session_id,
            &m.role,
            &m.content,
            &tcj,
            &m.tool_call_id,
            &m.created_at
        ],
    )?;
    Ok(())
}

/// Delete every message in this session whose `created_at` is strictly
/// greater than `after`. Used to roll back partial tool turns when the
/// orchestrator hits an error mid-loop.
pub fn delete_after(db: &DbPool, session_id: &str, after: &str) -> Result<usize> {
    let conn = db.get()?;
    let n = conn.execute(
        "DELETE FROM orchestrator_chat WHERE session_id=?1 AND created_at>?2",
        rusqlite::params![session_id, after],
    )?;
    Ok(n)
}

/// Wipe orchestrator chat history for one session.
pub fn clear(db: &DbPool, session_id: &str) -> Result<()> {
    let conn = db.get()?;
    conn.execute(
        "DELETE FROM orchestrator_chat WHERE session_id=?1",
        [session_id],
    )?;
    Ok(())
}

/// Convert a stored ChatMessage into the OpenAI-compatible message JSON the
/// orchestrator sends back to the model.
pub fn to_api_message(m: &ChatMessage) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("role".into(), Value::String(m.role.clone()));

    // For assistant messages that ONLY carry tool calls, send content as null.
    // Anthropic-backed routers reject empty-string content here.
    let has_tool_calls = m
        .tool_calls
        .as_ref()
        .map(|t| !t.is_empty())
        .unwrap_or(false);
    let content_value = if m.role == "assistant" && m.content.is_empty() && has_tool_calls {
        Value::Null
    } else {
        Value::String(m.content.clone())
    };
    obj.insert("content".into(), content_value);

    if let Some(id) = &m.tool_call_id {
        obj.insert("tool_call_id".into(), Value::String(id.clone()));
    }
    if has_tool_calls {
        if let Ok(v) = serde_json::to_value(m.tool_calls.as_ref().unwrap()) {
            obj.insert("tool_calls".into(), v);
        }
    }
    Value::Object(obj)
}
