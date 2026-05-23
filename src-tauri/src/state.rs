use crate::agent::AgentManager;
use crate::architect::Architect;
use crate::chat_queue_worker::ChatQueueWorker;
use crate::db::DbPool;
use crate::mcp::server::McpServerHandle;
use crate::memory::MemoryService;
use crate::orchestrator::Orchestrator;
use crate::project_resolver::ResolverService;
use crate::skills::SkillRegistry;
use crate::tasks::TaskManager;
use crate::voice::VoicePipeline;
use std::sync::Arc;

/// Global app state managed by Tauri.
pub struct AppState {
    pub db: DbPool,
    pub agent_mgr: Arc<AgentManager>,
    pub orchestrator: Arc<Orchestrator>,
    pub chat_queue: Arc<ChatQueueWorker>,
    pub voice: Arc<VoicePipeline>,
    pub task_mgr: Arc<TaskManager>,
    pub memory: Arc<MemoryService>,
    pub mcp: Arc<McpServerHandle>,
    pub skills: Arc<SkillRegistry>,
    pub architect: Arc<Architect>,
    pub resolver: Arc<ResolverService>,
    /// Watcher (Gemini-backed supervisor). Populated only when the `watcher`
    /// feature is compiled in AND `GEMINI_API_KEY` was readable at boot.
    #[cfg(feature = "watcher")]
    pub watcher: parking_lot::RwLock<Option<Arc<crate::watcher::Watcher>>>,
}
