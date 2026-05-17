//! Global hotkey integration for voice capture.
//!
//! Wires `tauri-plugin-global-shortcut` into the [`VoicePipeline`] through the
//! [`mode::ModeController`] state machine. Press/release edges are translated
//! into [`mode::Action`]s, and any registration error (compositor conflict,
//! missing portal, Wayland refusal) is surfaced to the frontend via the
//! `voice://hotkey-error` event rather than panicking.

use crate::db::{self, DbPool};
use crate::error::{Error, Result};
use crate::voice::mode::{self, Action, ModeController, RecordMode};
use crate::voice::VoicePipeline;
use parking_lot::RwLock;
use serde_json::json;
use std::str::FromStr;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

/// Settings key holding the user-configured accelerator string.
const SETTING_KEY: &str = "voice.hotkey";
/// Default accelerator if the setting is missing or unparseable.
pub const DEFAULT_HOTKEY: &str = "Alt+Space";
/// Frontend-facing error channel.
pub const EV_HOTKEY_ERROR: &str = "voice://hotkey-error";

/// Tauri-managed handle to the currently registered shortcut. We keep the
/// parsed value so [`unregister`] can match exactly what was registered, even
/// after the user mutates the settings string.
#[derive(Default)]
pub struct HotkeyRegistration {
    current: RwLock<Option<Shortcut>>,
}

impl HotkeyRegistration {
    fn store(&self, sc: Shortcut) {
        *self.current.write() = Some(sc);
    }

    fn take(&self) -> Option<Shortcut> {
        self.current.write().take()
    }
}

/// Newtype around `Arc<ModeController>` so it can be stored in Tauri's state
/// container and cloned freely without lifetime gymnastics.
#[derive(Clone)]
pub struct SharedModeController(pub Arc<ModeController>);

impl SharedModeController {
    fn new() -> Self {
        Self(Arc::new(ModeController::new()))
    }
}

/// Read the configured accelerator from settings, falling back to the default.
fn configured_accelerator(db: &DbPool) -> String {
    db::get_setting(db, SETTING_KEY)
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_HOTKEY.to_string())
}

/// Emit a structured hotkey error to the frontend. Never panics.
fn emit_error<R: Runtime>(app: &AppHandle<R>, err: &str) {
    tracing::warn!("voice hotkey error: {}", err);
    let _ = app.emit(EV_HOTKEY_ERROR, json!({ "error": err }));
}

/// Register the hotkey from settings and wire it to the voice pipeline.
///
/// On failure (compositor conflict, missing global-shortcut backend on
/// Wayland, malformed accelerator) the error is surfaced via
/// `voice://hotkey-error` and `Ok(())` is still returned — voice capture
/// remains usable through the UI button.
pub fn register<R: Runtime>(
    app: &AppHandle<R>,
    voice: Arc<VoicePipeline>,
    db: DbPool,
) -> Result<()> {
    // Lazily install the registration slot so multiple register/unregister
    // cycles share the same RwLock.
    if app.try_state::<HotkeyRegistration>().is_none() {
        app.manage(HotkeyRegistration::default());
    }
    // The controller is stored as Arc so press/release handlers can grab a
    // cheap clone; an in-flight Listening state survives a hotkey rebind.
    if app.try_state::<SharedModeController>().is_none() {
        app.manage(SharedModeController::new());
    }

    let accel = configured_accelerator(&db);
    register_inner(app, &accel, voice, db)
}

/// Switch to a new accelerator. Old binding is removed first; on parse or
/// registration failure the old binding is restored so voice keeps working.
pub fn set_hotkey<R: Runtime>(
    app: &AppHandle<R>,
    voice: Arc<VoicePipeline>,
    db: DbPool,
    accel: &str,
) -> Result<()> {
    let trimmed = accel.trim();
    if trimmed.is_empty() {
        return Err(Error::Invalid("hotkey accelerator is empty".into()));
    }

    // Snapshot the previous accelerator string before tearing it down so we
    // can roll back to the exact same binding if the new one fails.
    let previous_accel = db::get_setting(&db, SETTING_KEY)
        .ok()
        .flatten()
        .unwrap_or_else(|| DEFAULT_HOTKEY.to_string());

    unregister(app);
    match register_inner(app, trimmed, voice.clone(), db.clone()) {
        Ok(()) => {
            db::set_setting(&db, SETTING_KEY, trimmed)?;
            Ok(())
        }
        Err(err) => {
            // Best-effort rollback so the user isn't left without a hotkey.
            if let Err(roll_err) =
                register_inner(app, &previous_accel, voice, db.clone())
            {
                tracing::warn!(
                    "failed to roll back voice hotkey to {:?}: {}",
                    previous_accel,
                    roll_err
                );
            }
            Err(err)
        }
    }
}

/// Unregister the active accelerator (if any).
pub fn unregister<R: Runtime>(app: &AppHandle<R>) {
    let Some(slot) = app.try_state::<HotkeyRegistration>() else {
        return;
    };
    let Some(current) = slot.take() else {
        return;
    };
    if let Err(err) = app.global_shortcut().unregister(current) {
        tracing::warn!("voice hotkey unregister: {}", err);
    }
}

/// Parse `accel`, register it with a press/release handler that drives the
/// [`ModeController`], and stash the parsed shortcut for later teardown.
fn register_inner<R: Runtime>(
    app: &AppHandle<R>,
    accel: &str,
    voice: Arc<VoicePipeline>,
    db: DbPool,
) -> Result<()> {
    let shortcut = Shortcut::from_str(accel).map_err(|e| {
        let msg = format!("invalid hotkey {:?}: {}", accel, e);
        emit_error(app, &msg);
        Error::Voice(msg)
    })?;

    let controller = app
        .try_state::<SharedModeController>()
        .map(|s| s.0.clone())
        .ok_or_else(|| {
            Error::Voice("ModeController not managed; call register() first".into())
        })?;

    let voice_for_handler = voice;
    let db_for_handler = db;
    let controller_for_handler = controller;

    let reg_result = app.global_shortcut().on_shortcut(
        shortcut.clone(),
        move |handle, _sc, event| {
            // The handler is invoked from the global-hotkey background thread.
            // Keep it cheap: read the mode, ask the controller what to do,
            // then dispatch the heavy work to a tokio task.
            let voice = voice_for_handler.clone();
            let controller = controller_for_handler.clone();
            let mode = mode::current_mode(&db_for_handler);
            let action = match event.state() {
                ShortcutState::Pressed => controller.on_press(mode),
                ShortcutState::Released => controller.on_release(mode),
            };
            dispatch(handle.clone(), voice, controller, action);
        },
    );

    if let Err(err) = reg_result {
        let msg = format!("failed to register hotkey {:?}: {}", accel, err);
        emit_error(app, &msg);
        return Err(Error::Voice(msg));
    }

    if let Some(slot) = app.try_state::<HotkeyRegistration>() {
        slot.store(shortcut);
    }
    tracing::info!("voice hotkey bound to {:?}", accel);
    Ok(())
}

/// Execute the action returned by the controller. `Start` runs synchronously
/// because cpal stream construction is fast and we want errors to surface
/// before the user thinks recording is live; `StopAndTranscribe` is offloaded
/// to a tokio task because Whisper inference takes seconds.
fn dispatch<R: Runtime>(
    app: AppHandle<R>,
    voice: Arc<VoicePipeline>,
    controller: Arc<ModeController>,
    action: Action,
) {
    match action {
        Action::Ignore => {}
        Action::Start => {
            if let Err(e) = voice.start() {
                tracing::error!("voice start failed: {}", e);
                controller.reset();
                emit_error(&app, &format!("voice start failed: {}", e));
            }
        }
        Action::StopAndTranscribe => {
            let v = voice.clone();
            let c = controller.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = v.stop_and_transcribe().await {
                    tracing::error!("voice stop/transcribe failed: {}", e);
                }
                c.finish_processing();
            });
        }
        Action::Cancel => {
            // Third hotkey press during Processing — bin the in-flight
            // transcription so it can't race onto the user's draft input.
            voice.cancel();
            controller.finish_processing();
        }
    }
}

/// Convenience: read the persisted record mode from settings. Re-exported
/// here so command handlers wiring up the UI don't need to depend on
/// [`crate::voice::mode`] directly.
pub fn current_mode(db: &DbPool) -> RecordMode {
    mode::current_mode(db)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> DbPool {
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(2).build(manager).unwrap();
        pool.get()
            .unwrap()
            .execute_batch(
                "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
            )
            .unwrap();
        pool
    }

    #[test]
    fn default_accelerator_when_setting_missing() {
        let p = pool();
        assert_eq!(configured_accelerator(&p), DEFAULT_HOTKEY);

        db::set_setting(&p, SETTING_KEY, "CommandOrControl+Shift+V").unwrap();
        assert_eq!(configured_accelerator(&p), "CommandOrControl+Shift+V");
    }

    #[test]
    fn empty_setting_falls_back_to_default() {
        let p = pool();
        db::set_setting(&p, SETTING_KEY, "   ").unwrap();
        assert_eq!(configured_accelerator(&p), DEFAULT_HOTKEY);
    }
}
