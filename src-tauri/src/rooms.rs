//! Room templates: pre-baked workspace setups (agents + roles + tasks).
//!
//! A room is a quick-start preset that spawns N agents of given (type, role)
//! and optionally creates seed tasks. Catalog is shipped in-source for now;
//! later this becomes JSON in user config.

use crate::agent::{AgentManager, AgentType};
use crate::db::DbPool;
use crate::error::{Error, Result};
use crate::tasks::{CreateTaskArgs, TaskManager};
use crate::workspace::WorkspaceManager;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomAgentSpec {
    pub agent_type: String,
    pub role: String,
    #[serde(default = "one")]
    pub count: usize,
}
fn one() -> usize {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomTaskSpec {
    pub title: String,
    #[serde(default)]
    pub instructions: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    pub agents: Vec<RoomAgentSpec>,
    #[serde(default)]
    pub tasks: Vec<RoomTaskSpec>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyResult {
    pub spawned_agents: Vec<String>,
    pub created_tasks: Vec<String>,
}

pub fn catalog() -> Vec<RoomTemplate> {
    vec![
        RoomTemplate {
            id: "command".into(),
            name: "Command Room".into(),
            description: "Single shell-style agent for direct commands. Best for ad-hoc work."
                .into(),
            agents: vec![RoomAgentSpec {
                agent_type: "kiro-cli".into(),
                role: "builder".into(),
                count: 1,
            }],
            tasks: vec![],
        },
        RoomTemplate {
            id: "swarm".into(),
            name: "Swarm Room".into(),
            description: "Coordinator + 3 builders + 1 reviewer for parallel work.".into(),
            agents: vec![
                RoomAgentSpec {
                    agent_type: "claude".into(),
                    role: "coordinator".into(),
                    count: 1,
                },
                RoomAgentSpec {
                    agent_type: "kiro-cli".into(),
                    role: "builder".into(),
                    count: 3,
                },
                RoomAgentSpec {
                    agent_type: "claude".into(),
                    role: "reviewer".into(),
                    count: 1,
                },
            ],
            tasks: vec![],
        },
        RoomTemplate {
            id: "review".into(),
            name: "Review Room".into(),
            description: "Single reviewer agent for diff review and ship decisions.".into(),
            agents: vec![RoomAgentSpec {
                agent_type: "claude".into(),
                role: "reviewer".into(),
                count: 1,
            }],
            tasks: vec![RoomTaskSpec {
                title: "Review pending changes".into(),
                instructions: "Inspect git diff vs. base branch, flag issues, decide PASS/FAIL."
                    .into(),
            }],
        },
        RoomTemplate {
            id: "scout".into(),
            name: "Scout Room".into(),
            description: "Two scouts to explore unfamiliar codebases in parallel.".into(),
            agents: vec![RoomAgentSpec {
                agent_type: "kiro-cli".into(),
                role: "scout".into(),
                count: 2,
            }],
            tasks: vec![],
        },
    ]
}

pub fn find(id: &str) -> Option<RoomTemplate> {
    catalog().into_iter().find(|t| t.id == id)
}

/// Apply a template into a workspace. Spawns the agents (using auto-grid
/// layout) and creates the seed tasks. Returns the created ids.
pub fn apply(
    db: &DbPool,
    ws_mgr: &WorkspaceManager,
    agent_mgr: &Arc<AgentManager>,
    task_mgr: &TaskManager,
    workspace_id: &str,
    template_id: &str,
) -> Result<ApplyResult> {
    let tpl = find(template_id)
        .ok_or_else(|| Error::NotFound(format!("room template {}", template_id)))?;

    let mut ws = ws_mgr.get(workspace_id)?;
    let mut spawned = Vec::new();
    for spec in &tpl.agents {
        let agent_type = AgentType::parse(&spec.agent_type)
            .ok_or_else(|| Error::Invalid(format!("bad agent_type {}", spec.agent_type)))?;
        for _ in 0..spec.count.max(1) {
            let agent = agent_mgr.spawn(workspace_id, agent_type.clone(), None)?;
            // Persist the role override.
            {
                let conn = db.get()?;
                let _ = conn.execute(
                    "UPDATE agents SET role=?2 WHERE id=?1",
                    rusqlite::params![&agent.id, &spec.role],
                );
            }
            ws.layout = std::mem::take(&mut ws.layout).insert_grid(&agent.id, 0);
            spawned.push(agent.id);
        }
    }
    ws_mgr.update_layout(workspace_id, &ws.layout)?;

    let mut created_tasks = Vec::new();
    for ts in &tpl.tasks {
        let t = task_mgr.create(CreateTaskArgs {
            workspace_id: workspace_id.to_string(),
            title: ts.title.clone(),
            instructions: ts.instructions.clone(),
            knowledge: String::new(),
            parent_id: None,
        })?;
        created_tasks.push(t.id);
    }

    Ok(ApplyResult {
        spawned_agents: spawned,
        created_tasks,
    })
}
