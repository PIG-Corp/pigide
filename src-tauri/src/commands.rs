use crate::agent::AgentType;
use crate::chat::ChatMessage;
use crate::db::{self};
use crate::layout::LayoutNode;
use crate::memory::service::{Backlink, GraphData, NoteSummary, SearchHit};
use crate::memory::note::Note;
use crate::state::AppState;
use crate::tasks::{CreateTaskArgs, Task, UpdateTaskArgs};
use crate::workspace::Workspace;
use base64::Engine;
use serde::Deserialize;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn ping() -> std::result::Result<String, String> {
    Ok("pong".into())
}

// ---------- Workspaces ----------

#[tauri::command]
pub async fn list_workspaces(state: State<'_, AppState>) -> std::result::Result<Vec<Workspace>, String> {
    crate::workspace::WorkspaceManager::new(state.db.clone())
        .list()
        .map_err(Into::into)
}

#[tauri::command]
pub async fn get_workspace(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<Workspace, String> {
    let ws_mgr = crate::workspace::WorkspaceManager::new(state.db.clone());
    // Prune leaves that reference dead agents — fixes "blank xterm" after
    // re-entering a workspace whose PTYs were killed at startup.
    let _ = ws_mgr.prune_stale_layout(&id);
    ws_mgr.get(&id).map_err(Into::into)
}

#[derive(Deserialize)]
pub struct CreateWsArgs {
    pub name: String,
    #[serde(default)]
    pub paths: Vec<String>,
}

#[tauri::command]
pub async fn create_workspace(
    state: State<'_, AppState>,
    args: CreateWsArgs,
) -> std::result::Result<Workspace, String> {
    crate::workspace::WorkspaceManager::new(state.db.clone())
        .create(&args.name, args.paths)
        .map_err(Into::into)
}

#[tauri::command]
pub async fn rename_workspace(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> std::result::Result<(), String> {
    crate::workspace::WorkspaceManager::new(state.db.clone())
        .rename(&id, &name)
        .map_err(Into::into)
}

#[tauri::command]
pub async fn delete_workspace(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<(), String> {
    // Kill all agents in the workspace.
    let agents = state.agent_mgr.list(&id).unwrap_or_default();
    for a in &agents {
        let _ = state.agent_mgr.kill(&a.id);
    }
    crate::workspace::WorkspaceManager::new(state.db.clone())
        .delete(&id)
        .map_err(Into::into)
}

#[tauri::command]
pub async fn update_layout(
    state: State<'_, AppState>,
    workspace_id: String,
    layout: LayoutNode,
) -> std::result::Result<(), String> {
    crate::workspace::WorkspaceManager::new(state.db.clone())
        .update_layout(&workspace_id, &layout)
        .map_err(Into::into)
}

#[tauri::command]
pub async fn set_current_workspace(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<(), String> {
    db::set_setting(&state.db, "current_workspace_id", &id).map_err(Into::into)
}

#[tauri::command]
pub async fn get_current_workspace(
    state: State<'_, AppState>,
) -> std::result::Result<Option<String>, String> {
    db::get_setting(&state.db, "current_workspace_id").map_err(Into::into)
}

// ---------- Agents ----------

#[derive(Deserialize)]
pub struct SpawnAgentArgs {
    pub workspace_id: String,
    pub agent_type: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub count: Option<usize>,
    /// When true (default), the new agent is appended to the workspace
    /// layout via the auto-grid algorithm. When false, the caller is
    /// expected to update the layout itself (e.g. for a precise split).
    #[serde(default)]
    pub auto_layout: Option<bool>,
}

#[tauri::command]
pub async fn spawn_agent(
    state: State<'_, AppState>,
    args: SpawnAgentArgs,
) -> std::result::Result<Vec<crate::agent::Agent>, String> {
    let agent_type = AgentType::parse(&args.agent_type)
        .ok_or_else(|| format!("unknown agent_type: {}", args.agent_type))?;
    let count = args.count.unwrap_or(1).max(1);
    let auto_layout = args.auto_layout.unwrap_or(true);
    let mut spawned = Vec::with_capacity(count);

    let ws_mgr = crate::workspace::WorkspaceManager::new(state.db.clone());
    let mut ws = ws_mgr.get(&args.workspace_id).map_err(Into::<String>::into)?;

    for _ in 0..count {
        let a = state
            .agent_mgr
            .spawn(&args.workspace_id, agent_type.clone(), args.cwd.clone())
            .map_err(Into::<String>::into)?;
        if auto_layout {
            ws.layout = std::mem::take(&mut ws.layout).insert_grid(&a.id, 0);
        }
        spawned.push(a);
    }
    if auto_layout {
        ws_mgr
            .update_layout(&args.workspace_id, &ws.layout)
            .map_err(Into::<String>::into)?;
    }
    Ok(spawned)
}

#[tauri::command]
pub async fn kill_agent(
    state: State<'_, AppState>,
    agent_id: String,
) -> std::result::Result<(), String> {
    state.agent_mgr.kill(&agent_id).map_err(Into::into)
}

#[derive(Deserialize)]
pub struct WriteAgentArgs {
    pub agent_id: String,
    pub data_b64: String,
}

#[tauri::command]
pub async fn write_to_agent(
    state: State<'_, AppState>,
    args: WriteAgentArgs,
) -> std::result::Result<(), String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&args.data_b64)
        .map_err(|e| format!("base64: {}", e))?;
    state
        .agent_mgr
        .write(&args.agent_id, &bytes)
        .map(|_| ())
        .map_err(Into::into)
}

#[derive(Deserialize)]
pub struct ResizeAgentArgs {
    pub agent_id: String,
    pub cols: u16,
    pub rows: u16,
}

#[tauri::command]
pub async fn resize_agent(
    state: State<'_, AppState>,
    args: ResizeAgentArgs,
) -> std::result::Result<(), String> {
    state
        .agent_mgr
        .resize(&args.agent_id, args.cols, args.rows)
        .map_err(Into::into)
}

#[tauri::command]
pub async fn list_agents(
    state: State<'_, AppState>,
    workspace_id: String,
) -> std::result::Result<Vec<crate::agent::Agent>, String> {
    state.agent_mgr.list(&workspace_id).map_err(Into::into)
}

/// Return base64-encoded tail of an agent's PTY log (up to `max_bytes`,
/// default 64KiB). Used to replay scrollback into xterm after a session
/// restore.
#[tauri::command]
pub async fn agent_log_tail(
    state: State<'_, AppState>,
    agent_id: String,
    max_bytes: Option<usize>,
) -> std::result::Result<String, String> {
    let bytes = state
        .agent_mgr
        .read_log_tail(&agent_id, max_bytes.unwrap_or(64 * 1024))
        .map_err(Into::<String>::into)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// Trigger an explicit session restore (idempotent). Useful for tests and the
/// `Restore session` button in the UI when an automatic restore was skipped.
#[tauri::command]
pub async fn restore_session(
    state: State<'_, AppState>,
) -> std::result::Result<(usize, usize), String> {
    state.agent_mgr.restore_session().map_err(Into::into)
}

// ---------- Chat ----------

#[tauri::command]
pub async fn list_chat(
    state: State<'_, AppState>,
) -> std::result::Result<Vec<ChatMessage>, String> {
    let session_id = crate::chat_sessions::ensure_current(&state.db).map_err(|e| e.to_string())?;
    crate::chat::list(&state.db, &session_id, 200).map_err(Into::into)
}

#[derive(Deserialize)]
pub struct SendChatArgs {
    pub text: String,
    /// Optional `@`-mention attachments. Empty / missing = no attachments
    /// (backwards compatible with older frontends).
    #[serde(default)]
    pub attachments: Vec<crate::path_suggest::Attachment>,
}

#[tauri::command]
pub async fn send_chat(
    state: State<'_, AppState>,
    args: SendChatArgs,
) -> std::result::Result<crate::chat_queue::QueueItem, String> {
    let session_id =
        crate::chat_sessions::ensure_current(&state.db).map_err(|e| e.to_string())?;
    // Validate every attachment against the allow-list BEFORE enqueuing.
    // Bad paths surface as a user-visible error and the message is NOT
    // queued — UX expects the user to fix the chip and resend.
    let ws_mgr = crate::workspace::WorkspaceManager::new(state.db.clone());
    let validated = crate::path_suggest::validate_all(&ws_mgr, &args.attachments)
        .map_err(|e| e.to_string())?;
    let item = crate::chat_queue::enqueue_with_attachments(
        &state.db,
        &session_id,
        &args.text,
        validated,
    )
    .map_err(|e| e.to_string())?;
    state.chat_queue.poke();
    state.chat_queue.emit_snapshot();
    Ok(item)
}

#[derive(Deserialize)]
pub struct SuggestPathsArgs {
    pub query: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
}

#[tauri::command]
pub async fn suggest_paths(
    state: State<'_, AppState>,
    args: SuggestPathsArgs,
) -> std::result::Result<Vec<crate::path_suggest::Suggestion>, String> {
    let ws_mgr = crate::workspace::WorkspaceManager::new(state.db.clone());
    crate::path_suggest::suggest(
        &state.db,
        &ws_mgr,
        crate::path_suggest::SuggestArgs {
            query: args.query,
            workspace_id: args.workspace_id,
        },
    )
    .map_err(Into::into)
}

#[tauri::command]
pub async fn list_chat_queue(
    state: State<'_, AppState>,
) -> std::result::Result<Vec<crate::chat_queue::QueueItem>, String> {
    let session_id =
        crate::chat_sessions::ensure_current(&state.db).map_err(|e| e.to_string())?;
    crate::chat_queue::list(&state.db, &session_id).map_err(Into::into)
}

#[tauri::command]
pub async fn cancel_chat_queue_item(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<bool, String> {
    let removed =
        crate::chat_queue::cancel(&state.db, &id).map_err(|e| e.to_string())?;
    if removed {
        state.chat_queue.emit_snapshot();
    }
    Ok(removed)
}

#[derive(Deserialize)]
pub struct ChatQueueErrorPolicyArgs {
    pub continue_on_error: bool,
}

#[tauri::command]
pub async fn chat_queue_set_continue_on_error(
    state: State<'_, AppState>,
    args: ChatQueueErrorPolicyArgs,
) -> std::result::Result<(), String> {
    crate::db::set_setting(
        &state.db,
        "chat.queue.continue_on_error",
        if args.continue_on_error { "true" } else { "false" },
    )
    .map_err(Into::into)
}

#[tauri::command]
pub async fn chat_queue_get_continue_on_error(
    state: State<'_, AppState>,
) -> std::result::Result<bool, String> {
    Ok(crate::chat_queue::continue_on_error(&state.db))
}

#[tauri::command]
pub async fn clear_chat(state: State<'_, AppState>) -> std::result::Result<(), String> {
    let session_id = crate::chat_sessions::ensure_current(&state.db).map_err(|e| e.to_string())?;
    state.orchestrator.clear(&session_id).map_err(Into::into)
}

/// Cancel the orchestrator's currently-running turn (if any). Idempotent —
/// returns `false` when nothing was in flight, `true` when a cancel signal
/// was actually delivered. The orchestrator drops the in-flight LLM
/// stream, skips any not-yet-started tool_calls of this iteration, writes
/// a "(остановлено пользователем)" marker into the session, and emits
/// `chat://status idle` so the UI re-enables input.
#[tauri::command]
pub async fn stop_chat(state: State<'_, AppState>) -> std::result::Result<bool, String> {
    Ok(state.orchestrator.stop())
}

// ---------- Chat sessions ----------

#[tauri::command]
pub async fn list_chat_sessions(
    state: State<'_, AppState>,
) -> std::result::Result<Vec<crate::chat_sessions::ChatSession>, String> {
    crate::chat_sessions::list(&state.db).map_err(Into::into)
}

#[derive(Deserialize)]
pub struct CreateSessionArgs { pub name: String }

#[tauri::command]
pub async fn create_chat_session(
    state: State<'_, AppState>,
    args: CreateSessionArgs,
) -> std::result::Result<crate::chat_sessions::ChatSession, String> {
    crate::chat_sessions::create(&state.db, &args.name).map_err(Into::into)
}

#[derive(Deserialize)]
pub struct RenameSessionArgs { pub id: String, pub name: String }

#[tauri::command]
pub async fn rename_chat_session(
    state: State<'_, AppState>,
    args: RenameSessionArgs,
) -> std::result::Result<(), String> {
    crate::chat_sessions::rename(&state.db, &args.id, &args.name).map_err(Into::into)
}

#[tauri::command]
pub async fn delete_chat_session(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<(), String> {
    crate::chat_sessions::delete(&state.db, &id).map_err(Into::into)
}

#[tauri::command]
pub async fn get_current_session(
    state: State<'_, AppState>,
) -> std::result::Result<String, String> {
    crate::chat_sessions::ensure_current(&state.db).map_err(Into::into)
}

#[tauri::command]
pub async fn set_current_session(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<(), String> {
    crate::chat_sessions::set_current(&state.db, &id).map_err(Into::into)
}

// ---------- Settings ----------

#[derive(Deserialize)]
pub struct SetSettingArgs {
    pub key: String,
    pub value: String,
}

#[tauri::command]
pub async fn set_setting(
    state: State<'_, AppState>,
    args: SetSettingArgs,
) -> std::result::Result<(), String> {
    db::set_setting(&state.db, &args.key, &args.value).map_err(Into::into)
}

#[tauri::command]
pub async fn get_setting(
    state: State<'_, AppState>,
    key: String,
) -> std::result::Result<Option<String>, String> {
    db::get_setting(&state.db, &key).map_err(Into::into)
}

// ---------- Architect provider ----------

#[derive(serde::Serialize)]
pub struct ProviderInfo {
    pub provider: String,
    pub primary_model: String,
    pub fallback_model: Option<String>,
    pub has_api_key: bool,
}

/// Return the current architect provider configuration so the settings UI
/// can render an accurate snapshot without poking the model.
#[tauri::command]
pub async fn provider_info(
    state: State<'_, AppState>,
) -> std::result::Result<ProviderInfo, String> {
    let provider = crate::orchestrator::providers::build_provider(&state.db);
    Ok(ProviderInfo {
        provider: provider.label().to_string(),
        primary_model: provider.primary_model().to_string(),
        fallback_model: provider.fallback_model().map(|s| s.to_string()),
        has_api_key: true,
    })
}

/// Round-trip a tiny request to the active provider to validate auth + model.
/// Returns ok=false on failure with a human-readable note (never a hard error).
#[tauri::command]
pub async fn provider_test_connection(
    state: State<'_, AppState>,
) -> std::result::Result<crate::orchestrator::providers::PingInfo, String> {
    let provider = crate::orchestrator::providers::build_provider(&state.db);
    provider.ping().await.map_err(Into::into)
}

// ---------- Voice ----------

#[tauri::command]
pub async fn start_voice(state: State<'_, AppState>) -> std::result::Result<(), String> {
    state.voice.start().map_err(Into::into)
}

#[tauri::command]
pub async fn stop_voice(state: State<'_, AppState>) -> std::result::Result<(), String> {
    let voice: Arc<crate::voice::VoicePipeline> = state.voice.clone();
    tokio::spawn(async move {
        if let Err(e) = voice.stop_and_transcribe().await {
            tracing::error!("voice: {}", e);
        }
    });
    Ok(())
}

/// Abort an in-flight transcription. Maps to the third-press of the toggle
/// hotkey (or a click on the mic button while it's in the `transcribing`
/// state). Stale Whisper output is dropped instead of injected.
#[tauri::command]
pub async fn cancel_voice(state: State<'_, AppState>) -> std::result::Result<(), String> {
    state.voice.cancel();
    Ok(())
}

// ---------- Voice: model registry ----------

#[tauri::command]
pub async fn voice_list_models() -> std::result::Result<Vec<crate::voice::download::ModelInfo>, String> {
    Ok(crate::voice::download::list_models())
}

#[derive(Deserialize)]
pub struct VoiceSetModelArgs {
    pub model_id: String,
}

#[tauri::command]
pub async fn voice_set_model(
    state: State<'_, AppState>,
    args: VoiceSetModelArgs,
) -> std::result::Result<(), String> {
    let id = crate::voice::download::ModelId::parse(&args.model_id)
        .ok_or_else(|| format!("unknown model: {}", args.model_id))?;
    db::set_setting(&state.db, "whisper.model_id", id.as_str())
        .map_err(|e| e.to_string())?;
    // Drop the cached context for any previously-active model so the next
    // transcription rebuilds with the new one.
    state.voice.evict_model(id.as_str());
    Ok(())
}

// ---------- Voice: dictionary ----------

#[tauri::command]
pub async fn voice_dict_list(
    state: State<'_, AppState>,
) -> std::result::Result<Vec<crate::voice::dictionary::DictEntry>, String> {
    crate::voice::dictionary::list(&state.db).map_err(Into::into)
}

#[derive(Deserialize)]
pub struct VoiceDictAddArgs {
    pub pattern: String,
    pub replacement: String,
    #[serde(default)]
    pub case_sense: bool,
}

#[tauri::command]
pub async fn voice_dict_add(
    state: State<'_, AppState>,
    args: VoiceDictAddArgs,
) -> std::result::Result<crate::voice::dictionary::DictEntry, String> {
    crate::voice::dictionary::add(
        &state.db,
        &args.pattern,
        &args.replacement,
        args.case_sense,
    )
    .map_err(Into::into)
}

#[derive(Deserialize)]
pub struct VoiceDictUpdateArgs {
    pub id: String,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub replacement: Option<String>,
    #[serde(default)]
    pub case_sense: Option<bool>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[tauri::command]
pub async fn voice_dict_update(
    state: State<'_, AppState>,
    args: VoiceDictUpdateArgs,
) -> std::result::Result<(), String> {
    crate::voice::dictionary::update(
        &state.db,
        &args.id,
        args.pattern.as_deref(),
        args.replacement.as_deref(),
        args.case_sense,
        args.enabled,
    )
    .map_err(Into::into)
}

#[tauri::command]
pub async fn voice_dict_delete(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<(), String> {
    crate::voice::dictionary::delete(&state.db, &id).map_err(Into::into)
}

// ---------- Voice: history & stats ----------

#[tauri::command]
pub async fn voice_history_list(
    state: State<'_, AppState>,
    limit: Option<i64>,
) -> std::result::Result<Vec<crate::voice::history::Transcript>, String> {
    crate::voice::history::list(&state.db, limit.unwrap_or(100)).map_err(Into::into)
}

#[derive(Deserialize)]
pub struct VoiceHistorySearchArgs {
    pub query: String,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[tauri::command]
pub async fn voice_history_search(
    state: State<'_, AppState>,
    args: VoiceHistorySearchArgs,
) -> std::result::Result<Vec<crate::voice::history::Transcript>, String> {
    crate::voice::history::search(&state.db, &args.query, args.limit.unwrap_or(50))
        .map_err(Into::into)
}

#[tauri::command]
pub async fn voice_history_delete(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<(), String> {
    crate::voice::history::delete(&state.db, &id).map_err(Into::into)
}

#[tauri::command]
pub async fn voice_stats(
    state: State<'_, AppState>,
    range: Option<String>,
) -> std::result::Result<crate::voice::history::VoiceStats, String> {
    let r = range.unwrap_or_else(|| "all".to_string());
    crate::voice::history::stats(&state.db, &r).map_err(Into::into)
}

// ---------- MCP server ----------

#[derive(serde::Serialize)]
pub struct McpStatus {
    pub running: bool,
    pub addr: Option<String>,
}

#[tauri::command]
pub async fn mcp_status(state: State<'_, AppState>) -> std::result::Result<McpStatus, String> {
    Ok(McpStatus {
        running: state.mcp.is_running(),
        addr: state.mcp.current_addr().map(|a| a.to_string()),
    })
}

#[derive(Deserialize)]
pub struct McpStartArgs {
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub bind_all: bool,
}

#[tauri::command]
pub async fn mcp_start(
    state: State<'_, AppState>,
    args: Option<McpStartArgs>,
) -> std::result::Result<String, String> {
    let args = args.unwrap_or(McpStartArgs {
        port: None,
        bind_all: false,
    });
    let port = args.port.unwrap_or(20129);
    let host = if args.bind_all { [0, 0, 0, 0] } else { [127, 0, 0, 1] };
    let bind = std::net::SocketAddr::from((host, port));
    let mcp_state = crate::mcp::server::McpState {
        db: state.db.clone(),
        ws_mgr: Arc::new(crate::workspace::WorkspaceManager::new(state.db.clone())),
        agent_mgr: state.agent_mgr.clone(),
        task_mgr: state.task_mgr.clone(),
        memory: state.memory.clone(),
        resolver: state.resolver.clone(),
        #[cfg(feature = "watcher")]
        watcher: state.watcher.read().clone(),
    };
    let addr = crate::mcp::server::start(state.mcp.clone(), mcp_state, bind)
        .await
        .map_err(|e| e.to_string())?;
    Ok(addr.to_string())
}

#[tauri::command]
pub async fn mcp_stop(state: State<'_, AppState>) -> std::result::Result<(), String> {
    state.mcp.stop();
    Ok(())
}

#[tauri::command]
pub async fn mcp_list_keys(
    state: State<'_, AppState>,
) -> std::result::Result<Vec<crate::mcp::auth::KeyInfo>, String> {
    crate::mcp::auth::list(&state.db).map_err(Into::into)
}

#[derive(Deserialize)]
pub struct McpCreateKeyArgs {
    pub label: String,
    #[serde(default)]
    pub scopes: Vec<String>,
}

#[tauri::command]
pub async fn mcp_create_key(
    state: State<'_, AppState>,
    args: McpCreateKeyArgs,
) -> std::result::Result<crate::mcp::auth::CreatedKey, String> {
    crate::mcp::auth::create(&state.db, &args.label, args.scopes).map_err(Into::into)
}

#[tauri::command]
pub async fn mcp_revoke_key(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<(), String> {
    crate::mcp::auth::revoke(&state.db, &id).map_err(Into::into)
}

#[derive(serde::Serialize)]
pub struct McpRegisterCwdResult {
    /// Path to the merged `.mcp.json`.
    pub path: String,
    /// True if the file was written; false if `pigide` was already wired up.
    pub changed: bool,
}

/// One-shot fix-up: merges a `pigide` entry into `<cwd>/.mcp.json` for
/// already-running Claude tiles that pre-date auto-registration. Idempotent.
/// User-level servers in `~/.claude.json` are left untouched.
#[tauri::command]
pub async fn mcp_register_cwd(
    state: State<'_, AppState>,
    cwd: String,
) -> std::result::Result<McpRegisterCwdResult, String> {
    let p = std::path::PathBuf::from(&cwd);
    if !p.is_dir() {
        return Err(format!("not a directory: {}", cwd));
    }
    let changed = crate::mcp::launcher::merge_project_mcp_json(&state.db, &state.mcp, &p)
        .map_err(|e| e.to_string())?;
    Ok(McpRegisterCwdResult {
        path: p.join(".mcp.json").to_string_lossy().into_owned(),
        changed,
    })
}

// ---------- Rooms (workspace presets) ----------

#[tauri::command]
pub async fn list_room_templates() -> std::result::Result<Vec<crate::rooms::RoomTemplate>, String> {
    Ok(crate::rooms::catalog())
}

#[derive(Deserialize)]
pub struct ApplyRoomArgs {
    pub workspace_id: String,
    pub template_id: String,
}

#[tauri::command]
pub async fn apply_room_template(
    state: State<'_, AppState>,
    args: ApplyRoomArgs,
) -> std::result::Result<crate::rooms::ApplyResult, String> {
    let ws_mgr = crate::workspace::WorkspaceManager::new(state.db.clone());
    crate::rooms::apply(
        &state.db,
        &ws_mgr,
        &state.agent_mgr,
        &state.task_mgr,
        &args.workspace_id,
        &args.template_id,
    )
    .map_err(Into::into)
}

// ---------- Filesystem (editor + file browser + Quick Open) ----------

#[tauri::command]
pub async fn list_dir(
    path: String,
) -> std::result::Result<Vec<crate::files::DirEntry>, String> {
    crate::files::list_dir(&path).map_err(Into::into)
}

#[tauri::command]
pub async fn home_dir() -> std::result::Result<String, String> {
    Ok(dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "/".into()))
}

#[tauri::command]
pub async fn read_file(path: String) -> std::result::Result<String, String> {
    crate::files::read_file(&path).map_err(Into::into)
}

#[derive(Deserialize)]
pub struct WriteFileArgs {
    pub path: String,
    pub content: String,
}

#[tauri::command]
pub async fn write_file(args: WriteFileArgs) -> std::result::Result<(), String> {
    crate::files::write_file(&args.path, &args.content).map_err(Into::into)
}

#[derive(Deserialize)]
pub struct WalkFilesArgs {
    pub root: String,
    #[serde(default)]
    pub max_files: Option<usize>,
}

#[tauri::command]
pub async fn walk_files(
    args: WalkFilesArgs,
) -> std::result::Result<Vec<crate::files::DirEntry>, String> {
    crate::files::walk_files(&args.root, args.max_files.unwrap_or(2000)).map_err(Into::into)
}

// ---------- Tasks ----------

#[tauri::command]
pub async fn create_task(
    state: State<'_, AppState>,
    args: CreateTaskArgs,
) -> std::result::Result<Task, String> {
    state.task_mgr.create(args).map_err(Into::into)
}

#[tauri::command]
pub async fn get_task(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<Task, String> {
    state.task_mgr.get(&id).map_err(Into::into)
}

#[derive(Deserialize, Default)]
pub struct ListTasksArgs {
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
}

#[tauri::command]
pub async fn list_tasks(
    state: State<'_, AppState>,
    args: Option<ListTasksArgs>,
) -> std::result::Result<Vec<Task>, String> {
    let args = args.unwrap_or_default();
    state
        .task_mgr
        .list(
            args.workspace_id.as_deref(),
            args.status.as_deref(),
            args.agent_id.as_deref(),
        )
        .map_err(Into::into)
}

#[tauri::command]
pub async fn update_task(
    state: State<'_, AppState>,
    args: UpdateTaskArgs,
) -> std::result::Result<Task, String> {
    state.task_mgr.update(args).map_err(Into::into)
}

#[tauri::command]
pub async fn delete_task(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<(), String> {
    state.task_mgr.delete(&id).map_err(Into::into)
}

#[derive(Deserialize)]
pub struct AssignTaskArgs {
    pub task_id: String,
    pub agent_id: Option<String>,
}

#[tauri::command]
pub async fn assign_task(
    state: State<'_, AppState>,
    args: AssignTaskArgs,
) -> std::result::Result<Task, String> {
    state
        .task_mgr
        .assign(&args.task_id, args.agent_id.as_deref())
        .map_err(Into::into)
}

// ---------- Memory ----------

#[derive(Deserialize)]
pub struct CreateMemoryArgs {
    pub workspace_id: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub slug: Option<String>,
}

#[tauri::command]
pub async fn create_memory(
    state: State<'_, AppState>,
    args: CreateMemoryArgs,
) -> std::result::Result<Note, String> {
    state
        .memory
        .create(
            &args.workspace_id,
            &args.title,
            &args.body,
            args.tags,
            args.aliases,
            args.slug,
        )
        .map_err(Into::into)
}

#[tauri::command]
pub async fn read_memory(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<Note, String> {
    state.memory.get(&id).map_err(Into::into)
}

#[derive(Deserialize)]
pub struct UpdateMemoryArgs {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub aliases: Option<Vec<String>>,
}

#[tauri::command]
pub async fn update_memory(
    state: State<'_, AppState>,
    args: UpdateMemoryArgs,
) -> std::result::Result<Note, String> {
    state
        .memory
        .update(&args.id, args.title, args.body, args.tags, args.aliases)
        .map_err(Into::into)
}

#[tauri::command]
pub async fn delete_memory(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<(), String> {
    state.memory.delete(&id).map_err(Into::into)
}

#[derive(Deserialize)]
pub struct ListMemoriesArgs {
    pub workspace_id: String,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[tauri::command]
pub async fn list_memories(
    state: State<'_, AppState>,
    args: ListMemoriesArgs,
) -> std::result::Result<Vec<NoteSummary>, String> {
    state
        .memory
        .list(&args.workspace_id, args.tag.as_deref(), args.limit.unwrap_or(50))
        .map_err(Into::into)
}

#[derive(Deserialize)]
pub struct SearchMemoriesArgs {
    pub workspace_id: String,
    pub query: String,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[tauri::command]
pub async fn search_memories(
    state: State<'_, AppState>,
    args: SearchMemoriesArgs,
) -> std::result::Result<Vec<SearchHit>, String> {
    state
        .memory
        .search(&args.workspace_id, &args.query, args.limit.unwrap_or(10))
        .map_err(Into::into)
}

#[tauri::command]
pub async fn find_backlinks(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<Vec<Backlink>, String> {
    state.memory.find_backlinks(&id).map_err(Into::into)
}

#[tauri::command]
pub async fn suggest_connections(
    state: State<'_, AppState>,
    id: String,
    #[allow(non_snake_case)] limit: Option<i64>,
) -> std::result::Result<Vec<SearchHit>, String> {
    state
        .memory
        .suggest_connections(&id, limit.unwrap_or(5))
        .map_err(Into::into)
}

#[tauri::command]
pub async fn memory_graph(
    state: State<'_, AppState>,
    workspace_id: String,
) -> std::result::Result<GraphData, String> {
    state.memory.graph(&workspace_id).map_err(Into::into)
}

// ---------- Swarm: file ownership + review gates ----------

#[derive(Deserialize)]
pub struct ClaimFilesArgs {
    pub workspace_id: String,
    pub task_id: String,
    pub paths: Vec<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
}

#[derive(serde::Serialize)]
pub struct ClaimFilesResult {
    pub claimed: std::collections::HashMap<String, bool>,
    pub blocked_paths: Vec<String>,
    pub all_acquired: bool,
}

#[tauri::command]
pub async fn claim_files(
    state: State<'_, AppState>,
    args: ClaimFilesArgs,
) -> std::result::Result<ClaimFilesResult, String> {
    let mut claimed = std::collections::HashMap::with_capacity(args.paths.len());
    let mut blocked = Vec::new();
    for p in &args.paths {
        let ok = crate::swarm::ownership::acquire(
            &state.db,
            &args.workspace_id,
            p,
            &args.task_id,
            args.agent_id.as_deref(),
        )
        .map_err(Into::<String>::into)?;
        claimed.insert(p.clone(), ok);
        if !ok {
            blocked.push(p.clone());
        }
    }
    let all_acquired = blocked.is_empty();
    Ok(ClaimFilesResult {
        claimed,
        blocked_paths: blocked,
        all_acquired,
    })
}

#[derive(Deserialize)]
pub struct ReleaseFilesArgs {
    pub task_id: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub paths: Option<Vec<String>>,
}

#[tauri::command]
pub async fn release_files(
    state: State<'_, AppState>,
    args: ReleaseFilesArgs,
) -> std::result::Result<usize, String> {
    match args.paths {
        Some(paths) => {
            let ws = args
                .workspace_id
                .ok_or_else(|| "workspace_id required when releasing specific paths".to_string())?;
            let mut released = 0usize;
            for p in &paths {
                if crate::swarm::ownership::release(&state.db, &ws, p, &args.task_id)
                    .map_err(Into::<String>::into)?
                {
                    released += 1;
                }
            }
            Ok(released)
        }
        None => crate::swarm::ownership::release_all_for_task(&state.db, &args.task_id)
            .map_err(Into::into),
    }
}

#[derive(Deserialize, Default)]
pub struct ListFileOwnersArgs {
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
}

#[tauri::command]
pub async fn list_file_owners(
    state: State<'_, AppState>,
    args: Option<ListFileOwnersArgs>,
) -> std::result::Result<Vec<crate::swarm::ownership::Ownership>, String> {
    let args = args.unwrap_or_default();
    if let Some(t) = args.task_id {
        crate::swarm::ownership::list_for_task(&state.db, &t).map_err(Into::into)
    } else if let Some(w) = args.workspace_id {
        crate::swarm::ownership::list_for_workspace(&state.db, &w).map_err(Into::into)
    } else {
        Err("either workspace_id or task_id is required".into())
    }
}

#[derive(Deserialize)]
pub struct OpenReviewGateArgs {
    pub task_id: String,
    #[serde(default)]
    pub reviewer_id: Option<String>,
}

#[tauri::command]
pub async fn open_review_gate(
    state: State<'_, AppState>,
    args: OpenReviewGateArgs,
) -> std::result::Result<crate::swarm::review::ReviewGate, String> {
    crate::swarm::review::open(&state.db, &args.task_id, args.reviewer_id.as_deref())
        .map_err(Into::into)
}

#[derive(Deserialize)]
pub struct VoteReviewGateArgs {
    pub gate_id: String,
    pub verdict: String,
    #[serde(default)]
    pub reason: String,
}

#[tauri::command]
pub async fn vote_review_gate(
    state: State<'_, AppState>,
    args: VoteReviewGateArgs,
) -> std::result::Result<crate::swarm::review::ReviewGate, String> {
    let v = crate::swarm::review::Verdict::parse(&args.verdict)
        .ok_or_else(|| format!("bad verdict: {}", args.verdict))?;
    crate::swarm::review::vote(&state.db, &args.gate_id, v, &args.reason).map_err(Into::into)
}

#[tauri::command]
pub async fn list_review_gates(
    state: State<'_, AppState>,
    task_id: String,
) -> std::result::Result<Vec<crate::swarm::review::ReviewGate>, String> {
    crate::swarm::review::list_for_task(&state.db, &task_id).map_err(Into::into)
}

// ---------- Prompt library (#18) ----------

#[derive(Deserialize)]
pub struct CreatePromptArgs {
    #[serde(default)]
    pub workspace_id: Option<String>,
    pub name: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[tauri::command]
pub async fn create_prompt(
    state: State<'_, AppState>,
    args: CreatePromptArgs,
) -> std::result::Result<crate::prompts::Prompt, String> {
    crate::prompts::create(
        &state.db,
        args.workspace_id.as_deref(),
        &args.name,
        &args.body,
        &args.tags,
    )
    .map_err(Into::into)
}

#[derive(Deserialize)]
pub struct UpdatePromptArgs {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

#[tauri::command]
pub async fn update_prompt(
    state: State<'_, AppState>,
    args: UpdatePromptArgs,
) -> std::result::Result<crate::prompts::Prompt, String> {
    crate::prompts::update(
        &state.db,
        &args.id,
        args.name.as_deref(),
        args.body.as_deref(),
        args.tags.as_deref(),
    )
    .map_err(Into::into)
}

#[tauri::command]
pub async fn delete_prompt(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<(), String> {
    crate::prompts::delete(&state.db, &id).map(|_| ()).map_err(Into::into)
}

#[derive(Deserialize, Default)]
pub struct ListPromptsArgs {
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
}

#[tauri::command]
pub async fn list_prompts(
    state: State<'_, AppState>,
    args: Option<ListPromptsArgs>,
) -> std::result::Result<Vec<crate::prompts::Prompt>, String> {
    let args = args.unwrap_or_default();
    crate::prompts::list(&state.db, args.workspace_id.as_deref(), args.tag.as_deref())
        .map_err(Into::into)
}

#[tauri::command]
pub async fn get_prompt(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<crate::prompts::Prompt, String> {
    crate::prompts::get(&state.db, &id).map_err(Into::into)
}

// ---------- Agent configuration: per-role / per-type system prompts (#19) ----------

#[derive(Deserialize)]
pub struct UpsertRolePromptArgs {
    pub workspace_id: String,
    /// Empty string = "any agent type" (workspace-wide override).
    #[serde(default)]
    pub agent_type: String,
    pub role: String,
    pub prompt: String,
}

#[tauri::command]
pub async fn upsert_role_prompt(
    state: State<'_, AppState>,
    args: UpsertRolePromptArgs,
) -> std::result::Result<crate::swarm::prompts::RolePromptOverride, String> {
    crate::swarm::prompts::upsert(
        &state.db,
        &args.workspace_id,
        &args.agent_type,
        &args.role,
        &args.prompt,
    )
    .map_err(Into::into)
}

#[derive(Deserialize)]
pub struct DeleteRolePromptArgs {
    pub workspace_id: String,
    #[serde(default)]
    pub agent_type: String,
    pub role: String,
}

#[tauri::command]
pub async fn delete_role_prompt(
    state: State<'_, AppState>,
    args: DeleteRolePromptArgs,
) -> std::result::Result<bool, String> {
    crate::swarm::prompts::delete(&state.db, &args.workspace_id, &args.agent_type, &args.role)
        .map_err(Into::into)
}

#[tauri::command]
pub async fn list_role_prompts(
    state: State<'_, AppState>,
    workspace_id: String,
) -> std::result::Result<Vec<crate::swarm::prompts::RolePromptOverride>, String> {
    crate::swarm::prompts::list_for_workspace(&state.db, &workspace_id).map_err(Into::into)
}

#[derive(Deserialize)]
pub struct ResolveRolePromptArgs {
    pub workspace_id: String,
    #[serde(default)]
    pub agent_type: String,
    pub role: String,
}

#[tauri::command]
pub async fn resolve_role_prompt(
    state: State<'_, AppState>,
    args: ResolveRolePromptArgs,
) -> std::result::Result<String, String> {
    let role = crate::swarm::role::Role::parse(&args.role)
        .ok_or_else(|| format!("unknown role: {}", args.role))?;
    crate::swarm::prompts::resolve(&state.db, &args.workspace_id, &args.agent_type, role)
        .map_err(Into::into)
}

// ---------- SSH presets (#14) ----------

#[tauri::command]
pub async fn list_ssh_presets(
    state: State<'_, AppState>,
) -> std::result::Result<Vec<crate::ssh::SshPreset>, String> {
    crate::ssh::list(&state.db).map_err(Into::into)
}

#[tauri::command]
pub async fn create_ssh_preset(
    state: State<'_, AppState>,
    args: crate::ssh::CreatePresetArgs,
) -> std::result::Result<crate::ssh::SshPreset, String> {
    crate::ssh::create(&state.db, args).map_err(Into::into)
}

#[tauri::command]
pub async fn delete_ssh_preset(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<bool, String> {
    crate::ssh::delete(&state.db, &id).map_err(Into::into)
}

#[derive(Deserialize)]
pub struct SpawnSshArgs {
    pub workspace_id: String,
    pub preset_id: String,
}

#[tauri::command]
pub async fn spawn_ssh(
    state: State<'_, AppState>,
    args: SpawnSshArgs,
) -> std::result::Result<crate::agent::Agent, String> {
    crate::ssh::spawn_preset(&state.db, &state.agent_mgr, &args.workspace_id, &args.preset_id)
        .map_err(Into::into)
}

// ---------- Skills ----------

#[tauri::command]
pub async fn list_skills(
    state: State<'_, AppState>,
) -> std::result::Result<Vec<crate::skills::tools::SkillView>, String> {
    Ok(crate::skills::tools::list(&state.skills))
}

#[tauri::command]
pub async fn get_skill(
    state: State<'_, AppState>,
    id: String,
) -> std::result::Result<Option<crate::skills::tools::SkillFull>, String> {
    Ok(crate::skills::tools::get(&state.skills, &id))
}

#[derive(Deserialize)]
pub struct SetSkillEnabledArgs {
    pub id: String,
    pub enabled: bool,
}

#[tauri::command]
pub async fn set_skill_enabled(
    state: State<'_, AppState>,
    args: SetSkillEnabledArgs,
) -> std::result::Result<(), String> {
    crate::skills::tools::set_enabled(&state.db, &state.skills, &args.id, args.enabled)
        .map_err(Into::into)
}

#[tauri::command]
pub async fn reload_skills(
    state: State<'_, AppState>,
) -> std::result::Result<usize, String> {
    state.skills.reload_all().map_err(Into::<String>::into)?;
    Ok(state.skills.entries().len())
}

#[tauri::command]
pub async fn last_skills_trace(
    state: State<'_, AppState>,
    session_id: Option<String>,
) -> std::result::Result<Option<crate::skills::trace::TraceRow>, String> {
    crate::skills::tools::last_trace(&state.db, session_id.as_deref()).map_err(Into::into)
}

#[derive(Deserialize)]
pub struct CreateUserSkillArgs {
    pub id: String,
    pub name: String,
}

#[tauri::command]
pub async fn create_user_skill(
    _state: State<'_, AppState>,
    args: CreateUserSkillArgs,
) -> std::result::Result<String, String> {
    crate::skills::tools::create_user_stub(&args.id, &args.name).map_err(Into::into)
}

#[derive(Deserialize, Default)]
pub struct ImportClaudeSkillsArgs {
    /// Optional list of additional source roots to scan, on top of the
    /// auto-detected ones (`~/.claude/skills`, plugin caches, etc.).
    #[serde(default)]
    pub extra_paths: Vec<String>,
}

#[tauri::command]
pub async fn import_claude_skills(
    state: State<'_, AppState>,
    args: Option<ImportClaudeSkillsArgs>,
) -> std::result::Result<crate::skills::claude_import::ImportReport, String> {
    let args = args.unwrap_or_default();
    let extras: Vec<std::path::PathBuf> =
        args.extra_paths.iter().map(std::path::PathBuf::from).collect();
    let report = crate::skills::claude_import::import(&extras)
        .map_err(Into::<String>::into)?;
    // Re-scan the registry so the imported files appear immediately —
    // the watcher will also pick them up but we don't want to wait on the
    // debounce window (250 ms) for the UI to refresh.
    state.skills.reload_all().map_err(Into::<String>::into)?;
    Ok(report)
}

#[tauri::command]
pub async fn list_claude_skill_sources(
) -> std::result::Result<Vec<crate::skills::claude_import::ClaudeSourceRoot>, String> {
    Ok(crate::skills::claude_import::default_roots())
}

// ---------- Always-On Architect ----------

#[tauri::command]
pub async fn architect_get_config(
    state: State<'_, AppState>,
) -> std::result::Result<crate::architect::supervisor::ArchitectConfig, String> {
    Ok(crate::architect::supervisor::ArchitectConfig::load(&state.db))
}

#[derive(Deserialize)]
pub struct ArchitectSetEnabledArgs {
    pub enabled: bool,
}

#[tauri::command]
pub async fn architect_set_enabled(
    state: State<'_, AppState>,
    args: ArchitectSetEnabledArgs,
) -> std::result::Result<(), String> {
    db::set_setting(
        &state.db,
        "architect.enabled",
        if args.enabled { "true" } else { "false" },
    )
    .map_err(Into::<String>::into)?;
    if args.enabled {
        state.architect.resume();
    }
    Ok(())
}

#[tauri::command]
pub async fn architect_pause(
    state: State<'_, AppState>,
) -> std::result::Result<(), String> {
    state.architect.pause();
    Ok(())
}

#[tauri::command]
pub async fn architect_resume(
    state: State<'_, AppState>,
) -> std::result::Result<(), String> {
    state.architect.resume();
    Ok(())
}

#[tauri::command]
pub async fn architect_decisions(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> std::result::Result<Vec<crate::architect::DecisionRecord>, String> {
    Ok(state.architect.log().snapshot(limit.unwrap_or(50)))
}

// ---------- Project Resolver ----------

#[tauri::command]
pub async fn list_projects(
    state: State<'_, AppState>,
) -> std::result::Result<Vec<crate::project_resolver::ProjectEntry>, String> {
    Ok(state.resolver.list_projects())
}

#[derive(serde::Serialize)]
pub struct RebuildIndexReply {
    pub count: usize,
    pub took_ms: u64,
    pub built_at: String,
}

#[tauri::command]
pub async fn rebuild_project_index(
    state: State<'_, AppState>,
) -> std::result::Result<RebuildIndexReply, String> {
    let started = std::time::Instant::now();
    let idx = state.resolver.rebuild();
    Ok(RebuildIndexReply {
        count: idx.projects.len(),
        took_ms: started.elapsed().as_millis() as u64,
        built_at: idx.built_at,
    })
}

#[derive(Deserialize)]
pub struct ResolveProjectArgs {
    pub query: String,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[tauri::command]
pub async fn resolve_project(
    state: State<'_, AppState>,
    args: ResolveProjectArgs,
) -> std::result::Result<crate::project_resolver::ResolveOutcome, String> {
    Ok(state.resolver.resolve(&args.query, args.cwd.as_deref()))
}

#[derive(Deserialize)]
pub struct OpenProjectArgs {
    pub query: String,
    #[serde(default)]
    pub workspace_name: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OpenProjectReply {
    Opened {
        workspace_id: String,
        workspace_name: String,
        path: String,
        display_name: String,
        confidence: f64,
    },
    Ambiguous {
        candidates: Vec<crate::project_resolver::Candidate>,
    },
    NotFound {
        candidates: Vec<crate::project_resolver::Candidate>,
    },
}

#[tauri::command]
pub async fn open_project(
    state: State<'_, AppState>,
    args: OpenProjectArgs,
) -> std::result::Result<OpenProjectReply, String> {
    let outcome = state.resolver.resolve(&args.query, None);
    match outcome.status {
        crate::project_resolver::ResolveStatus::Found => {
            let pick = outcome
                .candidates
                .first()
                .ok_or_else(|| "found but no candidate".to_string())?
                .clone();
            let ws_mgr = crate::workspace::WorkspaceManager::new(state.db.clone());
            let ws_name = args.workspace_name.unwrap_or_else(|| pick.display_name.clone());
            let existing = ws_mgr
                .list()
                .map_err(Into::<String>::into)?
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
                        ws_mgr
                            .set_paths(&w.id, new_paths.clone())
                            .map_err(Into::<String>::into)?;
                        w.paths = new_paths;
                    }
                    w
                }
                None => ws_mgr
                    .create(&ws_name, vec![pick.path.clone()])
                    .map_err(Into::<String>::into)?,
            };
            db::set_setting(&state.db, "current_workspace_id", &ws.id)
                .map_err(Into::<String>::into)?;
            Ok(OpenProjectReply::Opened {
                workspace_id: ws.id,
                workspace_name: ws.name,
                path: pick.path,
                display_name: pick.display_name,
                confidence: outcome.confidence,
            })
        }
        crate::project_resolver::ResolveStatus::Ambiguous => {
            Ok(OpenProjectReply::Ambiguous {
                candidates: outcome.candidates,
            })
        }
        crate::project_resolver::ResolveStatus::NotFound => {
            Ok(OpenProjectReply::NotFound {
                candidates: outcome.candidates,
            })
        }
    }
}

fn paths_eq(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

#[derive(Deserialize)]
pub struct ProjectAliasArgs {
    pub path: String,
    pub alias: String,
}

#[tauri::command]
pub async fn add_project_alias(
    state: State<'_, AppState>,
    args: ProjectAliasArgs,
) -> std::result::Result<Vec<String>, String> {
    state
        .resolver
        .add_alias(&std::path::PathBuf::from(&args.path), &args.alias)
        .map_err(Into::into)
}

#[tauri::command]
pub async fn remove_project_alias(
    state: State<'_, AppState>,
    args: ProjectAliasArgs,
) -> std::result::Result<Vec<String>, String> {
    state
        .resolver
        .remove_alias(&std::path::PathBuf::from(&args.path), &args.alias)
        .map_err(Into::into)
}
