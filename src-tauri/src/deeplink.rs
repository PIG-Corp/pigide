//! Deep-link handler for the `pigide://` URI scheme (gap #17).
//!
//! Supported routes (BridgeSpace parity):
//!   pigide://workspace/<id>                        → switch to workspace
//!   pigide://agent/spawn?type=<type>&workspace=<id>&cwd=<path>
//!   pigide://task/<id>                             → focus task in board
//!   pigide://memory/<slug>                         → open memory note
//!   pigide://chat?text=<encoded>                   → seed orchestrator draft
//!
//! The plugin emits raw URLs into our async channel; we parse them here and
//! re-emit a structured `deep-link://nav` event for the frontend store.

use crate::error::Result;
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter};
use url::Url;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NavRoute {
    Workspace {
        id: String,
    },
    AgentSpawn {
        agent_type: String,
        workspace_id: Option<String>,
        cwd: Option<String>,
    },
    Task {
        id: String,
    },
    Memory {
        slug: String,
    },
    Chat {
        text: String,
    },
    Unknown {
        url: String,
    },
}

/// Parse a single `pigide://…` URL into a typed route.
pub fn parse(url: &str) -> NavRoute {
    let parsed = match Url::parse(url) {
        Ok(u) => u,
        Err(_) => {
            return NavRoute::Unknown {
                url: url.to_string(),
            }
        }
    };
    if parsed.scheme() != "pigide" {
        return NavRoute::Unknown {
            url: url.to_string(),
        };
    }
    // url::Url treats `pigide://workspace/abc` as host="workspace", path="/abc".
    let host = parsed.host_str().unwrap_or("").to_string();
    let path: Vec<&str> = parsed
        .path()
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let q: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
    match host.as_str() {
        "workspace" => path
            .first()
            .map(|id| NavRoute::Workspace {
                id: (*id).to_string(),
            })
            .unwrap_or_else(|| NavRoute::Unknown {
                url: url.to_string(),
            }),
        "agent" if path.first() == Some(&"spawn") => NavRoute::AgentSpawn {
            agent_type: q.get("type").cloned().unwrap_or_default(),
            workspace_id: q.get("workspace").cloned(),
            cwd: q.get("cwd").cloned(),
        },
        "task" => path
            .first()
            .map(|id| NavRoute::Task {
                id: (*id).to_string(),
            })
            .unwrap_or_else(|| NavRoute::Unknown {
                url: url.to_string(),
            }),
        "memory" => path
            .first()
            .map(|s| NavRoute::Memory {
                slug: (*s).to_string(),
            })
            .unwrap_or_else(|| NavRoute::Unknown {
                url: url.to_string(),
            }),
        "chat" => NavRoute::Chat {
            text: q.get("text").cloned().unwrap_or_default(),
        },
        _ => NavRoute::Unknown {
            url: url.to_string(),
        },
    }
}

/// Emit a `deep-link://nav` event with the parsed route as payload. The
/// frontend store listens for this and dispatches to the right panel.
pub fn dispatch(app: &AppHandle, url: &str) -> Result<()> {
    let route = parse(url);
    app.emit("deep-link://nav", json!({ "url": url, "route": route }))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_workspace() {
        match parse("pigide://workspace/abc-123") {
            NavRoute::Workspace { id } => assert_eq!(id, "abc-123"),
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn parses_agent_spawn_with_query() {
        let r = parse("pigide://agent/spawn?type=claude&workspace=ws1&cwd=/tmp");
        match r {
            NavRoute::AgentSpawn {
                agent_type,
                workspace_id,
                cwd,
            } => {
                assert_eq!(agent_type, "claude");
                assert_eq!(workspace_id.as_deref(), Some("ws1"));
                assert_eq!(cwd.as_deref(), Some("/tmp"));
            }
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn parses_task() {
        match parse("pigide://task/t-42") {
            NavRoute::Task { id } => assert_eq!(id, "t-42"),
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn parses_memory() {
        match parse("pigide://memory/some-note") {
            NavRoute::Memory { slug } => assert_eq!(slug, "some-note"),
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn parses_chat() {
        match parse("pigide://chat?text=hello%20world") {
            NavRoute::Chat { text } => assert_eq!(text, "hello world"),
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn rejects_other_scheme() {
        match parse("https://example.com/x") {
            NavRoute::Unknown { .. } => {}
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn unknown_path_is_unknown() {
        match parse("pigide://nope/bar") {
            NavRoute::Unknown { .. } => {}
            other => panic!("got {:?}", other),
        }
    }
}
