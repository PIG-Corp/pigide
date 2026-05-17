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
pub const EV_VOICE_STATE: &str = "voice://state";
pub const EV_VOICE_TRANSCRIPT: &str = "voice://transcript";
pub const EV_VOICE_DOWNLOAD: &str = "voice://download";
