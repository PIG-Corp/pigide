//! Fast-lane writer triggered when a task transitions to `complete`.
//! Composes a `tasks/<task-id>.md` stub with title, instructions,
//! knowledge, agent, and files-touched. No LLM calls.

use crate::tasks::Task;

/// Files this task held a lock on at completion time. Pulled from
/// `swarm::ownership::list_for_task`. Empty when the task didn't claim
/// any files.
#[derive(Debug, Clone)]
pub struct FilesTouched {
    pub paths: Vec<String>,
}

/// Build the deterministic body for a task-complete stub. Pure function
/// for easy unit testing — caller is responsible for paths/IO.
pub fn render_task_body(task: &Task, files: &FilesTouched) -> String {
    let mut out = String::new();
    out.push_str("## Summary\n\n");
    if task.instructions.trim().is_empty() {
        out.push_str("_No instructions recorded._\n");
    } else {
        out.push_str(task.instructions.trim());
        out.push_str("\n");
    }

    if !task.knowledge.trim().is_empty() {
        out.push_str("\n## Knowledge\n\n");
        out.push_str(task.knowledge.trim());
        out.push_str("\n");
    }

    out.push_str("\n## Status\n\n");
    out.push_str(&format!("- Final status: `{}`\n", task.status));
    if let Some(agent) = &task.agent_id {
        out.push_str(&format!("- Assigned agent: `{}`\n", agent));
    }
    out.push_str(&format!("- Created: {}\n", task.created_at));
    out.push_str(&format!("- Updated: {}\n", task.updated_at));

    if !files.paths.is_empty() {
        out.push_str("\n## Files touched\n\n");
        for p in &files.paths {
            out.push_str(&format!("- `{}`\n", p));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::Task;

    fn sample_task() -> Task {
        Task {
            id: "abc-123".into(),
            workspace_id: "ws-1".into(),
            agent_id: Some("agent-1".into()),
            parent_id: None,
            title: "Wire ingest".into(),
            instructions: "Add task→complete hook.\n\nSecond paragraph.".into(),
            knowledge: "[[orchestrator]]".into(),
            status: "complete".into(),
            created_at: "2026-05-27T10:00:00Z".into(),
            updated_at: "2026-05-27T15:00:00Z".into(),
        }
    }

    #[test]
    fn renders_summary_knowledge_status_and_files() {
        let body = render_task_body(
            &sample_task(),
            &FilesTouched {
                paths: vec!["src/foo.rs".into(), "src/bar.rs".into()],
            },
        );
        assert!(body.contains("## Summary"));
        assert!(body.contains("Add task→complete hook"));
        assert!(body.contains("## Knowledge"));
        assert!(body.contains("[[orchestrator]]"));
        assert!(body.contains("## Status"));
        assert!(body.contains("- Final status: `complete`"));
        assert!(body.contains("- Assigned agent: `agent-1`"));
        assert!(body.contains("## Files touched"));
        assert!(body.contains("- `src/foo.rs`"));
        assert!(body.contains("- `src/bar.rs`"));
    }

    #[test]
    fn omits_optional_sections_when_empty() {
        let mut t = sample_task();
        t.knowledge = "".into();
        t.agent_id = None;
        let body = render_task_body(&t, &FilesTouched { paths: vec![] });
        assert!(!body.contains("## Knowledge"));
        assert!(!body.contains("## Files touched"));
        assert!(!body.contains("Assigned agent"));
        assert!(body.contains("## Summary"));
        assert!(body.contains("## Status"));
    }

    #[test]
    fn falls_back_to_placeholder_when_instructions_blank() {
        let mut t = sample_task();
        t.instructions = "  \n\t  ".into();
        let body = render_task_body(&t, &FilesTouched { paths: vec![] });
        assert!(body.contains("_No instructions recorded._"));
    }
}
