use crate::agent::{AgentManager, AgentType};
use crate::db;
use crate::error::{Error, Result};
use crate::events::{EV_LAYOUT_CHANGED, EV_WORKSPACE_CHANGED};
use crate::layout::LayoutNode;
use crate::memory::{self, MemoryService};
use crate::project_resolver::{ResolveStatus, ResolverService};
use crate::swarm;
use crate::tasks::{CreateTaskArgs, TaskManager, UpdateTaskArgs};
use crate::workspace::WorkspaceManager;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

/// Build the OpenAI-style tools array.
pub fn tool_definitions() -> Vec<Value> {
    let mut out = vec![
        function_tool(
            "list_workspaces",
            "List all workspaces with id, name, agent_count.",
            json!({ "type": "object", "properties": {}, "required": [] }),
        ),
        function_tool(
            "create_workspace",
            "Create a new workspace. Idempotent by name: if a workspace with the same name already exists it is reused (returned with existed=true) and made current — do NOT retry by appending suffixes like '-2'.",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "paths": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["name"]
            }),
        ),
        function_tool(
            "switch_workspace",
            "Switch the active workspace by id.",
            json!({
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"]
            }),
        ),
        function_tool(
            "delete_workspace",
            "Delete a workspace and all its agents.",
            json!({
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"]
            }),
        ),
        function_tool(
            "list_agents",
            "List agents in the current workspace (or given workspace_id).",
            json!({
                "type": "object",
                "properties": {"workspace_id": {"type": "string"}}
            }),
        ),
        function_tool(
            "spawn_agent",
            "Spawn one or more CLI agents in the current workspace.",
            json!({
                "type": "object",
                "properties": {
                    "agent_type": {"type": "string", "enum": ["kiro-cli", "claude", "aider", "goose", "opencode", "devin", "codex"]},
                    "count": {"type": "integer", "minimum": 1, "maximum": 32, "default": 1},
                    "cwd": {"type": "string"}
                },
                "required": ["agent_type"]
            }),
        ),
        function_tool(
            "close_agent",
            "Close an agent tile by id.",
            json!({
                "type": "object",
                "properties": {"agent_id": {"type": "string"}},
                "required": ["agent_id"]
            }),
        ),
        function_tool(
            "send_to_agent",
            "Inject text + Enter into an agent's stdin. Use 'active' for the focused agent.",
            json!({
                "type": "object",
                "properties": {
                    "agent_id": {"type": "string"},
                    "text": {"type": "string"},
                    "press_enter": {"type": "boolean", "default": true}
                },
                "required": ["agent_id", "text"]
            }),
        ),
        function_tool(
            "wait_for_agent_idle",
            "Block (server-side) until the agent has been silent (no stdout) for `quiet_ms` milliseconds, or `timeout_ms` elapses. Use after `send_to_agent` to know when the agent is ready for the next prompt.",
            json!({
                "type": "object",
                "properties": {
                    "agent_id": {"type": "string"},
                    "quiet_ms": {"type": "integer", "minimum": 100, "maximum": 30000, "default": 1500},
                    "timeout_ms": {"type": "integer", "minimum": 1000, "maximum": 600000, "default": 60000}
                },
                "required": ["agent_id"]
            }),
        ),
        function_tool(
            "tail_agent",
            "Return the last N bytes of an agent's stdout log. Useful for harvesting an agent's reply after wait_for_agent_idle.",
            json!({
                "type": "object",
                "properties": {
                    "agent_id": {"type": "string"},
                    "bytes": {"type": "integer", "minimum": 100, "maximum": 65536, "default": 4000}
                },
                "required": ["agent_id"]
            }),
        ),
        function_tool(
            "get_layout",
            "Return the layout tree of the current workspace.",
            json!({"type": "object", "properties": {}, "required": []}),
        ),
        function_tool(
            "create_task",
            "Create a first-class task in a workspace. Use BEFORE delegating work to a Builder so progress is trackable.",
            json!({
                "type": "object",
                "properties": {
                    "workspace_id": {"type": "string", "description": "Workspace owning the task. Defaults to the current workspace."},
                    "title": {"type": "string", "minLength": 1, "maxLength": 200},
                    "instructions": {"type": "string", "description": "Self-contained brief — what done looks like.", "default": ""},
                    "knowledge": {"type": "string", "description": "Pre-loaded context: file paths, links, references to [[memory-notes]].", "default": ""},
                    "parent_id": {"type": "string", "description": "Optional parent task id for sub-tasks."}
                },
                "required": ["title"]
            }),
        ),
        function_tool(
            "list_tasks",
            "List tasks. Filters are optional and combine.",
            json!({
                "type": "object",
                "properties": {
                    "workspace_id": {"type": "string", "description": "Defaults to current workspace if omitted."},
                    "status": {"type": "string", "enum": ["todo","in_progress","in_review","complete","cancelled"]},
                    "agent_id": {"type": "string"}
                }
            }),
        ),
        function_tool(
            "get_task",
            "Fetch a single task by id (full body).",
            json!({
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"]
            }),
        ),
        function_tool(
            "update_task",
            "Patch a task. Provide only the fields you want to change. status moves through todo → in_progress → in_review → complete (or cancelled).",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "title": {"type": "string"},
                    "instructions": {"type": "string"},
                    "knowledge": {"type": "string"},
                    "status": {"type": "string", "enum": ["todo","in_progress","in_review","complete","cancelled"]}
                },
                "required": ["id"]
            }),
        ),
        function_tool(
            "assign_task_to_agent",
            "Link a task to an agent tile, or pass null to unassign.",
            json!({
                "type": "object",
                "properties": {
                    "task_id": {"type": "string"},
                    "agent_id": {"type": ["string", "null"]}
                },
                "required": ["task_id"]
            }),
        ),
    ];
    out.extend(memory::tools::tool_definitions());
    out.extend(swarm::tools::tool_definitions());
    out.push(function_tool(
        "resolve_project",
        "Match a fuzzy natural-language hint (e.g. 'drugs plugin', 'наркотики плагин', 'pigide') to a real project directory. Returns status=found|ambiguous|not_found and up to 5 candidates. Use this BEFORE deciding what to open.",
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Whatever the user said — typos, transliteration and partial names are fine."}
            },
            "required": ["query"]
        }),
    ));
    out.push(function_tool(
        "open_project",
        "Resolve a fuzzy hint with `resolve_project` and, if the match is unambiguous, create-or-reuse a workspace pointing at it and switch to it. On ambiguity returns the candidate list so you can ask the user. Prefer this over manually calling create_workspace when the user says 'open / switch / переключи / открой <name>'.",
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "workspace_name": {"type": "string", "description": "Optional human-readable workspace name. Defaults to the project's display name."}
            },
            "required": ["query"]
        }),
    ));
    out.push(function_tool(
        "remember_project_alias",
        "Teach the resolver an alternative name for a project. Persists to <path>/.pigmemory/aliases.json so the alias survives across sessions and workspace recreations.",
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Absolute project path."},
                "alias": {"type": "string", "description": "Free-form alias the user wants to remember."}
            },
            "required": ["path", "alias"]
        }),
    ));
    out.push(function_tool(
        "rebuild_project_index",
        "Force a fresh filesystem scan of project roots. Use after the user adds or moves a project on disk.",
        json!({"type": "object", "properties": {}, "required": []}),
    ));
    out
}

fn function_tool(name: &str, description: &str, parameters: Value) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": parameters
        }
    })
}

/// Dispatcher entrypoint: returns a JSON value to be embedded as the
/// "content" of the corresponding tool message.
pub async fn dispatch(
    db: &db::DbPool,
    ws_mgr: &WorkspaceManager,
    agent_mgr: &Arc<AgentManager>,
    task_mgr: &TaskManager,
    memory_svc: &MemoryService,
    resolver: &Arc<ResolverService>,
    app: Option<&AppHandle>,
    name: &str,
    args: &Value,
) -> Result<Value> {
    match name {
        "list_workspaces" => {
            let list = ws_mgr.list()?;
            Ok(json!(list
                .iter()
                .map(|w| json!({
                    "id": w.id,
                    "name": w.name,
                    "agent_count": w.agent_count,
                    "created_at": w.created_at
                }))
                .collect::<Vec<_>>()))
        }
        "create_workspace" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Invalid("name required".into()))?;
            let paths = args
                .get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default();
            // Idempotent by name: if a workspace with the same name already
            // exists, reuse it instead of spawning duplicates. Orchestrator
            // retries on the same intent should not pile up clones.
            let existing = ws_mgr
                .list()?
                .into_iter()
                .find(|w| w.name.eq_ignore_ascii_case(name));
            let (ws, existed) = match existing {
                Some(w) => {
                    // Backfill paths if the caller passed some and the
                    // existing workspace has none.
                    if !paths.is_empty() && w.paths.is_empty() {
                        ws_mgr.set_paths(&w.id, paths.clone())?;
                    }
                    (w, true)
                }
                None => (ws_mgr.create(name, paths)?, false),
            };
            db::set_setting(db, "current_workspace_id", &ws.id)?;
            if let Some(app) = app {
                let _ = app.emit(EV_WORKSPACE_CHANGED, json!({ "current_workspace_id": ws.id }));
            }
            Ok(json!({
                "id": ws.id,
                "name": ws.name,
                "created_at": ws.created_at,
                "existed": existed
            }))
        }
        "switch_workspace" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Invalid("id required".into()))?;
            let _ = ws_mgr.get(id)?;
            db::set_setting(db, "current_workspace_id", id)?;
            if let Some(app) = app {
                let _ = app.emit(EV_WORKSPACE_CHANGED, json!({ "current_workspace_id": id }));
            }
            Ok(json!({"current_workspace_id": id}))
        }
        "delete_workspace" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Invalid("id required".into()))?;
            // Kill any agents in the workspace first.
            let agents = agent_mgr.list(id)?;
            for a in &agents {
                let _ = agent_mgr.kill(&a.id);
            }
            ws_mgr.delete(id)?;
            if let Some(app) = app {
                let _ = app.emit(EV_WORKSPACE_CHANGED, json!({ "deleted": id }));
            }
            Ok(json!({"deleted": id}))
        }
        "list_agents" => {
            let ws_id = args
                .get("workspace_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| db::get_setting(db, "current_workspace_id").ok().flatten())
                .ok_or_else(|| Error::Invalid("no current workspace".into()))?;
            let list = agent_mgr.list(&ws_id)?;
            Ok(json!(list))
        }
        "spawn_agent" => {
            let ws_id = db::get_setting(db, "current_workspace_id")?
                .ok_or_else(|| Error::Invalid("no current workspace".into()))?;
            let agent_type_str = args
                .get("agent_type")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Invalid("agent_type required".into()))?;
            let agent_type = AgentType::parse(agent_type_str)
                .ok_or_else(|| Error::Invalid(format!("unknown agent_type: {}", agent_type_str)))?;
            let count = args
                .get("count")
                .and_then(|v| v.as_i64())
                .unwrap_or(1)
                .max(1)
                .min(32) as usize;
            let cwd = args
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let mut spawned = Vec::with_capacity(count);
            // Get current layout once.
            let mut ws = ws_mgr.get(&ws_id)?;
            for _ in 0..count {
                let a = agent_mgr.spawn(&ws_id, agent_type.clone(), cwd.clone())?;
                ws.layout = std::mem::take(&mut ws.layout).insert_grid(&a.id, 0);
                spawned.push(a);
            }
            ws_mgr.update_layout(&ws_id, &ws.layout)?;
            if let Some(app) = app {
                let _ = app.emit(
                    EV_LAYOUT_CHANGED,
                    json!({ "workspace_id": ws_id, "layout": ws.layout }),
                );
            }
            Ok(json!({
                "spawned": spawned.len(),
                "agents": spawned
            }))
        }
        "close_agent" => {
            let agent_id = args
                .get("agent_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Invalid("agent_id required".into()))?;
            // Find the workspace this agent belongs to so we can update layout.
            let conn = db.get()?;
            let ws_id: Option<String> = conn
                .query_row(
                    "SELECT workspace_id FROM agents WHERE id=?1",
                    [agent_id],
                    |r| r.get::<_, String>(0),
                )
                .ok();
            agent_mgr.kill(agent_id)?;
            if let Some(ws_id) = ws_id {
                let mut ws = ws_mgr.get(&ws_id)?;
                let (new_layout, _) = std::mem::take(&mut ws.layout).remove_leaf(agent_id);
                ws.layout = new_layout;
                ws_mgr.update_layout(&ws_id, &ws.layout)?;
                if let Some(app) = app {
                    let _ = app.emit(
                        EV_LAYOUT_CHANGED,
                        json!({ "workspace_id": ws_id, "layout": ws.layout }),
                    );
                }
            }
            Ok(json!({"closed": agent_id}))
        }
        "send_to_agent" => {
            let agent_id = args
                .get("agent_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Invalid("agent_id required".into()))?;
            let text = args
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Invalid("text required".into()))?;
            let press_enter = args
                .get("press_enter")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let resolved_id = if agent_id == "active" {
                // Use most recently spawned agent in current workspace as a stand-in.
                let ws_id = db::get_setting(db, "current_workspace_id")?
                    .ok_or_else(|| Error::Invalid("no current workspace".into()))?;
                let list = agent_mgr.list(&ws_id)?;
                list.last()
                    .map(|a| a.id.clone())
                    .ok_or_else(|| Error::Invalid("no active agent".into()))?
            } else {
                agent_id.to_string()
            };
            let mut payload = text.as_bytes().to_vec();
            if press_enter {
                payload.push(b'\r');
            }
            let bytes_written = agent_mgr.write(&resolved_id, &payload)?;
            Ok(json!({"sent_to": resolved_id, "bytes": bytes_written}))
        }
        "wait_for_agent_idle" => {
            let agent_id = args
                .get("agent_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Invalid("agent_id required".into()))?;
            let quiet_ms = args
                .get("quiet_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(1500);
            let timeout_ms = args
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(60_000);
            let started = std::time::Instant::now();
            let quiet = std::time::Duration::from_millis(quiet_ms);
            let timeout = std::time::Duration::from_millis(timeout_ms);
            loop {
                if started.elapsed() > timeout {
                    return Ok(json!({
                        "agent_id": agent_id,
                        "status": "timeout",
                        "waited_ms": started.elapsed().as_millis() as u64
                    }));
                }
                if let Some(age) = agent_mgr.last_stdout_age(agent_id) {
                    if age >= quiet {
                        return Ok(json!({
                            "agent_id": agent_id,
                            "status": "idle",
                            "quiet_ms": age.as_millis() as u64,
                            "waited_ms": started.elapsed().as_millis() as u64
                        }));
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
        "tail_agent" => {
            let agent_id = args
                .get("agent_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Invalid("agent_id required".into()))?;
            let n = args.get("bytes").and_then(|v| v.as_u64()).unwrap_or(4000) as usize;
            let log = dirs::data_local_dir()
                .map(|d| d.join("pigide").join("agents").join(format!("{}.log", agent_id)))
                .ok_or_else(|| Error::Other("no data dir".into()))?;
            if !log.exists() {
                return Ok(json!({"agent_id": agent_id, "tail": ""}));
            }
            let bytes = std::fs::read(&log)?;
            let start = bytes.len().saturating_sub(n);
            let tail = String::from_utf8_lossy(&bytes[start..]).to_string();
            Ok(json!({
                "agent_id": agent_id,
                "tail": tail,
                "size": bytes.len()
            }))
        }
        "get_layout" => {
            let ws_id = db::get_setting(db, "current_workspace_id")?
                .ok_or_else(|| Error::Invalid("no current workspace".into()))?;
            let ws = ws_mgr.get(&ws_id)?;
            Ok(serde_json::to_value(&ws.layout)?)
        }
        "create_task" => {
            let title = args
                .get("title")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Invalid("title required".into()))?
                .to_string();
            let workspace_id = match args.get("workspace_id").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => db::get_setting(db, "current_workspace_id")?
                    .ok_or_else(|| Error::Invalid("no current workspace".into()))?,
            };
            let instructions = args
                .get("instructions")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let knowledge = args
                .get("knowledge")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let parent_id = args
                .get("parent_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let task = task_mgr.create(CreateTaskArgs {
                workspace_id,
                title,
                instructions,
                knowledge,
                parent_id,
            })?;
            Ok(serde_json::to_value(&task)?)
        }
        "list_tasks" => {
            let ws_id = match args.get("workspace_id").and_then(|v| v.as_str()) {
                Some(s) => Some(s.to_string()),
                None => db::get_setting(db, "current_workspace_id").ok().flatten(),
            };
            let status = args.get("status").and_then(|v| v.as_str()).map(String::from);
            let agent_id = args
                .get("agent_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            let list = task_mgr.list(ws_id.as_deref(), status.as_deref(), agent_id.as_deref())?;
            Ok(json!(list))
        }
        "get_task" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Invalid("id required".into()))?;
            Ok(serde_json::to_value(&task_mgr.get(id)?)?)
        }
        "update_task" => {
            let id = args
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Invalid("id required".into()))?
                .to_string();
            let title = args
                .get("title")
                .and_then(|v| v.as_str())
                .map(String::from);
            let instructions = args
                .get("instructions")
                .and_then(|v| v.as_str())
                .map(String::from);
            let knowledge = args
                .get("knowledge")
                .and_then(|v| v.as_str())
                .map(String::from);
            let status = args
                .get("status")
                .and_then(|v| v.as_str())
                .map(String::from);
            let task = task_mgr.update(UpdateTaskArgs {
                id,
                title,
                instructions,
                knowledge,
                status,
                agent_id: None,
            })?;
            Ok(serde_json::to_value(&task)?)
        }
        "assign_task_to_agent" => {
            let task_id = args
                .get("task_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Invalid("task_id required".into()))?;
            let agent_id = args
                .get("agent_id")
                .and_then(|v| if v.is_null() { None } else { v.as_str() });
            let task = task_mgr.assign(task_id, agent_id)?;
            Ok(serde_json::to_value(&task)?)
        }
        // Memory tools dispatched from the dedicated module.
        "create_memory" | "read_memory" | "update_memory" | "delete_memory"
        | "list_memories" | "search_memories" | "find_backlinks"
        | "suggest_connections" => {
            memory::tools::dispatch(memory_svc, db, name, args).await
        }
        // Swarm tools (sync — no I/O beyond SQLite).
        "send_mail" | "broadcast" | "read_mailbox" | "mark_mail_read"
        | "start_rollcall" | "collect_rollcall"
        | "claim_files" | "release_files" | "list_file_owners"
        | "open_review_gate" | "vote_review_gate" | "list_review_gates" => {
            swarm::tools::dispatch(db, name, args)
        }
        "resolve_project" => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Invalid("query required".into()))?;
            let outcome = resolver.resolve(query, None);
            Ok(serde_json::to_value(&outcome)?)
        }
        "open_project" => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Invalid("query required".into()))?;
            let outcome = resolver.resolve(query, None);
            match outcome.status {
                ResolveStatus::Found => {
                    let pick = outcome
                        .candidates
                        .first()
                        .ok_or_else(|| Error::Other("found but no candidate".into()))?;
                    let ws_name = args
                        .get("workspace_name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| pick.display_name.clone());
                    let existing = ws_mgr
                        .list()?
                        .into_iter()
                        .find(|w| {
                            w.name.eq_ignore_ascii_case(&ws_name)
                                || w.paths.iter().any(|p| paths_eq(p, &pick.path))
                        });
                    let ws = match existing {
                        Some(mut w) => {
                            if !w.paths.iter().any(|p| paths_eq(p, &pick.path)) {
                                let mut new_paths = w.paths.clone();
                                new_paths.push(pick.path.clone());
                                ws_mgr.set_paths(&w.id, new_paths.clone())?;
                                w.paths = new_paths;
                            }
                            w
                        }
                        None => ws_mgr.create(&ws_name, vec![pick.path.clone()])?,
                    };
                    db::set_setting(db, "current_workspace_id", &ws.id)?;
                    if let Some(app) = app {
                        let _ = app.emit(
                            EV_WORKSPACE_CHANGED,
                            json!({ "current_workspace_id": ws.id }),
                        );
                    }
                    Ok(json!({
                        "status": "opened",
                        "workspace_id": ws.id,
                        "workspace_name": ws.name,
                        "path": pick.path,
                        "display_name": pick.display_name,
                        "confidence": outcome.confidence,
                    }))
                }
                ResolveStatus::Ambiguous => Ok(json!({
                    "status": "ambiguous",
                    "query": outcome.query,
                    "candidates": outcome.candidates,
                    "message": "Multiple projects could match; ask the user to pick.",
                })),
                ResolveStatus::NotFound => Ok(json!({
                    "status": "not_found",
                    "query": outcome.query,
                    "candidates": outcome.candidates,
                })),
            }
        }
        "remember_project_alias" => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Invalid("path required".into()))?;
            let alias = args
                .get("alias")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Invalid("alias required".into()))?;
            let p = PathBuf::from(path);
            let aliases = resolver.add_alias(&p, alias)?;
            Ok(json!({
                "path": path,
                "aliases": aliases,
            }))
        }
        "rebuild_project_index" => {
            let started = std::time::Instant::now();
            let idx = resolver.rebuild();
            Ok(json!({
                "count": idx.projects.len(),
                "took_ms": started.elapsed().as_millis() as u64,
                "built_at": idx.built_at,
            }))
        }
        other => Err(Error::Invalid(format!("unknown tool: {}", other))),
    }
}

fn paths_eq(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

// Suppress unused-variable warning for LayoutNode import in some configurations.
#[allow(dead_code)]
fn _force_use(_: LayoutNode) {}
