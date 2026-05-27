//! MCP-style tool definitions and dispatch for the memory subsystem.

use crate::error::{Error, Result};
use crate::memory::MemoryService;
use serde_json::{json, Value};

pub fn tool_definitions() -> Vec<Value> {
    vec![
        function_tool(
            "create_memory",
            "Persist a long-term note in the workspace's .pigmemory/ store. Use sparingly — for decisions, patterns, and incidents that future sessions will need.",
            json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string", "minLength": 1, "maxLength": 200},
                    "body": {"type": "string", "default": ""},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "aliases": {"type": "array", "items": {"type": "string"}},
                    "slug": {"type": "string", "description": "Optional slug override."},
                    "kind": {
                        "type": "string",
                        "enum": ["concept", "entity", "source", "task", "chat", "meta"],
                        "description": "Note kind. Defaults to 'source'. Folder placement follows the kind."
                    }
                },
                "required": ["title"]
            }),
        ),
        function_tool(
            "read_memory",
            "Read a note by id (full body, tags, backlinks).",
            json!({
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"]
            }),
        ),
        function_tool(
            "update_memory",
            "Patch a note. Provide only the fields you want to change.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "title": {"type": "string"},
                    "body": {"type": "string"},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "aliases": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["id"]
            }),
        ),
        function_tool(
            "delete_memory",
            "Permanently remove a note (file + index). Confirm with user before calling.",
            json!({
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"]
            }),
        ),
        function_tool(
            "list_memories",
            "List notes in the current workspace. Filter by tag.",
            json!({
                "type": "object",
                "properties": {
                    "tag": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 500, "default": 50}
                }
            }),
        ),
        function_tool(
            "search_memories",
            "Full-text search (FTS5/BM25). Returns ranked snippets.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "minLength": 1},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 50, "default": 10}
                },
                "required": ["query"]
            }),
        ),
        function_tool(
            "find_backlinks",
            "Notes that wikilink to the given id.",
            json!({
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"]
            }),
        ),
        function_tool(
            "suggest_connections",
            "Notes related to the given id by content + tag overlap.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 20, "default": 5}
                },
                "required": ["id"]
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

pub async fn dispatch(
    service: &MemoryService,
    db: &crate::db::DbPool,
    name: &str,
    args: &Value,
) -> Result<Value> {
    match name {
        "create_memory" => {
            let title = args
                .get("title")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Invalid("title required".into()))?;
            let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("");
            let tags = args
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let aliases = args
                .get("aliases")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let slug = args.get("slug").and_then(|v| v.as_str()).map(String::from);
            let kind = args
                .get("kind")
                .and_then(|v| v.as_str())
                .and_then(crate::memory::folders::Kind::parse)
                .unwrap_or_else(crate::memory::folders::Kind::default_for_legacy);
            let ws_id = current_workspace(db)?;
            let note = service.create(&ws_id, title, body, tags, aliases, slug, kind, None)?;
            Ok(json!(note))
        }
        "read_memory" => {
            let id = arg_str(args, "id")?;
            Ok(json!(service.get(id)?))
        }
        "update_memory" => {
            let id = arg_str(args, "id")?.to_string();
            let title = args.get("title").and_then(|v| v.as_str()).map(String::from);
            let body = args.get("body").and_then(|v| v.as_str()).map(String::from);
            let tags = args.get("tags").and_then(|v| v.as_array()).map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            });
            let aliases = args.get("aliases").and_then(|v| v.as_array()).map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            });
            let note = service.update(&id, title, body, tags, aliases)?;
            Ok(json!(note))
        }
        "delete_memory" => {
            let id = arg_str(args, "id")?;
            service.delete(id)?;
            Ok(json!({"deleted": id}))
        }
        "list_memories" => {
            let ws_id = current_workspace(db)?;
            let tag = args.get("tag").and_then(|v| v.as_str());
            let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(50);
            Ok(json!(service.list(&ws_id, tag, limit)?))
        }
        "search_memories" => {
            let ws_id = current_workspace(db)?;
            let q = arg_str(args, "query")?;
            let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(10);
            Ok(json!(service.search(&ws_id, q, limit)?))
        }
        "find_backlinks" => {
            let id = arg_str(args, "id")?;
            Ok(json!(service.find_backlinks(id)?))
        }
        "suggest_connections" => {
            let id = arg_str(args, "id")?;
            let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(5);
            Ok(json!(service.suggest_connections(id, limit)?))
        }
        other => Err(Error::Invalid(format!("unknown memory tool: {}", other))),
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Invalid(format!("{} required", key)))
}

fn current_workspace(db: &crate::db::DbPool) -> Result<String> {
    crate::db::get_setting(db, "current_workspace_id")?
        .ok_or_else(|| Error::Invalid("no current workspace".into()))
}
