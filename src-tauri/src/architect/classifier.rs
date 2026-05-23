//! Classifier: tail (последние ~4-8KB stdout агента) + время простоя -> состояние.
//!
//! Все паттерны pluggable через `regex::RegexSet`. Если захочется расширить —
//! правь массивы PATTERNS и пересобирай. ANSI-escape вычищаются перед матчем,
//! чтобы цветные выводы CLI-агентов не ломали детекцию.

use once_cell::sync::Lazy;
use regex::RegexSet;
use std::time::Duration;

use crate::sanitize::strip_ansi;

/// Чёткое классифицированное состояние агента.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSignal {
    /// Агент активно печатает — не трогаем.
    Working,
    /// Агент закончил задачу (handoff_ready / Done. / ✓ complete).
    IdleDone,
    /// Агент задал вопрос и ждёт ответа (y/N, выбор, "Press Enter").
    AwaitingInput,
    /// В свежем выводе видны паттерны ошибок.
    Error,
    /// Долго молчит без маркеров завершения и без вопроса.
    Stuck,
    /// Слишком мало сигналов чтобы делать выводы — наблюдаем дальше.
    Unknown,
}

impl AgentSignal {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentSignal::Working => "working",
            AgentSignal::IdleDone => "idle_done",
            AgentSignal::AwaitingInput => "awaiting_input",
            AgentSignal::Error => "error",
            AgentSignal::Stuck => "stuck",
            AgentSignal::Unknown => "unknown",
        }
    }
}

// --- Паттерны ---

/// Маркеры завершения задачи. Регистронезависимо.
const DONE_PATTERNS: &[&str] = &[
    r"(?i)handoff_ready\s*:",
    r"(?i)\btask\s+complete\b",
    r"(?i)✓\s*complete",
    r"(?i)\bdone\.\s*$",
    r"(?i)^\s*done\.?\s*$",
    r"(?i)\ball\s+tests?\s+passed\b",
    r"(?i)\bsuccessfully\s+completed\b",
    r"(?i)\bfinished\.?\s*$",
];

/// Маркеры "агент задал вопрос". Покрывают и интерактивные prompts.
const ASK_PATTERNS: &[&str] = &[
    r"\(\s*[yY]\s*/\s*[nN]\s*\)\s*\??",
    r"\[\s*[yY]\s*/\s*[nN]\s*\]\s*\??",
    r"(?i)\bcontinue\?\s*$",
    r"(?i)\bproceed\?\s*$",
    r"(?i)\bpress\s+enter\b",
    r"(?i)\bdo\s+you\s+want\s+(me\s+)?to\b",
    r"(?i)\bshould\s+i\s+(proceed|continue|run|delete|drop|push|deploy)\b",
    r"(?i)\bwould\s+you\s+like\s+(me\s+)?to\b",
    r"(?i)\bокей\?\s*$",
    r"(?i)\bпродолжить\?",
    r"(?i)\bвыбери\s+вариант",
    r"(?i)\bвы\s+уверены\?",
    // Числовой/буквенный выбор: [1] foo  [2] bar
    r"\[\s*[1-9]\s*\][^\n]+\[\s*[1-9]\s*\]",
    r"^\s*[1-9]\)\s+\S",
];

/// Маркеры ошибок.
const ERROR_PATTERNS: &[&str] = &[
    r"(?i)\bpanic(?:ked)?:\s",
    r"(?i)^\s*error:\s",
    r"(?i)^\s*fatal:\s",
    r"(?i)\btraceback\b",
    r"(?i)\bexception\b.*:\s",
    r"(?i)\bpermission\s+denied\b",
    r"(?i)\bcommand\s+not\s+found\b",
    r"(?i)\bno\s+such\s+file\s+or\s+directory\b",
    r"(?i)\bsegmentation\s+fault\b",
    r"(?i)\bfailed\s+with\s+exit\s+code\s+[1-9]",
    r"(?i)\bcompilation\s+failed\b",
    r"(?i)\bbuild\s+failed\b",
];

/// Destructive markers — даже если вопрос выглядит как "continue?", при
/// наличии любого из них policy эскалирует, а не нажимает `y`.
pub const DESTRUCTIVE_PATTERNS: &[&str] = &[
    r"(?i)\brm\s+-rf\b",
    r"(?i)\bgit\s+push\s+(-f|--force)\b",
    r"(?i)\bforce[- ]push\b",
    r"(?i)\bdrop\s+(table|database|schema)\b",
    r"(?i)\bdelete\s+(all|every|the\s+entire)\b",
    r"(?i)\bdeploy(?:ing)?\s+to\s+(prod|production)\b",
    r"(?i)\boverwrite\s+main\b",
    r"(?i)\brewrite\s+main\b",
    r"(?i)\bdrop\s+all\b",
    r"(?i)\btruncate\s+(table|all)\b",
    r"(?i)\bproduction\b",
    r"(?i)\bпрод(?:акшен|акшн)\b",
    r"(?i)\bудалить\s+(всё|все|базу|таблиц)",
];

static DONE_RE: Lazy<RegexSet> = Lazy::new(|| RegexSet::new(DONE_PATTERNS).unwrap());
static ASK_RE: Lazy<RegexSet> = Lazy::new(|| RegexSet::new(ASK_PATTERNS).unwrap());
static ERROR_RE: Lazy<RegexSet> = Lazy::new(|| RegexSet::new(ERROR_PATTERNS).unwrap());

/// Решить, что происходит с агентом.
///
/// `tail` — UTF-8 хвост лога (например, последние 4-8 KB). Не паникует на
/// невалидных байтах: вызывающая сторона уже привела к строке.
/// `idle_for` — сколько секунд в stdout была тишина (None если агент впервые
/// замечен и last_stdout не выставлен).
pub fn classify(
    tail: &str,
    idle_for: Option<Duration>,
    idle_done_after: Duration,
    stuck_after: Duration,
) -> AgentSignal {
    let cleaned = strip_ansi(tail);
    // Берём последние ~2 KB — больше нам нечего classify-ить, маркеры обычно
    // в самом конце вывода.
    let window: &str = if cleaned.len() > 2048 {
        let start = cleaned.len() - 2048;
        let start = cleaned.ceil_char_boundary(start);
        &cleaned[start..]
    } else {
        &cleaned
    };

    let idle_secs = idle_for.map(|d| d.as_secs_f32()).unwrap_or(0.0);

    // Свежий вывод = ещё работает. Кроме случая, когда в самом хвосте вопрос —
    // агент мог только что напечатать prompt и ждёт ответа.
    let is_quiet = idle_for.map(|d| d >= idle_done_after).unwrap_or(false);

    let asks = ASK_RE.is_match(window);
    let errors = ERROR_RE.is_match(window);
    let done = DONE_RE.is_match(window);

    // Вопрос всегда главнее — даже если поверх есть свежий вывод, мы должны
    // успеть отреагировать (иначе агент зависнет на prompt).
    if asks {
        return AgentSignal::AwaitingInput;
    }

    if errors {
        return AgentSignal::Error;
    }

    if !is_quiet && idle_secs < idle_done_after.as_secs_f32() {
        // Совсем недавно что-то печатал и не задал вопрос — ещё работает.
        return AgentSignal::Working;
    }

    if done {
        return AgentSignal::IdleDone;
    }

    if idle_for.map(|d| d >= stuck_after).unwrap_or(false) {
        return AgentSignal::Stuck;
    }

    if is_quiet {
        // Тихо, но без явных маркеров — не дёргаем.
        return AgentSignal::IdleDone;
    }

    AgentSignal::Unknown
}

/// True если в тексте есть destructive marker — используется policy.
pub fn looks_destructive(text: &str) -> bool {
    static DESTRUCTIVE_RE: Lazy<RegexSet> =
        Lazy::new(|| RegexSet::new(DESTRUCTIVE_PATTERNS).unwrap());
    DESTRUCTIVE_RE.is_match(&strip_ansi(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn s(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    #[test]
    fn working_when_idle_short() {
        let out = "compiling crate foo\nlinking...\n";
        let sig = classify(out, Some(s(0)), s(8), s(45));
        assert_eq!(sig, AgentSignal::Working);
    }

    #[test]
    fn done_marker_idle_done() {
        let out = "ok, applied patch.\nhandoff_ready: feature wired\n";
        let sig = classify(out, Some(s(10)), s(8), s(45));
        assert_eq!(sig, AgentSignal::IdleDone);
    }

    #[test]
    fn done_lower_case_done_dot() {
        let out = "patched 3 files.\nDone.\n";
        let sig = classify(out, Some(s(12)), s(8), s(45));
        assert_eq!(sig, AgentSignal::IdleDone);
    }

    #[test]
    fn ask_yn_immediate() {
        let out = "About to install 12 packages. Continue? (y/N) ";
        let sig = classify(out, Some(s(0)), s(8), s(45));
        assert_eq!(sig, AgentSignal::AwaitingInput);
    }

    #[test]
    fn ask_numeric_choice() {
        let out = "Pick approach:\n[1] rebase  [2] merge  [3] cherry-pick\n> ";
        let sig = classify(out, Some(s(2)), s(8), s(45));
        assert_eq!(sig, AgentSignal::AwaitingInput);
    }

    #[test]
    fn error_traceback() {
        let out = "Traceback (most recent call last):\n  File ...\nValueError: bad\n";
        let sig = classify(out, Some(s(3)), s(8), s(45));
        assert_eq!(sig, AgentSignal::Error);
    }

    #[test]
    fn error_command_not_found() {
        let out = "$ kiro-cli\nbash: kiro-cli: command not found\n";
        let sig = classify(out, Some(s(2)), s(8), s(45));
        assert_eq!(sig, AgentSignal::Error);
    }

    #[test]
    fn stuck_after_long_silence() {
        let out = "thinking...";
        let sig = classify(out, Some(s(120)), s(8), s(45));
        assert_eq!(sig, AgentSignal::Stuck);
    }

    #[test]
    fn idle_done_quiet_without_markers() {
        // Тихо >idle_done, нет вопроса, нет ошибок — считаем idle_done,
        // оркестратор спросит next-task.
        let out = "wrote src/foo.rs\n";
        let sig = classify(out, Some(s(15)), s(8), s(45));
        assert_eq!(sig, AgentSignal::IdleDone);
    }

    #[test]
    fn ansi_is_stripped() {
        let out = "\x1b[31merror:\x1b[0m something blew up\n";
        let sig = classify(out, Some(s(2)), s(8), s(45));
        assert_eq!(sig, AgentSignal::Error);
    }

    #[test]
    fn destructive_detector() {
        assert!(looks_destructive("Should I rm -rf node_modules?"));
        assert!(looks_destructive("git push -f origin main, ok?"));
        assert!(looks_destructive("DROP TABLE users; continue? (y/N)"));
        assert!(looks_destructive("Удалить базу данных prod?"));
        assert!(!looks_destructive("Format the file?"));
        assert!(!looks_destructive("Continue with the test run?"));
    }
}
