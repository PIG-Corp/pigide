/// Tauri event channel names.
pub const EV_AGENT_STDOUT: &str = "agent://stdout";
pub const EV_AGENT_EXIT: &str = "agent://exit";
pub const EV_AGENT_SPAWNED: &str = "agent://spawned";
pub const EV_LAYOUT_CHANGED: &str = "workspace://layout";
pub const EV_WORKSPACE_CHANGED: &str = "workspace://changed";
pub const EV_CHAT_MESSAGE: &str = "chat://message";
pub const EV_CHAT_CHUNK: &str = "chat://chunk";
pub const EV_CHAT_STATUS: &str = "chat://status";
/// Snapshot of the not-yet-finished message queue for a session — fires on
/// every enqueue, claim, cancel, and turn finish.
pub const EV_CHAT_QUEUE: &str = "chat://queue";
/// Active chat scope / current session changed. Payload:
/// `{ scope: "global"|"workspace", session_id: string, workspace_id: string|null }`.
/// Fires on `set_current_session`, `set_chat_scope`, and after a
/// `switch_workspace` resolves a different per-workspace session.
pub const EV_CHAT_SCOPE: &str = "chat://scope";
pub const EV_VOICE_STATE: &str = "voice://state";
pub const EV_VOICE_TRANSCRIPT: &str = "voice://transcript";
pub const EV_VOICE_DOWNLOAD: &str = "voice://download";
