use once_cell::sync::Lazy;
use regex::Regex;

static ANSI_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\x1B(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*\x07|[@-Z\\-_])").unwrap());

pub fn strip_ansi(s: &str) -> String {
    ANSI_RE.replace_all(s, "").into_owned()
}

pub fn sanitize(s: &str) -> String {
    let stripped = strip_ansi(s);
    let clean: String = stripped
        .chars()
        .filter(|&c| {
            c == '\n'
                || c == '\t'
                || (c >= ' ' && c != '\x7F' && !('\u{0080}'..='\u{009F}').contains(&c))
        })
        .collect();

    if clean.len() != s.len() {
        tracing::debug!(
            "sanitize: removed {} bytes ({} → {})",
            s.len() - clean.len(),
            s.len(),
            clean.len()
        );
    }

    clean
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_csi_sequences() {
        let input = "\x1b[31mERROR\x1b[0m: something failed";
        assert_eq!(sanitize(input), "ERROR: something failed");
    }

    #[test]
    fn strip_osc_title() {
        let input = "\x1b]0;my-terminal-title\x07visible text";
        assert_eq!(sanitize(input), "visible text");
    }

    #[test]
    fn strip_sgr_256_color() {
        let input = "\x1b[38;2;255;100;0mcolored\x1b[0m plain";
        assert_eq!(sanitize(input), "colored plain");
    }

    #[test]
    fn strip_cursor_and_erase() {
        let input = "\x1b[2J\x1b[H\x1b[Khello";
        assert_eq!(sanitize(input), "hello");
    }

    #[test]
    fn preserve_utf8_cyrillic_and_emoji() {
        let input = "\x1b[1mПривет\x1b[0m мир 🚀✨";
        assert_eq!(sanitize(input), "Привет мир 🚀✨");
    }

    #[test]
    fn mixed_ansi_utf8_emoji() {
        let input = "\x1b[38;2;100;200;50m🎉 Готово!\x1b[0m\n\x1b]0;done\x07Следующий шаг";
        assert_eq!(sanitize(input), "🎉 Готово!\nСледующий шаг");
    }

    #[test]
    fn preserves_newlines_and_tabs() {
        let input = "line1\n\tindented\nline3";
        assert_eq!(sanitize(input), "line1\n\tindented\nline3");
    }

    #[test]
    fn strips_control_chars() {
        let input = "hello\x01\x02\x03world\x7F";
        assert_eq!(sanitize(input), "helloworld");
    }

    #[test]
    fn strips_c1_control_chars() {
        let input = "before\u{0080}\u{008F}\u{009F}after";
        assert_eq!(sanitize(input), "beforeafter");
    }

    #[test]
    fn idempotent() {
        let input = "\x1b[31m\x1b]0;title\x07hello\x01world\x1b[0m\n";
        let once = sanitize(input);
        let twice = sanitize(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn empty_input() {
        assert_eq!(sanitize(""), "");
    }

    #[test]
    fn all_control_input() {
        let input = "\x1b[31m\x01\x02\x03\x1b[0m";
        assert_eq!(sanitize(input), "");
    }

    #[test]
    fn strip_ansi_only() {
        let input = "\x1b[31mred\x1b[0m";
        assert_eq!(strip_ansi(input), "red");
    }
}
