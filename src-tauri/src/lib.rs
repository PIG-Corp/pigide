pub mod agent;
pub mod agentd;
pub mod architect;
pub mod chat;
pub mod chat_queue;
pub mod chat_queue_worker;
pub mod chat_sessions;
pub mod commands;
pub mod db;
pub mod deeplink;
pub mod error;
pub mod events;
pub mod files;
pub mod ipc;
pub mod layout;
pub mod mcp;
pub mod memory;
pub mod orchestrator;
pub mod path_suggest;
pub mod project_resolver;
pub mod prompts;
pub mod rooms;
pub mod sanitize;
pub mod secrets;
pub mod skills;
pub mod ssh;
pub mod state;
pub mod swarm;
pub mod tasks;
pub mod voice;
#[cfg(feature = "watcher")]
pub mod watcher;
pub mod workspace;

use crate::agent::AgentManager;
use crate::mcp::server::McpServerHandle;
use crate::memory::MemoryService;
use crate::orchestrator::Orchestrator;
use crate::project_resolver::ResolverService;
use crate::skills::registry::default_sources;
use crate::skills::SkillRegistry;
use crate::state::AppState;
use crate::tasks::TaskManager;
use crate::voice::VoicePipeline;
use crate::workspace::WorkspaceManager;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::Manager;

/// Load `.env` from the first existing location. Lines already in the
/// environment take precedence — this is "fill in the blanks", not "override".
fn load_dotenv() {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(explicit) = std::env::var("PIGIDE_ENV_FILE") {
        candidates.push(PathBuf::from(explicit));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join(".env"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(".env"));
            if let Some(parent) = dir.parent() {
                candidates.push(parent.join(".env"));
            }
        }
    }
    if let Some(home) = dirs::config_dir() {
        candidates.push(home.join("pigide").join(".env"));
    }

    for path in candidates {
        if path.is_file() {
            match dotenvy::from_path(&path) {
                Ok(_) => {
                    eprintln!("pigide: loaded env from {}", path.display());
                    return;
                }
                Err(e) => {
                    eprintln!("pigide: failed to read {}: {}", path.display(), e);
                }
            }
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Load .env BEFORE anything else reads env (logging filter, watcher, etc).
    // Search order, first hit wins:
    //   1. $PIGIDE_ENV_FILE                       — explicit override
    //   2. ./.env                                 — current working dir (dev)
    //   3. <exe_dir>/.env                         — next to the binary (release)
    //   4. <exe_dir>/../.env                      — bin/ → repo root layout
    //   5. ~/.config/pigide/.env                  — XDG user config
    load_dotenv();
    // On Linux/Wayland, webkit2gtk frequently hits "Error 71 (Protocol error)"
    // unless we force the X11 backend (XWayland) and disable accelerated
    // compositing paths that don't survive the Wayland handshake. This must
    // be set BEFORE tauri::Builder constructs any windows.
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("GDK_BACKEND").is_none() {
            std::env::set_var("GDK_BACKEND", "x11");
        }
        if std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE").is_none() {
            std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
        }
        if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
    }

    // Init logging.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "pigide=info,info".into()),
        )
        .try_init();

    // Init state.
    let pool = match db::init_pool() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("db init failed: {}", e);
            std::process::exit(1);
        }
    };

    let agent_mgr = Arc::new(AgentManager::new(pool.clone()));
    // Persisted `running` rows are NOT immediately wiped on launch; instead
    // `restore_session` (below in `setup`) attempts to re-spawn each agent so
    // workspace layouts (which reference agent ids) survive a restart. Any
    // failures get marked exited inside that helper.
    let ws_mgr = Arc::new(WorkspaceManager::new(pool.clone()));
    let task_mgr = Arc::new(TaskManager::new(pool.clone()));
    let memory = Arc::new(MemoryService::new(pool.clone(), ws_mgr.clone()));
    let chat_buffer = Arc::new(crate::memory::ingest::chat_chunk::ChatBuffer::new());
    let smart_worker = Arc::new(crate::memory::ingest::smart::SmartIngestWorker::new(
        pool.clone(),
        memory.clone(),
        ws_mgr.clone(),
    ));
    smart_worker.clone().start();
    if let Err(e) = crate::memory::migration::run_once(&pool) {
        tracing::warn!("memory phase0 disk migration failed: {}", e);
    }
    let skills = SkillRegistry::new();
    // Skills DB table (idempotent — safe to call before any orchestration).
    if let Err(e) = crate::skills::trace::ensure_table(&pool) {
        tracing::warn!("skills trace table: {}", e);
    }
    let resolver = Arc::new(ResolverService::new(pool.clone(), ws_mgr.clone()));
    let orchestrator = Arc::new(Orchestrator::new(
        pool.clone(),
        ws_mgr.clone(),
        agent_mgr.clone(),
        task_mgr.clone(),
        memory.clone(),
        skills.clone(),
        resolver.clone(),
    ));
    let voice = Arc::new(VoicePipeline::new(pool.clone()));
    let architect = Arc::new(crate::architect::Architect::new(
        pool.clone(),
        agent_mgr.clone(),
        task_mgr.clone(),
        ws_mgr.clone(),
    ));
    // Chat-queue worker. Single consumer that drains queued user messages
    // and feeds them to the orchestrator one at a time. Idempotent table
    // creation defends against the migration not having run yet (e.g. tests).
    if let Err(e) = crate::chat_queue::ensure_table(&pool) {
        tracing::warn!("chat_queue ensure_table: {}", e);
    }
    let chat_queue =
        crate::chat_queue_worker::ChatQueueWorker::new(pool.clone(), orchestrator.clone());

    // Single-instance IPC: lets `pigide-cli .` hand a workspace path to the
    // already-running app via Unix socket. Failures are logged, not fatal.
    crate::ipc::spawn(pool.clone());

    // Auto-create default workspace if none exist.
    if ws_mgr.list().map(|v| v.is_empty()).unwrap_or(false) {
        let _ = ws_mgr.create("default", Vec::new());
    }

    // Configure skills sources (built-in + user + first workspace).
    {
        let workspace_dir: Option<PathBuf> = db::get_setting(&pool, "current_workspace_id")
            .ok()
            .flatten()
            .and_then(|id| ws_mgr.get(&id).ok())
            .and_then(|w| w.paths.first().cloned().map(PathBuf::from));
        skills.set_sources(default_sources(None, workspace_dir));
        if let Err(e) = skills.reload_all() {
            tracing::warn!("skills initial scan: {}", e);
        }
        if let Ok(overrides) = crate::skills::tools::load_overrides(&pool) {
            skills.set_overrides(overrides);
        }
    }

    let state = AppState {
        db: pool,
        agent_mgr: agent_mgr.clone(),
        orchestrator: orchestrator.clone(),
        chat_queue: chat_queue.clone(),
        voice: voice.clone(),
        task_mgr: task_mgr.clone(),
        memory: memory.clone(),
        chat_buffer: chat_buffer.clone(),
        mcp: Arc::new(McpServerHandle::default()),
        skills: skills.clone(),
        architect: architect.clone(),
        resolver: resolver.clone(),
        #[cfg(feature = "watcher")]
        watcher: parking_lot::RwLock::new(None),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_deep_link::init())
        .manage(state)
        .setup(move |app| {
            // Hand AppHandle to managers that emit events.
            agent_mgr.set_app_handle(app.handle().clone());
            agent_mgr.set_ingest_handles(memory.clone(), chat_buffer.clone());
            orchestrator.set_app_handle(app.handle().clone());
            voice.set_app_handle(app.handle().clone());
            architect.set_app_handle(app.handle().clone());
            architect.spawn_loop();
            chat_queue.set_app_handle(app.handle().clone());
            chat_queue.clone().spawn();

            // Watcher (feature-gated). Disabled silently if GEMINI_API_KEY
            // is missing — the supervisor logs a single warning and the
            // rest of the app keeps working unaware.
            #[cfg(feature = "watcher")]
            {
                let st = app.state::<AppState>();
                match crate::watcher::Watcher::new(st.db.clone(), st.agent_mgr.clone()) {
                    Ok(w) => {
                        let w = Arc::new(w);
                        *st.watcher.write() = Some(w.clone());
                        w.spawn(app.handle().clone());
                        tracing::info!("watcher: spawned (gemma-3-4b-it)");
                    }
                    Err(e) => {
                        tracing::warn!("watcher disabled: {}", e);
                    }
                }
            }

            // Wire the MCP handle into the agent manager BEFORE
            // `restore_session` runs so re-spawned Claude tiles also pick up
            // `--mcp-config`. The handle starts empty; if autostart is
            // enabled we kick the server below and existing tiles auto-grab
            // the freshly-bound URL on next spawn.
            {
                let st = app.state::<AppState>();
                agent_mgr.set_mcp_handle(st.mcp.clone());
            }

            // PigMCP autostart. Defaults ON so spawned Claude tiles always
            // see the `pigide` server in `/mcp`. Setting `mcp.autostart=false`
            // (e.g. when an external supervisor manages the port) skips it.
            {
                let st = app.state::<AppState>();
                let on = db::get_setting(&st.db, "mcp.autostart")
                    .ok()
                    .flatten()
                    .map(|v| v.eq_ignore_ascii_case("true"))
                    .unwrap_or(true);
                if on {
                    let port: u16 = db::get_setting(&st.db, "mcp.port")
                        .ok()
                        .flatten()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(20129);
                    let handle = st.mcp.clone();
                    let mcp_state = crate::mcp::server::McpState {
                        db: st.db.clone(),
                        ws_mgr: Arc::new(crate::workspace::WorkspaceManager::new(st.db.clone())),
                        agent_mgr: st.agent_mgr.clone(),
                        task_mgr: st.task_mgr.clone(),
                        memory: st.memory.clone(),
                        resolver: st.resolver.clone(),
                        #[cfg(feature = "watcher")]
                        watcher: st.watcher.read().clone(),
                    };
                    let bind = std::net::SocketAddr::from(([127, 0, 0, 1], port));
                    tauri::async_runtime::spawn(async move {
                        match crate::mcp::server::start(handle, mcp_state, bind).await {
                            Ok(addr) => tracing::info!("MCP autostart: {}", addr),
                            Err(e) => tracing::warn!("MCP autostart: {}", e),
                        }
                    });
                }
            }

            // Deep links: subscribe to the plugin's URL stream and re-emit
            // a structured `deep-link://nav` event the frontend can act on.
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let app_handle = app.handle().clone();
                let _ = app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        let s = url.to_string();
                        if let Err(e) = crate::deeplink::dispatch(&app_handle, &s) {
                            tracing::warn!("deep_link dispatch failed: {}", e);
                        }
                    }
                });
            }

            // Session snapshot restore: re-spawn agents the DB still believes
            // were running. Must happen AFTER `set_app_handle` so the new PTYs
            // emit stdout to the frontend. Done on a background thread so
            // boot is never blocked by a slow respawn.
            {
                let mgr = agent_mgr.clone();
                std::thread::spawn(move || match mgr.restore_session() {
                    Ok((restored, failed)) => {
                        tracing::info!("session restore: {} restored, {} failed", restored, failed);
                    }
                    Err(e) => tracing::warn!("session restore: {}", e),
                });
            }

            // Spawn a memory watcher for the current workspace, if any.
            let current_ws = db::get_setting(&app.state::<AppState>().db, "current_workspace_id")
                .ok()
                .flatten();
            if let Some(ws_id) = current_ws {
                if let Err(e) = crate::memory::watcher::spawn(memory.clone(), ws_mgr.clone(), ws_id)
                {
                    tracing::warn!("memory watcher: {}", e);
                }
            }

            // Skills hot-reload watchers (built-in + user + workspace).
            {
                let sources = skills.sources();
                if let Err(e) =
                    crate::skills::watcher::spawn_all(app.handle().clone(), skills.clone(), sources)
                {
                    tracing::warn!("skills watcher: {}", e);
                }
            }

            // Voice global hotkey. Failures (Wayland-only env, accelerator
            // conflict) emit `voice://hotkey-error` rather than aborting setup.
            // Opt-in only: set `voice.hotkey_enabled = "true"` to register
            // a global accelerator. Many compositors steal Alt+Space (window
            // menu), so the default is OFF to avoid hanging the WM at startup.
            let hotkey_on = db::get_setting(&app.state::<AppState>().db, "voice.hotkey_enabled")
                .ok()
                .flatten()
                .map(|v| v.to_lowercase() == "true")
                .unwrap_or(false);
            if hotkey_on {
                if let Err(e) = crate::voice::hotkey::register(
                    &app.handle().clone(),
                    voice.clone(),
                    app.state::<AppState>().db.clone(),
                ) {
                    tracing::warn!("voice hotkey: {}", e);
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Linux/Windows graceful close path: the OS sends a per-window
            // Destroyed event before the loop tears down. macOS Cmd+Q does
            // NOT take this path — see the `RunEvent::ExitRequested`/`Exit`
            // handler in `.run(...)` below for the platform-correct hook.
            if let tauri::WindowEvent::Destroyed = event {
                if let Some(state) = window.app_handle().try_state::<AppState>() {
                    state.agent_mgr.kill_all();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            commands::list_workspaces,
            commands::get_workspace,
            commands::create_workspace,
            commands::rename_workspace,
            commands::delete_workspace,
            commands::update_layout,
            commands::set_current_workspace,
            commands::get_current_workspace,
            commands::spawn_agent,
            commands::kill_agent,
            commands::write_to_agent,
            commands::resize_agent,
            commands::list_agents,
            commands::agent_log_tail,
            commands::restore_session,
            commands::list_chat,
            commands::send_chat,
            commands::suggest_paths,
            commands::clear_chat,
            commands::stop_chat,
            commands::list_chat_queue,
            commands::cancel_chat_queue_item,
            commands::chat_queue_set_continue_on_error,
            commands::chat_queue_get_continue_on_error,
            commands::list_chat_sessions,
            commands::create_chat_session,
            commands::rename_chat_session,
            commands::delete_chat_session,
            commands::get_current_session,
            commands::set_current_session,
            commands::set_chat_scope,
            commands::set_setting,
            commands::get_setting,
            commands::provider_info,
            commands::provider_test_connection,
            commands::provider_list,
            commands::provider_create,
            commands::provider_update,
            commands::provider_delete,
            commands::provider_set_model,
            commands::provider_set_active,
            commands::provider_fetch_models,
            commands::provider_probe_models,
            commands::start_voice,
            commands::stop_voice,
            commands::cancel_voice,
            commands::voice_list_models,
            commands::voice_set_model,
            commands::voice_dict_list,
            commands::voice_dict_add,
            commands::voice_dict_update,
            commands::voice_dict_delete,
            commands::voice_history_list,
            commands::voice_history_search,
            commands::voice_history_delete,
            commands::voice_stats,
            commands::mcp_status,
            commands::mcp_start,
            commands::mcp_stop,
            commands::mcp_list_keys,
            commands::mcp_create_key,
            commands::mcp_revoke_key,
            commands::mcp_register_cwd,
            commands::list_room_templates,
            commands::apply_room_template,
            commands::list_dir,
            commands::browse_dir,
            commands::home_dir,
            commands::read_file,
            commands::write_file,
            commands::walk_files,
            commands::create_task,
            commands::get_task,
            commands::list_tasks,
            commands::update_task,
            commands::delete_task,
            commands::assign_task,
            commands::create_memory,
            commands::read_memory,
            commands::update_memory,
            commands::delete_memory,
            commands::list_memories,
            commands::search_memories,
            commands::find_backlinks,
            commands::suggest_connections,
            commands::memory_graph,
            commands::memory_smart_status,
            commands::memory_set_smart_setting,
            commands::memory_reingest_note,
            commands::claim_files,
            commands::release_files,
            commands::list_file_owners,
            commands::open_review_gate,
            commands::vote_review_gate,
            commands::list_review_gates,
            commands::list_skills,
            commands::get_skill,
            commands::set_skill_enabled,
            commands::reload_skills,
            commands::last_skills_trace,
            commands::create_user_skill,
            commands::import_claude_skills,
            commands::list_claude_skill_sources,
            commands::architect_get_config,
            commands::architect_set_enabled,
            commands::architect_pause,
            commands::architect_resume,
            commands::architect_decisions,
            commands::create_prompt,
            commands::update_prompt,
            commands::delete_prompt,
            commands::list_prompts,
            commands::get_prompt,
            commands::upsert_role_prompt,
            commands::delete_role_prompt,
            commands::list_role_prompts,
            commands::resolve_role_prompt,
            commands::list_ssh_presets,
            commands::create_ssh_preset,
            commands::delete_ssh_preset,
            commands::spawn_ssh,
            commands::list_projects,
            commands::rebuild_project_index,
            commands::resolve_project,
            commands::open_project,
            commands::add_project_alias,
            commands::remove_project_alias,
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app_handle, event| match event {
            // macOS Cmd+Q delivers `applicationWillTerminate` which tao
            // forwards as `Event::LoopDestroyed` → `RunEvent::Exit` WITHOUT
            // first emitting per-window `Destroyed` events. If we only
            // hooked `WindowEvent::Destroyed`, the agent manager would
            // never see `kill_all()` on Cmd+Q — and when the runtime then
            // dropped `AppState`, each `AgentRuntime`'s master fd would
            // close, the per-agent reader threads would observe EOF, and
            // (with `shutting_down=false`) `handle_reader_eof` would write
            // `UPDATE agents SET status='exited'`, wiping the rows the
            // next launch needed for `restore_session` to rehydrate.
            //
            // Hooking `RunEvent::ExitRequested` (also synthesized on the
            // graceful path) raises `shutting_down` BEFORE Drop chains run,
            // so reader-thread cleanup correctly skips the DB write —
            // matching the SIGKILL/Btop path that already worked because
            // the process died before any cleanup could execute.
            tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit => {
                if let Some(state) = app_handle.try_state::<AppState>() {
                    state.agent_mgr.kill_all();
                }
            }
            _ => {}
        });
}
