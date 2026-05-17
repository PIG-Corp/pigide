//! Orchestrator-tool definitions and dispatch for the swarm subsystem.

use crate::db::DbPool;
use crate::error::{Error, Result};
use crate::swarm::{mailbox, ownership, review, rollcall};
use serde_json::{json, Value};

pub fn tool_definitions() -> Vec<Value> {
    vec![
        function_tool(
            "send_mail",
            "Send a message to another agent. `to` is an agent UUID or `role:builder` (etc.) for broadcast.",
            json!({
                "type": "object",
                "properties": {
                    "to": {"type": "string"},
                    "body": {"type": "string"},
                    "thread_id": {"type": "string"}
                },
                "required": ["to", "body"]
            }),
        ),
        function_tool(
            "broadcast",
            "Send a message to every agent of a role.",
            json!({
                "type": "object",
                "properties": {
                    "role": {"type": "string", "enum": ["coordinator","builder","reviewer","scout"]},
                    "body": {"type": "string"}
                },
                "required": ["role", "body"]
            }),
        ),
        function_tool(
            "read_mailbox",
            "Read pending mail for an address (agent_id or `role:<x>`).",
            json!({
                "type": "object",
                "properties": {
                    "to": {"type": "string"},
                    "unread_only": {"type": "boolean", "default": true},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50}
                },
                "required": ["to"]
            }),
        ),
        function_tool(
            "mark_mail_read",
            "Mark mailbox messages as read.",
            json!({
                "type": "object",
                "properties": {
                    "ids": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["ids"]
            }),
        ),
        function_tool(
            "start_rollcall",
            "Broadcast a prompt to a role and get a rollcall id; collect later via collect_rollcall.",
            json!({
                "type": "object",
                "properties": {
                    "role": {"type": "string", "enum": ["coordinator","builder","reviewer","scout"]},
                    "prompt": {"type": "string"}
                },
                "required": ["role", "prompt"]
            }),
        ),
        function_tool(
            "collect_rollcall",
            "Return all responses gathered for a rollcall id so far.",
            json!({
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"]
            }),
        ),
        function_tool(
            "claim_files",
            "Take exclusive ownership of one or more files for a task. Other tasks cannot edit a file you've claimed until you `release_files` (or the task closes). Returns a per-path map: true = locked by you, false = blocked by another task.",
            json!({
                "type": "object",
                "properties": {
                    "workspace_id": {"type": "string"},
                    "task_id": {"type": "string"},
                    "agent_id": {"type": "string"},
                    "paths": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["workspace_id", "task_id", "paths"]
            }),
        ),
        function_tool(
            "release_files",
            "Release file locks held by a task. Without `paths`, releases every lock the task holds.",
            json!({
                "type": "object",
                "properties": {
                    "workspace_id": {"type": "string"},
                    "task_id": {"type": "string"},
                    "paths": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["task_id"]
            }),
        ),
        function_tool(
            "list_file_owners",
            "List file ownership rows. Filter by workspace or task.",
            json!({
                "type": "object",
                "properties": {
                    "workspace_id": {"type": "string"},
                    "task_id": {"type": "string"}
                }
            }),
        ),
        function_tool(
            "open_review_gate",
            "Open a review gate on a task. The task cannot be marked `complete` until every gate votes PASS.",
            json!({
                "type": "object",
                "properties": {
                    "task_id": {"type": "string"},
                    "reviewer_id": {"type": "string"}
                },
                "required": ["task_id"]
            }),
        ),
        function_tool(
            "vote_review_gate",
            "Cast the Reviewer's verdict on a gate.",
            json!({
                "type": "object",
                "properties": {
                    "gate_id": {"type": "string"},
                    "verdict": {"type": "string", "enum": ["pass","fail","pending"]},
                    "reason": {"type": "string"}
                },
                "required": ["gate_id", "verdict"]
            }),
        ),
        function_tool(
            "list_review_gates",
            "List all gates for a task with their current verdicts.",
            json!({
                "type": "object",
                "properties": {"task_id": {"type": "string"}},
                "required": ["task_id"]
            }),
        ),
    ]
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

pub fn dispatch(db: &DbPool, name: &str, args: &Value) -> Result<Value> {
    match name {
        "send_mail" => {
            let to = arg_str(args, "to")?;
            let body = arg_str(args, "body")?;
            let thread = args.get("thread_id").and_then(|v| v.as_str());
            let m = mailbox::send(db, None, to, body, thread)?;
            Ok(json!(m))
        }
        "broadcast" => {
            let role = arg_str(args, "role")?;
            let body = arg_str(args, "body")?;
            let m = mailbox::broadcast(db, None, role, body)?;
            Ok(json!(m))
        }
        "read_mailbox" => {
            let to = arg_str(args, "to")?;
            let unread = args
                .get("unread_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(50);
            Ok(json!(mailbox::list(db, Some(to), unread, limit)?))
        }
        "mark_mail_read" => {
            let ids: Vec<String> = args
                .get("ids")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let n = mailbox::mark_read(db, &ids)?;
            Ok(json!({"updated": n}))
        }
        "start_rollcall" => {
            let role = arg_str(args, "role")?;
            let prompt = arg_str(args, "prompt")?;
            Ok(json!(rollcall::start(db, role, prompt)?))
        }
        "collect_rollcall" => {
            let id = arg_str(args, "id")?;
            Ok(json!(rollcall::collect(db, id)?))
        }
        "claim_files" => {
            let workspace_id = arg_str(args, "workspace_id")?;
            let task_id = arg_str(args, "task_id")?;
            let agent_id = args.get("agent_id").and_then(|v| v.as_str());
            let paths: Vec<String> = args
                .get("paths")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            if paths.is_empty() {
                return Err(Error::Invalid("paths cannot be empty".into()));
            }
            let mut results = serde_json::Map::new();
            let mut blocked: Vec<String> = Vec::new();
            for p in &paths {
                let ok = ownership::acquire(db, workspace_id, p, task_id, agent_id)?;
                results.insert(p.clone(), json!(ok));
                if !ok {
                    blocked.push(p.clone());
                }
            }
            Ok(json!({
                "claimed": results,
                "blocked_paths": blocked,
                "all_acquired": blocked.is_empty()
            }))
        }
        "release_files" => {
            let task_id = arg_str(args, "task_id")?;
            match args.get("paths").and_then(|v| v.as_array()) {
                Some(arr) => {
                    let workspace_id = arg_str(args, "workspace_id")?;
                    let mut released = 0u64;
                    for v in arr {
                        if let Some(p) = v.as_str() {
                            if ownership::release(db, workspace_id, p, task_id)? {
                                released += 1;
                            }
                        }
                    }
                    Ok(json!({"released": released}))
                }
                None => {
                    let n = ownership::release_all_for_task(db, task_id)?;
                    Ok(json!({"released": n}))
                }
            }
        }
        "list_file_owners" => {
            if let Some(task_id) = args.get("task_id").and_then(|v| v.as_str()) {
                Ok(json!(ownership::list_for_task(db, task_id)?))
            } else if let Some(ws_id) = args.get("workspace_id").and_then(|v| v.as_str()) {
                Ok(json!(ownership::list_for_workspace(db, ws_id)?))
            } else {
                Err(Error::Invalid(
                    "either task_id or workspace_id is required".into(),
                ))
            }
        }
        "open_review_gate" => {
            let task_id = arg_str(args, "task_id")?;
            let reviewer = args.get("reviewer_id").and_then(|v| v.as_str());
            Ok(json!(review::open(db, task_id, reviewer)?))
        }
        "vote_review_gate" => {
            let gate_id = arg_str(args, "gate_id")?;
            let verdict = review::Verdict::parse(arg_str(args, "verdict")?)
                .ok_or_else(|| Error::Invalid("verdict must be pass|fail|pending".into()))?;
            let reason = args.get("reason").and_then(|v| v.as_str()).unwrap_or("");
            Ok(json!(review::vote(db, gate_id, verdict, reason)?))
        }
        "list_review_gates" => {
            let task_id = arg_str(args, "task_id")?;
            Ok(json!(review::list_for_task(db, task_id)?))
        }
        other => Err(Error::Invalid(format!("unknown swarm tool: {}", other))),
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Invalid(format!("{} required", key)))
}
