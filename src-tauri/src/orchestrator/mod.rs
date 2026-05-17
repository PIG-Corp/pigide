pub mod client;
pub mod phantom;
pub mod prompt;
pub mod providers;
pub mod tools;

use crate::agent::AgentManager;
use crate::chat::{self, ChatMessage};
use crate::chat_sessions;
use crate::db::{self, DbPool};
use crate::error::Result;
use crate::events::{EV_CHAT_CHUNK, EV_CHAT_MESSAGE, EV_CHAT_STATUS};
use crate::memory::MemoryService;
use crate::project_resolver::ResolverService;
use crate::skills::router::{route, RouterConfig, RouterMode};
use crate::skills::{compose_system_prompt, SkillRegistry};
use crate::tasks::TaskManager;
use crate::workspace::WorkspaceManager;
use parking_lot::RwLock;
use prompt::SYSTEM_PROMPT_BASE;
use providers::{ChatRequest, LlmProvider};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

const HISTORY_LIMIT: i64 = 60;
const MAX_ITERATIONS: usize = 6;

pub struct Orchestrator {
    db: DbPool,
    ws_mgr: Arc<WorkspaceManager>,
    agent_mgr: Arc<AgentManager>,
    task_mgr: Arc<TaskManager>,
    memory: Arc<MemoryService>,
    skills: Arc<SkillRegistry>,
    resolver: Arc<ResolverService>,
    app: RwLock<Option<AppHandle>>,
}

impl Orchestrator {
    pub fn new(
        db: DbPool,
        ws_mgr: Arc<WorkspaceManager>,
        agent_mgr: Arc<AgentManager>,
        task_mgr: Arc<TaskManager>,
        memory: Arc<MemoryService>,
        skills: Arc<SkillRegistry>,
        resolver: Arc<ResolverService>,
    ) -> Self {
        Self {
            db,
            ws_mgr,
            agent_mgr,
            task_mgr,
            memory,
            skills,
            resolver,
            app: RwLock::new(None),
        }
    }

    pub fn set_app_handle(&self, app: AppHandle) {
        *self.app.write() = Some(app);
    }

    fn build_provider(&self) -> Arc<dyn LlmProvider> {
        providers::build_provider(&self.db)
    }

    fn build_request(
        &self,
        provider: &dyn LlmProvider,
        messages: Vec<Value>,
        tools: Vec<Value>,
    ) -> ChatRequest {
        ChatRequest {
            model: provider.primary_model().to_string(),
            messages,
            tools: Some(tools),
            temperature: 0.2,
            max_tokens: 8192,
            cache: true,
        }
    }

    fn emit_status(&self, state: &str) {
        if let Some(app) = self.app.read().as_ref() {
            let _ = app.emit(EV_CHAT_STATUS, json!({ "state": state }));
        }
    }

    fn emit_message(&self, m: &ChatMessage) {
        if let Some(app) = self.app.read().as_ref() {
            let _ = app.emit(EV_CHAT_MESSAGE, m);
        }
    }

    /// Return `paths[0]` of the current workspace, if any. Used to anchor
    /// the phantom log under `<project>/.pigmemory/phantom_log.jsonl`.
    fn current_workspace_path(&self) -> Option<String> {
        let id = db::get_setting(&self.db, "current_workspace_id")
            .ok()
            .flatten()?;
        if id.is_empty() {
            return None;
        }
        let ws = self.ws_mgr.get(&id).ok()?;
        ws.paths.into_iter().find(|p| !p.is_empty())
    }

    /// Build the dynamic system prompt with the current world state.
    fn build_system_prompt(&self) -> String {
        let mut s = String::from(SYSTEM_PROMPT_BASE);
        s.push_str("\n\n[WORLD STATE]\n");
        let current_id = db::get_setting(&self.db, "current_workspace_id")
            .ok()
            .flatten()
            .unwrap_or_default();
        let workspaces = self.ws_mgr.list().unwrap_or_default();
        s.push_str(&format!(
            "current_workspace_id: {}\n",
            if current_id.is_empty() { "(none)" } else { &current_id }
        ));
        s.push_str("workspaces:\n");
        if workspaces.is_empty() {
            s.push_str("  (none)\n");
        }
        for w in &workspaces {
            let marker = if w.id == current_id { "*" } else { "-" };
            s.push_str(&format!(
                "  {} id={} name={:?} agents={}\n",
                marker, w.id, w.name, w.agent_count
            ));
        }
        s.push_str("agents (across all workspaces):\n");
        let mut any = false;
        for w in &workspaces {
            if let Ok(agents) = self.agent_mgr.list(&w.id) {
                for a in agents {
                    if a.status != "running" {
                        continue;
                    }
                    any = true;
                    s.push_str(&format!(
                        "  - id={} type={} workspace_id={} workspace_name={:?}\n",
                        a.id, a.agent_type, a.workspace_id, w.name
                    ));
                }
            }
        }
        if !any {
            s.push_str("  (no running agents)\n");
        }

        // Tasks for the current workspace (if any).
        if !current_id.is_empty() {
            if let Ok(tasks) = self.task_mgr.list(Some(&current_id), None, None) {
                if !tasks.is_empty() {
                    s.push_str("tasks (current workspace):\n");
                    for t in tasks.iter().take(20) {
                        s.push_str(&format!(
                            "  - id={} status={} title={:?} agent={:?}\n",
                            t.id, t.status, t.title, t.agent_id
                        ));
                    }
                    if tasks.len() > 20 {
                        s.push_str(&format!("  ... and {} more\n", tasks.len() - 20));
                    }
                }
            }
        }
        s
    }

    /// If the user's latest message has any matching memories, prepend a
    /// short context block to the system prompt. Disabled when
    /// `memory.auto_inject` is `"false"` in settings.
    fn build_memory_preamble(&self, user_text: &str, workspace_id: &str) -> Option<String> {
        let on = db::get_setting(&self.db, "memory.auto_inject")
            .ok()
            .flatten()
            .map(|v| v.to_lowercase() != "false")
            .unwrap_or(true);
        if !on {
            return None;
        }
        let hits = self.memory.search(workspace_id, user_text, 3).ok()?;
        let hits: Vec<_> = hits.into_iter().filter(|h| h.score >= 0.1).collect();
        if hits.is_empty() {
            return None;
        }
        let mut out = String::from("\n\n[MEMORY CONTEXT — top relevant notes]\n");
        for h in &hits {
            out.push_str(&format!(
                "- [[{}]] (score {:.2}): {}\n",
                h.slug,
                h.score,
                h.snippet.replace('\n', " ")
            ));
        }
        out.push_str("Use [[wikilinks]] to reference these in your reply.\n");
        Some(out)
    }

    fn build_messages(
        &self,
        session_id: &str,
        last_user_text: Option<&str>,
        phantom_nag: bool,
    ) -> Result<Vec<Value>> {
        let history = chat::list(&self.db, session_id, HISTORY_LIMIT)?;
        let mut msgs = Vec::with_capacity(history.len() + 1);
        let mut sys = self.build_system_prompt();
        if let Some(text) = last_user_text {
            // Inject skills BEFORE memory context so skills can shape the
            // model's reasoning about which memories to look up.
            sys = self.inject_skills(&sys, text, session_id);
            let ws_id = db::get_setting(&self.db, "current_workspace_id")
                .ok()
                .flatten()
                .unwrap_or_default();
            if !ws_id.is_empty() {
                if let Some(p) = self.build_memory_preamble(text, &ws_id) {
                    sys.push_str(&p);
                }
            }
        }
        msgs.push(json!({"role": "system", "content": sys}));

        // OmniRouter rejects role="tool" entirely. We round-trip via plain
        // user/assistant messages so the model still sees a coherent flow:
        //   user(query) -> assistant(text describing tool calls)
        //               -> user([Tool result]: ...)
        //               -> assistant(next step or final reply)
        for m in &history {
            if m.role == "system" {
                continue;
            }
            if m.role == "tool" {
                let tool_name = lookup_tool_name(&history, m.tool_call_id.as_deref());
                let body = truncate_for_model(&m.content, 4000);
                let label = tool_name
                    .map(|n| format!("[Tool result of {}]\n{}", n, body))
                    .unwrap_or_else(|| format!("[Tool result]\n{}", body));
                msgs.push(json!({ "role": "user", "content": label }));
                continue;
            }
            if m.role == "assistant" {
                let has_tc = m
                    .tool_calls
                    .as_ref()
                    .map(|t| !t.is_empty())
                    .unwrap_or(false);
                if has_tc {
                    let mut text = m.content.clone();
                    if !text.is_empty() {
                        text.push_str("\n");
                    }
                    text.push_str("Calling tools:");
                    for tc in m.tool_calls.as_ref().unwrap() {
                        text.push_str(&format!(
                            "\n  - {}({})",
                            tc.function.name, tc.function.arguments
                        ));
                    }
                    msgs.push(json!({
                        "role": "assistant",
                        "content": text,
                    }));
                    continue;
                }
            }
            msgs.push(chat::to_api_message(m));
        }
        if phantom_nag {
            // Hard nag: model just narrated without emitting tool_calls.
            // Inserted as a *system* message at the tail so it sits right
            // before the next assistant turn the model is about to write.
            msgs.push(json!({
                "role": "system",
                "content": phantom::RETRY_NAG,
            }));
        }
        Ok(msgs)
    }

    /// Run a chat turn: persist user message, call the active provider,
    /// dispatch tool calls, persist + emit each step.
    pub async fn run_chat(self: Arc<Self>, text: String) -> Result<()> {
        let session_id = chat_sessions::ensure_current(&self.db)?;
        let user_msg = ChatMessage::user(session_id.clone(), text);
        chat::insert(&self.db, &user_msg)?;
        let _ = chat_sessions::touch(&self.db, &session_id);
        self.emit_message(&user_msg);
        self.emit_status("thinking");

        let provider = self.build_provider();
        let tools = tools::tool_definitions();
        let user_created_at = user_msg.created_at.clone();

        let result = self.tool_loop(provider.as_ref(), &tools, &session_id).await;
        if let Err(e) = result {
            // Roll back any partial assistant/tool messages so the next
            // request doesn't replay an inconsistent history.
            let _ = chat::delete_after(&self.db, &session_id, &user_created_at);
            let m = ChatMessage::system(session_id.clone(), format!("error: {}", e));
            let _ = chat::insert(&self.db, &m);
            self.emit_message(&m);
        }
        self.emit_status("idle");
        Ok(())
    }

    /// Wipe orchestrator chat history for one session.
    pub fn clear(&self, session_id: &str) -> Result<()> {
        chat::clear(&self.db, session_id)?;
        Ok(())
    }

    async fn tool_loop(
        &self,
        provider: &dyn LlmProvider,
        tools: &[Value],
        session_id: &str,
    ) -> Result<()> {
        // The latest user message anchors memory auto-injection.
        let last_user_text = chat::list(&self.db, session_id, HISTORY_LIMIT)?
            .into_iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content);

        // Phantom-tool-call retry latch. We allow at most ONE re-prompt
        // per user turn: once a phantom is detected, the next iteration
        // builds the message list with `phantom_nag=true`. After that
        // turn we either resolved (got a tool_call) or warn the user.
        let mut phantom_nag = false;
        let mut phantom_pending: Option<String> = None;

        for iter in 0..MAX_ITERATIONS {
            // 1. Build a fresh message list (history may have grown via prior
            //    iterations — each tool result becomes a [Tool result] user msg).
            let messages =
                self.build_messages(session_id, last_user_text.as_deref(), phantom_nag)?;

            // 2. Insert an empty assistant placeholder so the UI can stream
            //    text deltas into a bubble.
            let mut placeholder =
                ChatMessage::assistant(session_id.to_string(), String::new(), None);
            chat::insert(&self.db, &placeholder)?;
            self.emit_message(&placeholder);
            let placeholder_id = placeholder.id.clone();

            // 3. Stream the model response via the active provider.
            // Each text delta arrives via mpsc and is forwarded to the UI as
            // a `chat://chunk` event. Streaming runs concurrently with the
            // provider future so the UI updates incrementally.
            let app = self.app.read().clone();
            let id_for_cb = placeholder_id.clone();
            let req = self.build_request(provider, messages, tools.to_vec());
            let (delta_tx, mut delta_rx) =
                tokio::sync::mpsc::unbounded_channel::<String>();
            let forwarder = tokio::spawn(async move {
                while let Some(delta) = delta_rx.recv().await {
                    if let Some(app) = &app {
                        let _ = app.emit(
                            EV_CHAT_CHUNK,
                            json!({ "id": id_for_cb, "delta": delta }),
                        );
                    }
                }
            });
            let stream_result = provider.chat_stream(req, delta_tx).await;
            let _ = forwarder.await;
            let assembled = match stream_result {
                Ok(m) => m,
                Err(e) => {
                    let _ = chat::delete_after(&self.db, session_id, &placeholder.created_at);
                    return Err(e);
                }
            };

            let final_text = assembled.content.clone().unwrap_or_default();
            let has_tools = assembled
                .tool_calls
                .as_ref()
                .map(|t| !t.is_empty())
                .unwrap_or(false);

            // 4. Patch placeholder with final content + tool_calls.
            placeholder.content = final_text.clone();
            placeholder.tool_calls = assembled.tool_calls.clone();
            {
                let conn = self.db.get()?;
                conn.execute(
                    "DELETE FROM orchestrator_chat WHERE id=?1",
                    [&placeholder_id],
                )?;
            }
            chat::insert(&self.db, &placeholder)?;
            self.emit_message(&placeholder);

            // 4b. Phantom-tool-call detection.
            //
            // The model narrated an action ("I'll send to agent",
            // "сейчас вызову X") but emitted no tool_calls. Re-prompt
            // once with a hard nag; if the SECOND attempt also fails,
            // surface a visible warning to the user.
            let is_phantom = phantom::is_phantom(&final_text, has_tools);
            if is_phantom && !phantom_nag {
                // First detection — log, latch, and loop again with the nag.
                let model = provider.primary_model().to_string();
                let ws_path = self.current_workspace_path().unwrap_or_default();
                phantom::append_event(
                    &phantom::resolve_log_root(Some(&ws_path)),
                    &phantom::PhantomEvent::new(&model, &final_text, false, false),
                );
                tracing::warn!(
                    target: "orchestrator::phantom",
                    "phantom tool_call detected, re-prompting once: {:?}",
                    final_text.chars().take(120).collect::<String>()
                );
                phantom_nag = true;
                phantom_pending = Some(final_text.clone());
                continue;
            }
            if phantom_nag {
                // We were in retry mode. Record the outcome.
                let model = provider.primary_model().to_string();
                let ws_path = self.current_workspace_path().unwrap_or_default();
                let resolved = !is_phantom;
                let snippet = phantom_pending.take().unwrap_or_default();
                phantom::append_event(
                    &phantom::resolve_log_root(Some(&ws_path)),
                    &phantom::PhantomEvent::new(&model, &snippet, true, resolved),
                );
                phantom_nag = false;
                if !resolved {
                    // Second failure — warn the user and stop the turn.
                    let warn = ChatMessage::system(
                        session_id.to_string(),
                        "warning: model described a tool call but did not emit \
                         one (phantom tool_call). The action did not execute. \
                         Try rephrasing your request."
                            .to_string(),
                    );
                    chat::insert(&self.db, &warn)?;
                    self.emit_message(&warn);
                    return Ok(());
                }
                // else fall through — model recovered, treat normally.
            }

            if !has_tools {
                // Model is done.
                return Ok(());
            }

            // 5. Execute tool_calls; persist each as a tool message in DB so
            //    next iteration's build_messages can transform it into a
            //    [Tool result] user message for the model.
            self.emit_status("tool");
            for call in assembled.tool_calls.unwrap_or_default() {
                let args: Value = serde_json::from_str(&call.function.arguments)
                    .unwrap_or_else(|_| Value::Object(Default::default()));
                let app_handle = self.app.read().clone();
                let result = match tools::dispatch(
                    &self.db,
                    &self.ws_mgr,
                    &self.agent_mgr,
                    &self.task_mgr,
                    &self.memory,
                    &self.resolver,
                    app_handle.as_ref(),
                    &call.function.name,
                    &args,
                )
                .await
                {
                    Ok(v) => v,
                    Err(e) => json!({"error": e.to_string()}),
                };
                let tool_msg =
                    ChatMessage::tool(session_id.to_string(), &call.id, result.to_string());
                chat::insert(&self.db, &tool_msg)?;
                self.emit_message(&tool_msg);
            }

            // Loop continues: model gets a chance to plan the next phase.
            self.emit_status("thinking");
            tracing::debug!("orchestrator: completed iteration {}", iter + 1);
        }

        // Hit the iteration ceiling. Surface a system note in the chat.
        let m = ChatMessage::system(
            session_id.to_string(),
            format!("Reached max iterations ({}); stopping.", MAX_ITERATIONS),
        );
        chat::insert(&self.db, &m)?;
        self.emit_message(&m);
        Ok(())
    }
}

/// Find the function name of the tool_call referenced by `tool_call_id` by
/// scanning previous assistant messages.
fn lookup_tool_name(
    history: &[crate::chat::ChatMessage],
    tool_call_id: Option<&str>,
) -> Option<String> {
    let id = tool_call_id?;
    for m in history.iter().rev() {
        if m.role != "assistant" {
            continue;
        }
        if let Some(tcs) = &m.tool_calls {
            for tc in tcs {
                if tc.id == id {
                    return Some(tc.function.name.clone());
                }
            }
        }
    }
    None
}

fn truncate_for_model(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars).collect();
    format!("{}…[truncated]", head)
}

impl Orchestrator {
    /// Read router configuration from settings.
    fn router_config(&self) -> RouterConfig {
        let mode = db::get_setting(&self.db, "skills.router.mode")
            .ok()
            .flatten()
            .map(|v| RouterMode::from_str(&v))
            .unwrap_or_default();
        let max_skills = db::get_setting(&self.db, "skills.router.max_skills")
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4);
        let token_budget = db::get_setting(&self.db, "skills.router.token_budget")
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4000);
        let llm_fallback = db::get_setting(&self.db, "skills.router.llm_fallback")
            .ok()
            .flatten()
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        RouterConfig {
            mode,
            max_skills,
            token_budget,
            llm_fallback,
            force_include: Vec::new(),
            mention_prefixes: vec!["@skill:".into(), "@".into()],
        }
    }

    /// Compose the active skills into the system prompt and persist a
    /// per-turn trace. Returns the augmented prompt.
    fn inject_skills(&self, sys: &str, user_text: &str, session_id: &str) -> String {
        let cfg = self.router_config();
        if matches!(cfg.mode, RouterMode::Off) {
            return sys.to_string();
        }
        let active = self.skills.active();
        if active.is_empty() {
            return sys.to_string();
        }

        // Heuristic for "dispatching": if the user message smells like a
        // dispatch (mentions agents/builder/send/выдай), we promote the
        // meta-skill so the brief comes out production-quality.
        let lower = user_text.to_lowercase();
        let dispatching = [
            "send_to_agent",
            "builder",
            "сборщик",
            "выдай задание",
            "tell the agent",
            "tell the builder",
            "instruct the agent",
            "напиши промпт",
            "draft a prompt",
        ]
        .iter()
        .any(|n| lower.contains(n));

        let routed = route(&active, user_text, &cfg, dispatching);
        if routed.selected.is_empty() {
            // Still write a trace row so the UI can show "nothing matched".
            let _ = crate::skills::trace::record(&self.db, session_id, &routed, 0);
            return sys.to_string();
        }

        // Build a context map. Today the only auto-populated variable is
        // `user_message`; future skills may rely on it.
        let mut ctx: BTreeMap<String, Value> = BTreeMap::new();
        ctx.insert("user_message".into(), Value::String(user_text.to_string()));

        // Resolve selected ids -> Skill.
        let by_id: std::collections::HashMap<String, &crate::skills::Skill> =
            active.iter().map(|s| (s.id.clone(), s)).collect();
        let ordered: Vec<&crate::skills::Skill> = routed
            .selected
            .iter()
            .filter_map(|s| by_id.get(&s.id).copied())
            .collect();

        let char_budget = cfg.token_budget.saturating_mul(4);
        let res = compose_system_prompt(sys, &ordered, &ctx, char_budget);
        let _ = crate::skills::trace::record(
            &self.db,
            session_id,
            &routed,
            res.composed_chars,
        );
        res.prompt
    }
}
