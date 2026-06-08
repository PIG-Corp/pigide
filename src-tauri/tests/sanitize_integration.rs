use pigide_lib::sanitize::{sanitize, strip_ansi};

const REAL_ANSI_BLOB: &str =
    "\x1b[2J\x1b[H\x1b[38;2;255;100;0m\x1b[1m❯\x1b[0m \x1b[38;2;100;200;50mcargo build\x1b[0m\n\
    \x1b[K   Compiling pigide v0.1.0\n\
    \x1b[K   \x1b[32mFinished\x1b[0m dev [unoptimized] target(s) in 24.58s\n\
    \x1b]0;user@host: ~/pigide\x07\
    \x1b[38;2;255;100;0m\x1b[1m❯\x1b[0m \x1b[?25h\
    Задача выполнена ✅\n\
    \x1b[?25l\x1b[1;1H";

#[test]
fn real_blob_contains_no_escape_bytes() {
    let result = sanitize(REAL_ANSI_BLOB);
    assert!(
        !result.contains('\x1b'),
        "sanitized output still contains \\x1b: {:?}",
        result
    );
}

#[test]
fn real_blob_preserves_visible_text() {
    let result = sanitize(REAL_ANSI_BLOB);
    assert!(result.contains("cargo build"));
    assert!(result.contains("Compiling pigide v0.1.0"));
    assert!(result.contains("Finished"));
    assert!(result.contains("Задача выполнена ✅"));
}

#[test]
fn real_blob_no_control_chars_except_newline_tab() {
    let result = sanitize(REAL_ANSI_BLOB);
    for (i, c) in result.chars().enumerate() {
        if c == '\n' || c == '\t' {
            continue;
        }
        assert!(
            c >= ' ' && c != '\x7F' && !('\u{0080}'..='\u{009F}').contains(&c),
            "unexpected control char {:?} (U+{:04X}) at position {}",
            c,
            c as u32,
            i
        );
    }
}

#[test]
fn strip_ansi_is_subset_of_sanitize() {
    let stripped = strip_ansi(REAL_ANSI_BLOB);
    let sanitized = sanitize(REAL_ANSI_BLOB);
    assert!(
        stripped.contains(&sanitized) || sanitized.len() <= stripped.len(),
        "sanitize should remove at least as much as strip_ansi"
    );
}

#[test]
fn sanitize_idempotent_on_real_blob() {
    let once = sanitize(REAL_ANSI_BLOB);
    let twice = sanitize(&once);
    assert_eq!(once, twice);
}
