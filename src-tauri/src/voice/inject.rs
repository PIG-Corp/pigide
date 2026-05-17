//! Cross-platform text injection — типит транскрипт в фокусное окно.
//!
//! Strategy:
//! - **X11 / Windows**: enigo.text() — синтез Unicode keystrokes.
//! - **Wayland / fallback**: clipboard + Ctrl+V (через arboard + enigo).
//! - **macOS**: clipboard + Cmd+V (требует Accessibility permission).

use crate::db::{DbPool, get_setting, set_setting};
use crate::error::{Error, Result};

const SETTING_KEY: &str = "voice.inject_enabled";

/// Whether transcript auto-injection is enabled. Defaults to `false`.
pub fn inject_enabled(db: &DbPool) -> bool {
    match get_setting(db, SETTING_KEY) {
        Ok(Some(v)) => v == "true",
        _ => false,
    }
}

/// Persist the auto-injection toggle.
pub fn set_enabled(db: &DbPool, on: bool) -> Result<()> {
    set_setting(db, SETTING_KEY, if on { "true" } else { "false" })
}

/// Type `text` into the currently focused window.
///
/// Empty inputs and Whisper error sentinels (`[error: ...]`) are silently
/// skipped. Caller is responsible for guarding on `inject_enabled` and
/// window-focus state — `mod.rs` does this before invoking us.
pub fn type_text(text: &str) -> Result<()> {
    if is_skippable(text) {
        return Ok(());
    }

    if prefer_direct_type() {
        match try_direct_type(text) {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::warn!("enigo.text() failed, falling back to clipboard: {}", e);
            }
        }
    }

    paste_via_clipboard(text)
}

fn is_skippable(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return true;
    }
    t.starts_with("[error:")
}

/// X11 / Windows: пробуем синтез Unicode keystrokes напрямую. На Wayland
/// и macOS — сразу clipboard+paste, без бесполезной попытки.
fn prefer_direct_type() -> bool {
    if cfg!(target_os = "windows") {
        return true;
    }
    if cfg!(target_os = "linux") {
        return !is_wayland();
    }
    false
}

fn is_wayland() -> bool {
    std::env::var("WAYLAND_DISPLAY")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

fn try_direct_type(text: &str) -> Result<()> {
    use enigo::{Enigo, Keyboard, Settings};

    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|e| Error::Voice(format!("enigo init: {}", e)))?;
    enigo
        .text(text)
        .map_err(|e| Error::Voice(format!("enigo.text: {}", e)))?;
    Ok(())
}

fn paste_via_clipboard(text: &str) -> Result<()> {
    use arboard::Clipboard;

    let mut cb =
        Clipboard::new().map_err(|e| Error::Voice(format!("clipboard init: {}", e)))?;

    // Сохранить старое содержимое (если оно было текстом).
    let previous = cb.get_text().ok();

    cb.set_text(text.to_string())
        .map_err(|e| Error::Voice(format!("clipboard set: {}", e)))?;

    if let Err(e) = press_paste() {
        // Clipboard всё равно заполнен — пользователь может вставить вручную.
        tracing::warn!("paste hotkey failed (clipboard contains text): {}", e);
    }

    // Через 200мс восстановить прошлое содержимое в фоновом потоке,
    // не блокируя caller. Caller'у важно вернуться к event loop'у.
    if let Some(prev) = previous {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            match Clipboard::new() {
                Ok(mut cb) => {
                    if let Err(e) = cb.set_text(prev) {
                        tracing::warn!("clipboard restore failed: {}", e);
                    }
                }
                Err(e) => {
                    tracing::warn!("clipboard restore (init) failed: {}", e);
                }
            }
        });
    }

    Ok(())
}

fn press_paste() -> Result<()> {
    use enigo::{
        Direction::{Click, Press, Release},
        Enigo, Key, Keyboard, Settings,
    };

    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|e| Error::Voice(format!("enigo init: {}", e)))?;

    let modifier = if cfg!(target_os = "macos") {
        Key::Meta
    } else {
        Key::Control
    };

    enigo
        .key(modifier, Press)
        .map_err(|e| Error::Voice(format!("enigo press mod: {}", e)))?;

    // Click v независимо от исхода release — модификатор обязан быть отпущен.
    let click_res = enigo.key(Key::Unicode('v'), Click);
    let release_res = enigo.key(modifier, Release);

    click_res.map_err(|e| Error::Voice(format!("enigo click v: {}", e)))?;
    release_res.map_err(|e| Error::Voice(format!("enigo release mod: {}", e)))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> DbPool {
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(2).build(manager).unwrap();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL);",
        )
        .unwrap();
        pool
    }

    #[test]
    fn inject_enabled_default_false() {
        let p = pool();
        assert!(!inject_enabled(&p));
    }

    #[test]
    fn set_enabled_round_trip() {
        let p = pool();
        set_enabled(&p, true).unwrap();
        assert!(inject_enabled(&p));
        set_enabled(&p, false).unwrap();
        assert!(!inject_enabled(&p));
    }

    #[test]
    fn type_text_skips_empty_and_error_marker() {
        // Должны выйти раньше любой попытки тронуть clipboard/enigo
        // (важно для headless CI, где display отсутствует).
        type_text("").unwrap();
        type_text("   ").unwrap();
        type_text("[error: model load failed]").unwrap();
    }

    #[test]
    fn is_skippable_classifies_correctly() {
        assert!(is_skippable(""));
        assert!(is_skippable("   \n"));
        assert!(is_skippable("[error: foo]"));
        assert!(!is_skippable("hello world"));
        assert!(!is_skippable("error: not bracketed"));
    }
}
