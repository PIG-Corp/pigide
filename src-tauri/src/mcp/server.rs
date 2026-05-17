//! PigMCP HTTP/JSON-RPC server.
//!
//! Single endpoint `POST /mcp`. Bearer auth via `Authorization` header or
//! `?apiKey=` query. Methods:
//!   - `initialize` — returns server info + capabilities.
//!   - `tools/list` — returns the orchestrator tool registry as MCP-shaped tools.
//!   - `tools/call` — runs a single tool through the orchestrator dispatcher.
//!
//! Dangerous tools (spawn_agent, send_to_agent, delete_workspace,
//! delete_memory) require the `dangerous` scope.

use crate::agent::AgentManager;
use crate::db::DbPool;
use crate::error::Result;
use crate::mcp::auth::{self, KeyInfo};
use crate::memory::MemoryService;
use crate::orchestrator::tools as orch_tools;
use crate::tasks::TaskManager;
use crate::workspace::WorkspaceManager;
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::task::JoinHandle;
use uuid::Uuid;

const PIGIDE_DEVELOPER_GUIDE: &str = include_str!("prompts/pigide_developer_guide.md");

/// Tools that mutate state and so require the `mutate` scope at minimum.
fn is_mutating(name: &str) -> bool {
    matches!(
        name,
        "create_workspace"
            | "switch_workspace"
            | "delete_workspace"
            | "spawn_agent"
            | "close_agent"
            | "send_to_agent"
            | "create_task"
            | "update_task"
            | "delete_task"
            | "assign_task_to_agent"
            | "create_memory"
            | "update_memory"
            | "delete_memory"
            | "send_mail"
            | "broadcast"
            | "mark_mail_read"
            | "start_rollcall"
    )
}

/// Tools whose blast radius is large enough to require the `dangerous` scope.
fn is_dangerous(name: &str) -> bool {
    matches!(
        name,
        "spawn_agent"
            | "send_to_agent"
            | "delete_workspace"
            | "delete_memory"
            | "delete_task"
    )
}

#[derive(Clone)]
pub struct McpState {
    pub db: DbPool,
    pub ws_mgr: Arc<WorkspaceManager>,
    pub agent_mgr: Arc<AgentManager>,
    pub task_mgr: Arc<TaskManager>,
    pub memory: Arc<MemoryService>,
    pub resolver: Arc<crate::project_resolver::ResolverService>,
}

#[derive(Default)]
pub struct McpServerHandle {
    pub running: Mutex<Option<RunningHandle>>,
}

pub struct RunningHandle {
    pub addr: std::net::SocketAddr,
    pub join: JoinHandle<()>,
    pub shutdown: tokio::sync::oneshot::Sender<()>,
}

impl McpServerHandle {
    pub fn is_running(&self) -> bool {
        self.running.lock().is_some()
    }
    pub fn current_addr(&self) -> Option<std::net::SocketAddr> {
        self.running.lock().as_ref().map(|h| h.addr)
    }
    pub fn stop(&self) {
        if let Some(rh) = self.running.lock().take() {
            let _ = rh.shutdown.send(());
            rh.join.abort();
        }
    }
}

/// Spawn the MCP server. Idempotent: if already running, returns current addr.
pub async fn start(
    handle: Arc<McpServerHandle>,
    state: McpState,
    bind: std::net::SocketAddr,
) -> Result<std::net::SocketAddr> {
    if let Some(addr) = handle.current_addr() {
        return Ok(addr);
    }
    let app = Router::new()
        .route("/mcp", post(handle_rpc))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(bind).await.map_err(|e| {
        crate::error::Error::Other(format!("MCP bind {}: {}", bind, e))
    })?;
    let addr = listener.local_addr().map_err(|e| {
        crate::error::Error::Other(format!("MCP local_addr: {}", e))
    })?;
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let join = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await;
    });
    *handle.running.lock() = Some(RunningHandle {
        addr,
        join,
        shutdown: tx,
    });
    tracing::info!("MCP server listening on {}", addr);
    Ok(addr)
}

#[derive(Deserialize)]
struct AuthQuery {
    #[serde(rename = "apiKey")]
    api_key: Option<String>,
}

#[derive(Deserialize)]
struct JsonRpcRequest {
    #[serde(default)]
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

fn extract_token(headers: &HeaderMap, q: &AuthQuery) -> Option<String> {
    if let Some(h) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        if let Some(rest) = h.strip_prefix("Bearer ").or_else(|| h.strip_prefix("bearer ")) {
            return Some(rest.trim().to_string());
        }
    }
    q.api_key.clone()
}

async fn handle_rpc(
    State(state): State<McpState>,
    Query(q): Query<AuthQuery>,
    headers: HeaderMap,
    Json(req): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    if req.jsonrpc != "2.0" && !req.jsonrpc.is_empty() {
        return error_response(req.id, -32600, "invalid jsonrpc version");
    }

    // Auth: every method except `initialize` requires a key.
    let presented = extract_token(&headers, &q);
    let key: Option<KeyInfo> = match presented {
        Some(t) => match auth::validate(&state.db, &t) {
            Ok(k) => k,
            Err(e) => return error_response(req.id, -32000, &format!("auth error: {}", e)),
        },
        None => None,
    };

    if req.method != "initialize" && key.is_none() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "jsonrpc": "2.0",
                "id": req.id,
                "error": {"code": -32001, "message": "missing or invalid api key"}
            })),
        )
            .into_response();
    }

    let result_value: std::result::Result<Value, (i64, String)> = match req.method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {"listChanged": false},
                "prompts": {"listChanged": false}
            },
            "serverInfo": {"name": "pigide", "version": env!("CARGO_PKG_VERSION")}
        })),
        "tools/list" => Ok(tools_list_response()),
        "tools/call" => match dispatch_tool(state.clone(), key.clone(), req.params).await {
            Ok(v) => Ok(v),
            Err((code, msg)) => Err((code, msg)),
        },
        "prompts/list" => Ok(json!({
            "prompts": [{
                "name": "pigide_developer_guide",
                "description": "Onboarding guide for PigIDE workflows."
            }]
        })),
        "prompts/get" => {
            let name = req
                .params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if name == "pigide_developer_guide" {
                Ok(json!({
                    "description": "PigIDE developer guide",
                    "messages": [{
                        "role": "user",
                        "content": {"type": "text", "text": PIGIDE_DEVELOPER_GUIDE}
                    }]
                }))
            } else {
                Err((-32602, format!("unknown prompt: {}", name)))
            }
        }
        "ping" => Ok(json!({})),
        other => Err((-32601, format!("method not found: {}", other))),
    };

    match result_value {
        Ok(v) => Json(json!({
            "jsonrpc": "2.0",
            "id": req.id,
            "result": v
        }))
        .into_response(),
        Err((code, msg)) => error_response(req.id, code, &msg),
    }
}

fn error_response(id: Option<Value>, code: i64, msg: &str) -> axum::response::Response {
    Json(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": msg}
    }))
    .into_response()
}

fn tools_list_response() -> Value {
    let raw = orch_tools::tool_definitions();
    let mut tools = Vec::with_capacity(raw.len());
    for entry in raw {
        if let Some(f) = entry.get("function") {
            let name = f.get("name").cloned().unwrap_or(Value::Null);
            let desc = f.get("description").cloned().unwrap_or(Value::Null);
            let schema = f.get("parameters").cloned().unwrap_or(json!({}));
            tools.push(json!({
                "name": name,
                "description": desc,
                "inputSchema": schema
            }));
        }
    }
    json!({"tools": tools})
}

async fn dispatch_tool(
    state: McpState,
    key: Option<KeyInfo>,
    params: Value,
) -> std::result::Result<Value, (i64, String)> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or((-32602, "params.name required".into()))?
        .to_string();
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(Value::Object(Default::default()));

    // Scope check.
    let scopes = key.as_ref().map(|k| k.scopes.clone()).unwrap_or_default();
    if is_dangerous(&name) && !scopes.iter().any(|s| s == "dangerous") {
        audit(&state.db, key.as_ref(), &name, &arguments, "denied:scope");
        return Err((-32002, format!("scope `dangerous` required for {}", name)));
    }
    if is_mutating(&name) && !scopes.iter().any(|s| s == "mutate" || s == "dangerous") {
        audit(&state.db, key.as_ref(), &name, &arguments, "denied:scope");
        return Err((-32002, format!("scope `mutate` required for {}", name)));
    }

    let result = orch_tools::dispatch(
        &state.db,
        &state.ws_mgr,
        &state.agent_mgr,
        &state.task_mgr,
        &state.memory,
        &state.resolver,
        None, // no AppHandle — MCP server can't emit Tauri events directly
        &name,
        &arguments,
    )
    .await;

    match result {
        Ok(v) => {
            audit(&state.db, key.as_ref(), &name, &arguments, "ok");
            Ok(json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string(&v).unwrap_or_default()
                }],
                "isError": false
            }))
        }
        Err(e) => {
            let msg = e.to_string();
            audit(&state.db, key.as_ref(), &name, &arguments, &format!("err:{}", msg));
            Err((-32603, msg))
        }
    }
}

fn audit(db: &DbPool, key: Option<&KeyInfo>, tool: &str, args: &Value, status: &str) {
    let _ = (|| -> Result<()> {
        let conn = db.get()?;
        conn.execute(
            "INSERT INTO mcp_audit(id, key_id, tool, args_json, result_status, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                Uuid::new_v4().to_string(),
                key.map(|k| k.id.clone()),
                tool,
                serde_json::to_string(args).ok(),
                status,
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    })();
}

#[allow(dead_code)]
fn _hint_unused(_h: HashMap<String, String>) {}
