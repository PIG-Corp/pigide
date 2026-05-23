//! Supervisor: tokio-loop, который тикает и вызывает classifier+policy для
//! каждого running-агента в активном workspace.
//!
//! Loop запускается из `lib.rs` после `Tauri::Builder.setup()`. Включается
//! db-настройкой `architect.enabled = "true"` (по умолчанию OFF). Любые
//! автодействия сначала пишутся в DecisionLog (последние 200 записей в RAM
//! + событие в UI), потом исполняются.

use crate::agent::AgentManager;
use crate::architect::classifier::{classify, AgentSignal};
use crate::architect::policy::{decide, extract_quote, PolicyDecision};
use crate::db::{self, DbPool};
use crate::tasks::TaskManager;
use crate::workspace::WorkspaceManager;
use chrono::Utc;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::time::interval;

/// Tauri-event для UI: одна запись лога решений.
pub const EV_ARCHITECT_DECISION: &str = "architect://decision";
/// Tauri-event для UI: bulk snapshot классификаций (id -> signal) — позволяет
/// тайлам красить бейдж без необходимости подписываться на каждое решение.
pub const EV_ARCHITECT_SIGNAL: &str = "architect://signal";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ArchitectConfig {
    pub enabled: bool,
    pub poll_interval_ms: u64,
    pub idle_done_after_secs: u64,
    pub stuck_after_secs: u64,
    pub auto_confirm: bool,
    pub auto_assign_next: bool,
}

impl Default for ArchitectConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            poll_interval_ms: 2000,
            idle_done_after_secs: 8,
            stuck_after_secs: 60,
            auto_confirm: true,
            auto_assign_next: true,
        }
    }
}

impl ArchitectConfig {
    pub fn load(db: &DbPool) -> Self {
        let mut c = Self::default();
        if let Ok(Some(v)) = db::get_setting(db, "architect.enabled") {
            c.enabled = v.eq_ignore_ascii_case("true");
        }
        if let Ok(Some(v)) = db::get_setting(db, "architect.poll_interval_ms") {
            if let Ok(n) = v.parse::<u64>() {
                c.poll_interval_ms = n.clamp(500, 30_000);
            }
        }
        if let Ok(Some(v)) = db::get_setting(db, "architect.idle_done_after_secs") {
            if let Ok(n) = v.parse::<u64>() {
                c.idle_done_after_secs = n.clamp(2, 600);
            }
        }
        if let Ok(Some(v)) = db::get_setting(db, "architect.stuck_after_secs") {
            if let Ok(n) = v.parse::<u64>() {
                c.stuck_after_secs = n.clamp(10, 1800);
            }
        }
        if let Ok(Some(v)) = db::get_setting(db, "architect.auto_confirm") {
            c.auto_confirm = v.eq_ignore_ascii_case("true");
        }
        if let Ok(Some(v)) = db::get_setting(db, "architect.auto_assign_next") {
            c.auto_assign_next = v.eq_ignore_ascii_case("true");
        }
        c
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DecisionRecord {
    pub at: String,
    pub agent_id: String,
    pub workspace_id: String,
    pub signal: &'static str,
    pub kind: &'static str,
    pub reason: String,
    pub quote: Option<String>,
    pub auto_executed: bool,
}

#[derive(Default)]
pub struct DecisionLog {
    inner: RwLock<Vec<DecisionRecord>>,
}

impl DecisionLog {
    pub fn push(&self, rec: DecisionRecord) {
        let mut g = self.inner.write();
        g.push(rec);
        // Ring-buffer на 200 записей.
        let len = g.len();
        if len > 200 {
            g.drain(0..(len - 200));
        }
    }

    pub fn snapshot(&self, limit: usize) -> Vec<DecisionRecord> {
        let g = self.inner.read();
        let n = g.len();
        let start = n.saturating_sub(limit);
        g[start..].to_vec()
    }
}

#[derive(Default)]
struct PerAgentState {
    /// Агент уже получил один пинг "status?".
    pinged_stuck: bool,
    /// Один авто-retry для error уже сделан.
    retried_error: bool,
    /// Последний классифицированный signal — чтобы не спамить decisions
    /// одинаковыми событиями.
    last_signal: Option<AgentSignal>,
    /// Эскалация уже отправлена для текущего "застревания" — не дублируем.
    escalated_for_signal: Option<AgentSignal>,
}

pub struct Architect {
    db: DbPool,
    agent_mgr: Arc<AgentManager>,
    task_mgr: Arc<TaskManager>,
    #[allow(dead_code)]
    ws_mgr: Arc<WorkspaceManager>,
    app: RwLock<Option<AppHandle>>,
    log: Arc<DecisionLog>,
    /// Hard kill switch — при `false` loop пропускает все тики.
    paused: Arc<AtomicBool>,
    state: Arc<RwLock<HashMap<String, PerAgentState>>>,
}

impl Architect {
    pub fn new(
        db: DbPool,
        agent_mgr: Arc<AgentManager>,
        task_mgr: Arc<TaskManager>,
        ws_mgr: Arc<WorkspaceManager>,
    ) -> Self {
        Self {
            db,
            agent_mgr,
            task_mgr,
            ws_mgr,
            app: RwLock::new(None),
            log: Arc::new(DecisionLog::default()),
            paused: Arc::new(AtomicBool::new(false)),
            state: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn set_app_handle(&self, app: AppHandle) {
        *self.app.write() = Some(app);
    }

    pub fn log(&self) -> Arc<DecisionLog> {
        self.log.clone()
    }

    /// Hard pause — loop продолжает крутиться, но не делает решений. Снять
    /// — `resume()`.
    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Запустить tokio-таску супервизора. Безопасно вызывать несколько раз —
    /// внутренние spawned-task сами проверяют `enabled` на каждом тике, так
    /// что перезапуск не нужен; если уже запущена — этот метод просто
    /// сбрасывает паузу.
    pub fn spawn_loop(self: &Arc<Self>) {
        self.resume();
        let me = self.clone();
        // NB: must be `tauri::async_runtime::spawn`, NOT `tokio::spawn`.
        // This is invoked from the Tauri `setup` callback, which runs on the
        // main thread *outside* any tokio runtime context — `tokio::spawn`
        // would panic with "there is no reactor running".
        tauri::async_runtime::spawn(async move {
            let mut tick = interval(Duration::from_millis(2000));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let cfg = ArchitectConfig::load(&me.db);
                if !cfg.enabled || me.is_paused() {
                    continue;
                }
                // Если interval поменяли в settings — пересоздаём.
                if tick.period() != Duration::from_millis(cfg.poll_interval_ms) {
                    tick = interval(Duration::from_millis(cfg.poll_interval_ms));
                    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                }
                if let Err(e) = me.tick(&cfg) {
                    tracing::warn!("architect tick: {}", e);
                }
            }
        });
    }

    fn current_workspace_id(&self) -> Option<String> {
        db::get_setting(&self.db, "current_workspace_id")
            .ok()
            .flatten()
    }

    fn tick(&self, cfg: &ArchitectConfig) -> crate::error::Result<()> {
        let Some(ws_id) = self.current_workspace_id() else {
            return Ok(());
        };
        let agents = self.agent_mgr.list(&ws_id)?;
        let mut signals: Vec<(String, &'static str)> = Vec::with_capacity(agents.len());

        for a in &agents {
            if a.status != "running" {
                continue;
            }
            let tail_bytes = self
                .agent_mgr
                .read_log_tail(&a.id, 8 * 1024)
                .unwrap_or_default();
            let tail = String::from_utf8_lossy(&tail_bytes).to_string();
            let idle_for = self.agent_mgr.last_stdout_age(&a.id);

            let signal = classify(
                &tail,
                idle_for,
                Duration::from_secs(cfg.idle_done_after_secs),
                Duration::from_secs(cfg.stuck_after_secs),
            );
            signals.push((a.id.clone(), signal.as_str()));

            // Загружаем per-agent state.
            let (pinged_stuck, retried_error, last_signal, escalated_for_signal) = {
                let g = self.state.read();
                let s = g.get(&a.id);
                (
                    s.map(|s| s.pinged_stuck).unwrap_or(false),
                    s.map(|s| s.retried_error).unwrap_or(false),
                    s.and_then(|s| s.last_signal),
                    s.and_then(|s| s.escalated_for_signal),
                )
            };

            // Сбросить флаги, если агент явно вернулся в работу.
            if signal == AgentSignal::Working {
                let mut g = self.state.write();
                let s = g.entry(a.id.clone()).or_default();
                s.pinged_stuck = false;
                s.retried_error = false;
                s.last_signal = Some(signal);
                s.escalated_for_signal = None;
                continue;
            }

            // Проверка очереди — нужна для idle_done.
            let has_pending = if cfg.auto_assign_next && signal == AgentSignal::IdleDone {
                self.task_mgr
                    .list(Some(&ws_id), Some("todo"), None)
                    .map(|v| !v.is_empty())
                    .unwrap_or(false)
            } else {
                false
            };

            let decision = decide(signal, &tail, has_pending, pinged_stuck, retried_error);

            // Дедупликация: если решение Observe и сигнал не сменился —
            // ничего не пишем в лог (иначе UI зальёт спамом).
            let signal_changed = last_signal != Some(signal);
            let is_observe = matches!(decision, PolicyDecision::Observe { .. });
            if is_observe && !signal_changed {
                self.update_last_signal(&a.id, signal);
                continue;
            }
            // Не дублировать эскалацию для одного и того же signal.
            if let PolicyDecision::Escalate { .. } = &decision {
                if escalated_for_signal == Some(signal) {
                    self.update_last_signal(&a.id, signal);
                    continue;
                }
            }

            let executed = self.execute(&a.id, &a.workspace_id, &decision, cfg, &tail);

            // Запись в лог + событие.
            let rec = DecisionRecord {
                at: Utc::now().to_rfc3339(),
                agent_id: a.id.clone(),
                workspace_id: a.workspace_id.clone(),
                signal: signal.as_str(),
                kind: decision.kind(),
                reason: match &decision {
                    PolicyDecision::Observe { reason }
                    | PolicyDecision::AutoConfirm { reason }
                    | PolicyDecision::AutoChoose { reason, .. }
                    | PolicyDecision::AssignNext { reason }
                    | PolicyDecision::PingStuck { reason }
                    | PolicyDecision::AutoRetryError { reason } => reason.clone(),
                    PolicyDecision::Escalate { reason, .. } => reason.clone(),
                },
                quote: match &decision {
                    PolicyDecision::Escalate { quote, .. } => Some(quote.clone()),
                    _ => Some(extract_quote(&tail)).filter(|q| !q.is_empty()),
                },
                auto_executed: executed,
            };
            self.log.push(rec.clone());
            if let Some(app) = self.app.read().as_ref() {
                let _ = app.emit(EV_ARCHITECT_DECISION, &rec);
            }

            // Обновить per-agent state в зависимости от того, что сделали.
            self.post_decision_state(&a.id, signal, &decision);
        }

        if let Some(app) = self.app.read().as_ref() {
            let _ = app.emit(
                EV_ARCHITECT_SIGNAL,
                serde_json::json!({
                    "workspace_id": ws_id,
                    "signals": signals.into_iter()
                        .map(|(id, s)| serde_json::json!({"agent_id": id, "signal": s}))
                        .collect::<Vec<_>>(),
                }),
            );
        }
        Ok(())
    }

    fn update_last_signal(&self, agent_id: &str, signal: AgentSignal) {
        let mut g = self.state.write();
        let s = g.entry(agent_id.to_string()).or_default();
        s.last_signal = Some(signal);
    }

    fn post_decision_state(&self, agent_id: &str, signal: AgentSignal, decision: &PolicyDecision) {
        let mut g = self.state.write();
        let s = g.entry(agent_id.to_string()).or_default();
        s.last_signal = Some(signal);
        match decision {
            PolicyDecision::PingStuck { .. } => {
                s.pinged_stuck = true;
            }
            PolicyDecision::AutoRetryError { .. } => {
                s.retried_error = true;
            }
            PolicyDecision::Escalate { .. } => {
                s.escalated_for_signal = Some(signal);
            }
            _ => {}
        }
    }

    /// Реально выполнить действие. Возвращает `true` если что-то отправили
    /// агенту (или назначили задачу). Escalate / Observe не считаются auto-
    /// executed, но всё равно попадают в лог.
    fn execute(
        &self,
        agent_id: &str,
        workspace_id: &str,
        decision: &PolicyDecision,
        cfg: &ArchitectConfig,
        _tail: &str,
    ) -> bool {
        match decision {
            PolicyDecision::Observe { .. } => false,
            PolicyDecision::AutoConfirm { .. } => {
                if !cfg.auto_confirm {
                    return false;
                }
                // `y` + Enter — Enter мы добавляем как `\r` (PTY-ы любят CR).
                if let Err(e) = self.agent_mgr.write(agent_id, b"y\r") {
                    tracing::warn!("architect AutoConfirm write: {}", e);
                    return false;
                }
                true
            }
            PolicyDecision::AutoChoose { index, .. } => {
                let payload = format!("{}\r", index);
                if let Err(e) = self.agent_mgr.write(agent_id, payload.as_bytes()) {
                    tracing::warn!("architect AutoChoose write: {}", e);
                    return false;
                }
                true
            }
            PolicyDecision::PingStuck { .. } => {
                // No PTY injection — just record the decision so the next
                // Stuck signal escalates instead of pinging again.
                false
            }
            PolicyDecision::AutoRetryError { .. } => {
                let msg =
                    b"\rThe last command failed. Inspect the error above and propose a fix.\r";
                if let Err(e) = self.agent_mgr.write(agent_id, msg) {
                    tracing::warn!("architect AutoRetryError write: {}", e);
                    return false;
                }
                true
            }
            PolicyDecision::AssignNext { .. } => {
                if !cfg.auto_assign_next {
                    return false;
                }
                self.assign_next_todo(agent_id, workspace_id)
            }
            PolicyDecision::Escalate { .. } => false,
        }
    }

    fn assign_next_todo(&self, agent_id: &str, workspace_id: &str) -> bool {
        let todos = match self.task_mgr.list(Some(workspace_id), Some("todo"), None) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("architect list todos: {}", e);
                return false;
            }
        };
        let Some(next) = todos.into_iter().next() else {
            return false;
        };
        if let Err(e) = self.task_mgr.assign(&next.id, Some(agent_id)) {
            tracing::warn!("architect assign: {}", e);
            return false;
        }
        // Move out of `todo` so the next tick doesn't re-pick the same row
        // and re-spam the agent before it has produced any output.
        if let Err(e) = self.task_mgr.update(crate::tasks::UpdateTaskArgs {
            id: next.id.clone(),
            title: None,
            instructions: None,
            knowledge: None,
            status: Some("in_progress".into()),
            agent_id: None,
        }) {
            tracing::warn!("architect mark in_progress: {}", e);
        }
        let prompt = format!(
            "\rArchitect: take task `{}`. Brief:\n{}\n\nWhen done, print `handoff_ready: <summary>`.\r",
            next.title.replace('`', "'"),
            next.instructions.replace('`', "'"),
        );
        if let Err(e) = self.agent_mgr.write(agent_id, prompt.as_bytes()) {
            tracing::warn!("architect AssignNext write: {}", e);
            return false;
        }
        true
    }
}
