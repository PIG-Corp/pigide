//! Agent roles. Stored in `agents.role`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Coordinator,
    Builder,
    Reviewer,
    Scout,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Coordinator => "coordinator",
            Role::Builder => "builder",
            Role::Reviewer => "reviewer",
            Role::Scout => "scout",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "coordinator" => Some(Role::Coordinator),
            "builder" => Some(Role::Builder),
            "reviewer" => Some(Role::Reviewer),
            "scout" => Some(Role::Scout),
            _ => None,
        }
    }
    /// Default system-prompt seed for the role. Real prompts live in user
    /// `settings.roles.<name>.prompt`; the orchestrator falls back to these.
    pub fn default_prompt(&self) -> &'static str {
        match self {
            Role::Coordinator => "You are the swarm Coordinator (Architect). Plan work, dispatch builders, monitor progress.\n\n\
Watcher integration: when the Watcher feature is enabled, a Gemini-backed supervisor classifies every spawned agent's stdout chunks. \
Whenever an agent pauses on a question or interactive choice, you'll receive a mail addressed to `role:coordinator` on thread `watcher:<agent_id>`. \
The body is a JSON object {agent_id, prompt_text, options}. Reply on the SAME thread (`thread_id = \"watcher:<agent_id>\"`) using `send_mail {from_agent_id: <your id>, ...}` — your first reply on that thread is auto-injected into the originating agent's stdin and the thread is closed. Keep the reply short and unambiguous (e.g. `y`, `2`, `abort`, or a one-line answer); the Watcher does not reformat it. If you need clarification first, ask the user before replying — once you reply, the agent unblocks immediately.",
            Role::Builder => "You are a Builder. Implement the assigned task end-to-end. Ask the Coordinator if blocked.",
            Role::Reviewer => "You are a Reviewer. Read the diff, run mental checks, return PASS or FAIL with one-line reason.",
            Role::Scout => "You are a Scout. Read existing code, summarise findings; do not write code without explicit ask.",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for r in [
            Role::Coordinator,
            Role::Builder,
            Role::Reviewer,
            Role::Scout,
        ] {
            assert_eq!(Role::parse(r.as_str()), Some(r));
        }
        assert!(Role::parse("ghost").is_none());
    }
}
