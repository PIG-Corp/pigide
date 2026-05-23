//! Policy: на основе классификации и текста вопроса решить, что делать.
//!
//! Главный инвариант: destructive-вопросы НИКОГДА не получают авто-`y`.
//! Свободно-формовые вопросы ("describe the bug", "what name?") тоже
//! эскалируются — мы не пытаемся их додумать.

use super::classifier::{looks_destructive, AgentSignal};
use crate::sanitize::strip_ansi;
use once_cell::sync::Lazy;
use regex::Regex;

/// Что супервизор собирается сделать с агентом.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PolicyDecision {
    /// Ничего не делаем (агент ещё работает или ситуация неоднозначная).
    Observe { reason: String },
    /// Безопасно нажать `y` (или Enter) — confirm prompt.
    AutoConfirm { reason: String },
    /// Выбрать default из numeric choice (например первый отмеченный
    /// recommended/default; если такого нет — escalate).
    AutoChoose { index: u8, reason: String },
    /// Назначить следующую todo-таску этому агенту (idle_done без вопроса).
    AssignNext { reason: String },
    /// Один раз пингануть "status?" чтобы вывести из ступора.
    PingStuck { reason: String },
    /// Попросить агента самому посмотреть на ошибку и исправить.
    AutoRetryError { reason: String },
    /// Вынести в чат-панель — нужен человек.
    Escalate { reason: String, quote: String },
}

impl PolicyDecision {
    pub fn kind(&self) -> &'static str {
        match self {
            PolicyDecision::Observe { .. } => "observe",
            PolicyDecision::AutoConfirm { .. } => "auto_confirm",
            PolicyDecision::AutoChoose { .. } => "auto_choose",
            PolicyDecision::AssignNext { .. } => "assign_next",
            PolicyDecision::PingStuck { .. } => "ping_stuck",
            PolicyDecision::AutoRetryError { .. } => "auto_retry_error",
            PolicyDecision::Escalate { .. } => "escalate",
        }
    }
}

/// Что-то, что точно не "y/N" — свободный текстовый вопрос.
/// Если вопрос матчит — эскалируем.
static FREE_FORM_PATTERNS: &[&str] = &[
    r"(?i)\bwhat\s+(name|file|message|reason|approach)\b",
    r"(?i)\bdescribe\s+the\b",
    r"(?i)\bplease\s+(provide|paste|enter)\b",
    r"(?i)\benter\s+a\s+(commit|branch|name|message)\b",
    r"(?i)\bкак\s+(назвать|оформить)\b",
];

static FREE_FORM_RE: Lazy<regex::RegexSet> =
    Lazy::new(|| regex::RegexSet::new(FREE_FORM_PATTERNS).unwrap());

/// Confirm-prompts, на которые безопасно нажимать `y`. Этим matchером мы
/// разрешаем auto-`y` только если вопрос узнаваемо "yes/no continue".
static SAFE_CONFIRM_PATTERNS: &[&str] = &[
    r"\(\s*[yY]\s*/\s*[nN]\s*\)\s*\??\s*$",
    r"(?i)\bcontinue\?\s*$",
    r"(?i)\bproceed\?\s*$",
    r"(?i)\bpress\s+enter\b",
    r"(?i)\bокей\?\s*$",
    r"(?i)\bпродолжить\?\s*$",
];

static SAFE_CONFIRM_RE: Lazy<regex::RegexSet> =
    Lazy::new(|| regex::RegexSet::new(SAFE_CONFIRM_PATTERNS).unwrap());

/// Numeric choice — `[1] foo  [2] bar`. Возвращает (index, ok-to-auto).
/// Auto только если ровно один вариант помечен как `default`/`recommended`.
fn parse_numeric_choice(text: &str) -> Option<(u8, bool)> {
    static OPT_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"\[\s*([1-9])\s*\]\s*([^\[\n]+)").unwrap());
    static DEFAULT_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)\b(default|recommended|рекоменд|по умолчанию)\b").unwrap());
    let mut count = 0u32;
    let mut default_idx: Option<u8> = None;
    let mut multiple_defaults = false;
    for cap in OPT_RE.captures_iter(text) {
        count += 1;
        let idx: u8 = cap.get(1)?.as_str().parse().ok()?;
        let body = cap.get(2)?.as_str();
        if DEFAULT_RE.is_match(body) {
            if default_idx.is_some() {
                multiple_defaults = true;
            } else {
                default_idx = Some(idx);
            }
        }
    }
    if !(2..=5).contains(&count) {
        return None;
    }
    Some((
        default_idx.unwrap_or(1),
        default_idx.is_some() && !multiple_defaults,
    ))
}

/// Из последних строк tail вытащить «вопрос» (для quote в decision-log /
/// эскалации). Возвращаем максимум ~240 символов, схлопываем многократные
/// пустые строки.
pub fn extract_quote(tail: &str) -> String {
    let cleaned = strip_ansi(tail);
    let lines: Vec<&str> = cleaned.lines().filter(|l| !l.trim().is_empty()).collect();
    let take = lines.len().saturating_sub(6);
    let mut q = lines[take..].join("\n");
    if q.len() > 240 {
        let cut = q.ceil_char_boundary(q.len() - 240);
        q = format!("…{}", &q[cut..]);
    }
    q
}

/// Принять решение. `tail` — буфер, по которому уже классифицирован сигнал.
pub fn decide(
    signal: AgentSignal,
    tail: &str,
    has_pending_task: bool,
    already_pinged_stuck: bool,
    already_retried_error: bool,
) -> PolicyDecision {
    let cleaned = strip_ansi(tail);

    match signal {
        AgentSignal::Working => PolicyDecision::Observe {
            reason: "agent is producing output".into(),
        },

        AgentSignal::AwaitingInput => {
            // 1. Destructive => escalate.
            if looks_destructive(&cleaned) {
                return PolicyDecision::Escalate {
                    reason: "destructive prompt — auto-confirm disabled".into(),
                    quote: extract_quote(&cleaned),
                };
            }
            // 2. Free-form => escalate.
            if FREE_FORM_RE.is_match(&cleaned) {
                return PolicyDecision::Escalate {
                    reason: "free-form question requires human input".into(),
                    quote: extract_quote(&cleaned),
                };
            }
            // 3. Numeric choice — берём default если он один и явный.
            if let Some((idx, ok_to_auto)) = parse_numeric_choice(&cleaned) {
                if ok_to_auto {
                    return PolicyDecision::AutoChoose {
                        index: idx,
                        reason: format!("default option [{}] selected", idx),
                    };
                }
                return PolicyDecision::Escalate {
                    reason: "numeric choice without obvious default".into(),
                    quote: extract_quote(&cleaned),
                };
            }
            // 4. Safe confirm — нажимаем `y`.
            if SAFE_CONFIRM_RE.is_match(&cleaned) {
                return PolicyDecision::AutoConfirm {
                    reason: "non-destructive yes/no continue prompt".into(),
                };
            }
            // 5. Иначе — эскалация.
            PolicyDecision::Escalate {
                reason: "unrecognized prompt — escalating".into(),
                quote: extract_quote(&cleaned),
            }
        }

        AgentSignal::Error => {
            if already_retried_error {
                PolicyDecision::Escalate {
                    reason: "error persists after one auto-retry".into(),
                    quote: extract_quote(&cleaned),
                }
            } else {
                PolicyDecision::AutoRetryError {
                    reason: "first error — asking agent to inspect and fix".into(),
                }
            }
        }

        AgentSignal::IdleDone => {
            if has_pending_task {
                PolicyDecision::AssignNext {
                    reason: "agent finished current work, queue has todo".into(),
                }
            } else {
                PolicyDecision::Observe {
                    reason: "idle done; queue empty".into(),
                }
            }
        }

        AgentSignal::Stuck => {
            if already_pinged_stuck {
                PolicyDecision::Escalate {
                    reason: "still silent after ping".into(),
                    quote: extract_quote(&cleaned),
                }
            } else {
                PolicyDecision::PingStuck {
                    reason: "long silence; nudging once".into(),
                }
            }
        }

        AgentSignal::Unknown => PolicyDecision::Observe {
            reason: "not enough signal yet".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destructive_yn_escalates() {
        let tail = "About to drop table users.\nProceed? (y/N) ";
        let d = decide(AgentSignal::AwaitingInput, tail, false, false, false);
        assert!(matches!(d, PolicyDecision::Escalate { .. }), "{:?}", d);
    }

    #[test]
    fn rm_rf_escalates() {
        let tail = "rm -rf /home/user/project? (y/N) ";
        assert!(matches!(
            decide(AgentSignal::AwaitingInput, tail, false, false, false),
            PolicyDecision::Escalate { .. }
        ));
    }

    #[test]
    fn safe_continue_auto_confirms() {
        let tail = "Installing 4 packages. Continue? (y/N) ";
        let d = decide(AgentSignal::AwaitingInput, tail, false, false, false);
        assert!(matches!(d, PolicyDecision::AutoConfirm { .. }), "{:?}", d);
    }

    #[test]
    fn russian_continue_auto_confirms() {
        let tail = "Сейчас обновлю файлы. Продолжить?";
        let d = decide(AgentSignal::AwaitingInput, tail, false, false, false);
        assert!(matches!(d, PolicyDecision::AutoConfirm { .. }), "{:?}", d);
    }

    #[test]
    fn free_form_escalates() {
        let tail = "Please provide a commit message.";
        let d = decide(AgentSignal::AwaitingInput, tail, false, false, false);
        assert!(matches!(d, PolicyDecision::Escalate { .. }), "{:?}", d);
    }

    #[test]
    fn numeric_with_default_auto_chooses() {
        let tail = "Pick build:\n[1] release (default)\n[2] debug\n> ";
        let d = decide(AgentSignal::AwaitingInput, tail, false, false, false);
        assert!(
            matches!(d, PolicyDecision::AutoChoose { index: 1, .. }),
            "{:?}",
            d
        );
    }

    #[test]
    fn numeric_no_default_escalates() {
        let tail = "Pick build:\n[1] release\n[2] debug\n[3] dev\n> ";
        let d = decide(AgentSignal::AwaitingInput, tail, false, false, false);
        assert!(matches!(d, PolicyDecision::Escalate { .. }), "{:?}", d);
    }

    #[test]
    fn idle_done_with_queue_assigns_next() {
        let tail = "handoff_ready: feature wired";
        let d = decide(AgentSignal::IdleDone, tail, true, false, false);
        assert!(matches!(d, PolicyDecision::AssignNext { .. }), "{:?}", d);
    }

    #[test]
    fn idle_done_empty_queue_observes() {
        let tail = "Done.";
        let d = decide(AgentSignal::IdleDone, tail, false, false, false);
        assert!(matches!(d, PolicyDecision::Observe { .. }), "{:?}", d);
    }

    #[test]
    fn first_error_retries() {
        let tail = "error: cannot find module foo";
        let d = decide(AgentSignal::Error, tail, false, false, false);
        assert!(
            matches!(d, PolicyDecision::AutoRetryError { .. }),
            "{:?}",
            d
        );
    }

    #[test]
    fn second_error_escalates() {
        let tail = "error: cannot find module foo";
        let d = decide(AgentSignal::Error, tail, false, false, true);
        assert!(matches!(d, PolicyDecision::Escalate { .. }), "{:?}", d);
    }

    #[test]
    fn stuck_first_pings_then_escalates() {
        let d1 = decide(AgentSignal::Stuck, "thinking...", false, false, false);
        assert!(matches!(d1, PolicyDecision::PingStuck { .. }));
        let d2 = decide(AgentSignal::Stuck, "thinking...", false, true, false);
        assert!(matches!(d2, PolicyDecision::Escalate { .. }));
    }

    #[test]
    fn working_observes() {
        let d = decide(AgentSignal::Working, "compiling...", true, false, false);
        assert!(matches!(d, PolicyDecision::Observe { .. }));
    }
}
