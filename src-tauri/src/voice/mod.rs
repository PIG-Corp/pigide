pub mod capture;
pub mod cloud;
pub mod dictionary;
pub mod download;
pub mod history;
pub mod hotkey;
pub mod inject;
pub mod mode;
pub mod whisper;

use crate::db::{self, DbPool};
use crate::error::Result;
use crate::events::{EV_VOICE_STATE, EV_VOICE_TRANSCRIPT};
use download::ModelId;
use parking_lot::{Mutex, RwLock};
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter};

pub struct VoicePipeline {
    db: DbPool,
    capture: Arc<capture::Capture>,
    /// Loaded whisper contexts, capped at MAX_LOADED_MODELS so the
    /// process can't OOM when the user toggles between whisper sizes.
    /// Each `large`/`medium` model is 1.5 GB on CPU; LRU eviction keeps
    /// memory bounded during long editing sessions and when the user
    /// keeps switching providers.
    whisper: Mutex<HashMap<String, (Arc<whisper::Whisper>, Instant)>>,
    last_emit: Mutex<Option<(String, Instant)>>,
    generation: Arc<AtomicU64>,
    app: RwLock<Option<AppHandle>>,
}

/// Hard cap on the number of whisper models held in RAM simultaneously.
/// The hot path is "one model at a time" — we keep at most two (the
/// current + the previous) so a `voice_set_model` swap doesn't have to
/// re-load from disk if the user toggles back and forth.
const MAX_LOADED_MODELS: usize = 2;

impl VoicePipeline {
    pub fn new(db: DbPool) -> Self {
        Self {
            db,
            capture: Arc::new(capture::Capture::new()),
            whisper: Mutex::new(HashMap::new()),
            last_emit: Mutex::new(None),
            generation: Arc::new(AtomicU64::new(0)),
            app: RwLock::new(None),
        }
    }

    pub fn set_app_handle(&self, app: AppHandle) {
        *self.app.write() = Some(app);
    }

    fn emit_state(&self, state: &str) {
        if let Some(app) = self.app.read().as_ref() {
            let _ = app.emit(EV_VOICE_STATE, json!({ "state": state }));
        }
    }

    fn emit_transcript(&self, text: &str) {
        let coalesce_window = std::time::Duration::from_secs(2);
        if !text.is_empty() && !text.starts_with("[error") {
            let mut slot = self.last_emit.lock();
            if let Some((prev, ts)) = slot.as_ref() {
                if prev == text && ts.elapsed() < coalesce_window {
                    return;
                }
            }
            *slot = Some((text.to_string(), Instant::now()));
        }
        if let Some(app) = self.app.read().as_ref() {
            let _ = app.emit(EV_VOICE_TRANSCRIPT, json!({ "text": text }));
        }
    }

    fn current_model(&self) -> ModelId {
        let raw = db::get_setting(&self.db, "whisper.model_id")
            .ok()
            .flatten()
            .unwrap_or_else(|| "small".to_string());
        ModelId::parse(&raw).unwrap_or(ModelId::Small)
    }

    pub fn start(&self) -> Result<()> {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.capture.start()?;
        self.emit_state("recording");
        Ok(())
    }

    pub async fn stop_and_transcribe(self: Arc<Self>) -> Result<()> {
        let gen_at_entry = self.generation.load(Ordering::SeqCst);
        let is_current = || self.generation.load(Ordering::SeqCst) == gen_at_entry;

        let (samples, src_rate) = self.capture.stop()?;
        let started_at = std::time::Instant::now();
        if is_current() {
            self.emit_state("transcribing");
        }

        if samples.is_empty() {
            if is_current() {
                self.emit_state("idle");
            }
            return Ok(());
        }

        let language = db::get_setting(&self.db, "whisper.language")
            .ok()
            .flatten()
            .unwrap_or_else(|| "auto".to_string());
        let app = self.app.read().clone();
        let model_id = self.current_model();

        let model_path = match download::ensure_model(model_id, app.clone()).await {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("whisper model unavailable: {}", e);
                if is_current() {
                    self.emit_transcript(&format!("[error: speech model unavailable: {}]", e));
                    self.emit_state("idle");
                }
                return Ok(());
            }
        };

        let whisper = {
            let mut slot = self.whisper.lock();
            let key = model_id.as_str().to_string();
            // Touch the LRU timestamp on a hit, evict cold entries when
            // we're at capacity, then load on miss. Eviction drops the
            // `Arc` — when the last reference is gone, the model memory
            // is freed.
            if let Some((_, ts)) = slot.get_mut(&key) {
                *ts = Instant::now();
            } else {
                match whisper::Whisper::open(&model_path, &key) {
                    Ok(w) => {
                        // LRU eviction: drop the oldest entry if adding
                        // a new one would exceed the cap.
                        while slot.len() >= MAX_LOADED_MODELS {
                            if let Some(oldest_key) = slot
                                .iter()
                                .min_by_key(|(_, (_, ts))| *ts)
                                .map(|(k, _)| k.clone())
                            {
                                slot.remove(&oldest_key);
                            } else {
                                break;
                            }
                        }
                        slot.insert(key.clone(), (Arc::new(w), Instant::now()));
                    }
                    Err(e) => {
                        drop(slot);
                        if is_current() {
                            self.emit_transcript(&format!("[error: whisper init: {}]", e));
                            self.emit_state("idle");
                        }
                        return Ok(());
                    }
                }
            }
            slot.get(&key).unwrap().0.clone()
        };

        let resampled = capture::resample_to_16k(&samples, src_rate);
        let lang = language.clone();
        // Run on the blocking pool. We mark the closure with a
        // `defer!` to drop our own OS-thread priority hint right
        // after the inference returns — a no-op on Windows and a
        // `setpriority(PRIO_PROCESS, 0, +nice)` on Linux/macOS,
        // which tells the scheduler to prefer the user's
        // foreground app (game, video editor) over our transcription
        // thread. The `nice` value is positive = lower priority.
        let result = tokio::task::spawn_blocking(move || {
            #[cfg(unix)]
            let _prio_guard = VoicePriorityGuard::new();
            whisper.transcribe(&resampled, &lang)
        })
        .await
        .unwrap_or_else(|e| Err(crate::error::Error::Voice(format!("blocking: {}", e))));

        if !is_current() {
            tracing::info!(
                "voice: dropping stale transcription (gen={}, current={})",
                gen_at_entry,
                self.generation.load(Ordering::SeqCst)
            );
            return Ok(());
        }

        let duration_ms = started_at.elapsed().as_millis() as i64;
        let model_str = model_id.as_str().to_string();
        let lang_opt = if language.is_empty() || language == "auto" {
            None
        } else {
            Some(language.as_str())
        };
        match result {
            Ok(raw_text) => {
                let final_text = dictionary::apply(&self.db, &raw_text).unwrap_or_else(|e| {
                    tracing::warn!("dictionary apply: {}", e);
                    raw_text.clone()
                });
                let _ = history::insert(
                    &self.db,
                    &final_text,
                    &raw_text,
                    lang_opt,
                    &model_str,
                    "local",
                    duration_ms,
                );
                self.emit_transcript(&final_text);
                if inject::inject_enabled(&self.db) && !self.is_pigide_focused() {
                    if let Err(e) = inject::type_text(&final_text) {
                        tracing::warn!("inject: {}", e);
                    }
                }
            }
            Err(e) => self.emit_transcript(&format!("[error: {}]", e)),
        }
        self.emit_state("idle");
        Ok(())
    }

    pub fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        if self.capture.is_recording() {
            let _ = self.capture.stop();
        }
        self.emit_state("idle");
    }

    pub fn evict_model(&self, model_id: &str) {
        self.whisper.lock().remove(model_id);
    }

    fn is_pigide_focused(&self) -> bool {
        use tauri::Manager;
        match self.app.read().as_ref() {
            Some(app) => app
                .webview_windows()
                .values()
                .any(|w| w.is_focused().unwrap_or(false)),
            None => false,
        }
    }
}

/// RAII guard that nudges the current OS thread toward *lower* priority
/// while a whisper inference is running, and restores the previous
/// value on drop. Why: when a user is gaming or running another GPU-
/// heavy foreground app, our transcription thread can steal CPU from
/// the render loop and cause visible stutter. `nice +5` is enough to
/// surrender a few CPU cores' worth of contention without making the
/// transcription feel sluggish.
///
/// This is best-effort — on platforms where the syscall isn't available
/// the guard is a no-op and the inference runs at default priority
/// (which is already capped at 2 CPU threads by `WHISPER_THREAD_BUDGET`).
#[cfg(unix)]
struct VoicePriorityGuard {
    prev: i32,
}

#[cfg(unix)]
impl VoicePriorityGuard {
    fn new() -> Self {
        let prev = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
        // Bump niceness. range is -20..19; we set to +5 = "below normal
        // interactive, above idle". Failure to setpriority is harmless
        // (likely EACCES in a sandbox), so we swallow it.
        unsafe {
            let _ = libc::setpriority(libc::PRIO_PROCESS, 0, 5);
        }
        Self { prev }
    }
}

#[cfg(unix)]
impl Drop for VoicePriorityGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = libc::setpriority(libc::PRIO_PROCESS, 0, self.prev);
        }
    }
}
