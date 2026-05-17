//! Recording-mode state machine for the voice subsystem.
//!
//! Drives the same global hotkey through two distinct interaction models:
//!
//! * [`RecordMode::PushToTalk`] — hold to record, release to transcribe.
//! * [`RecordMode::Toggle`] — first press starts, second press stops.
//!
//! The mode is persisted in `settings.voice.record_mode`. The controller
//! debounces repeats faster than 150ms to absorb mechanical switch bounce
//! and stray OS auto-repeat events seen on Wayland/X11.

use crate::db::{self, DbPool};
use crate::error::Result;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

const SETTING_KEY: &str = "voice.record_mode";
const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(150);

/// User-facing recording mode. Serialised in kebab-case so the frontend
/// can set the value via the settings KV without mapping helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecordMode {
    PushToTalk,
    Toggle,
}

impl RecordMode {
    /// Tolerant parser. Accepts the canonical settings value `"ptt"` along
    /// with the serde kebab-case form and a couple of common variants.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ptt" | "push-to-talk" | "push_to_talk" | "pushtotalk" => {
                Some(Self::PushToTalk)
            }
            "toggle" => Some(Self::Toggle),
            _ => None,
        }
    }

    /// Canonical string form written to settings.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PushToTalk => "push-to-talk",
            Self::Toggle => "toggle",
        }
    }
}

/// Internal recording state tracked across hotkey events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceState {
    Idle,
    Listening,
    Processing,
}

/// Outcome of a single hotkey event — what the caller should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Begin audio capture.
    Start,
    /// Stop capture and run Whisper.
    StopAndTranscribe,
    /// Abort an in-flight transcription. Any pending result must be dropped
    /// instead of being injected into the user's draft input.
    Cancel,
    /// No-op (debounced, irrelevant edge, or wrong state for the mode).
    Ignore,
}

/// Read `voice.record_mode` from settings, defaulting to PushToTalk.
pub fn current_mode(db: &DbPool) -> RecordMode {
    db::get_setting(db, SETTING_KEY)
        .ok()
        .flatten()
        .and_then(|v| RecordMode::parse(&v))
        .unwrap_or(RecordMode::PushToTalk)
}

/// Persist the record mode to settings.
pub fn set_mode(db: &DbPool, mode: RecordMode) -> Result<()> {
    db::set_setting(db, SETTING_KEY, mode.as_str())
}

/// Tracks the recording state machine plus a small debounce window so that
/// keyboard auto-repeat and bounce don't produce spurious transitions.
pub struct ModeController {
    state: Mutex<VoiceState>,
    last_event_at: Mutex<Option<Instant>>,
    debounce: Duration,
}

impl Default for ModeController {
    fn default() -> Self {
        Self::new()
    }
}

impl ModeController {
    /// Production constructor — 150ms debounce.
    pub fn new() -> Self {
        Self::with_debounce(DEFAULT_DEBOUNCE)
    }

    /// Construct with a custom debounce window. `Duration::ZERO` disables it,
    /// which is convenient for unit tests that need deterministic timing.
    pub fn with_debounce(debounce: Duration) -> Self {
        Self {
            state: Mutex::new(VoiceState::Idle),
            last_event_at: Mutex::new(None),
            debounce,
        }
    }

    /// Snapshot the current state. Mainly useful for tests and diagnostics.
    pub fn state(&self) -> VoiceState {
        *self.state.lock()
    }

    /// Move Processing back to Idle. Called by the hotkey dispatcher once
    /// `stop_and_transcribe()` resolves. Idempotent — anything other than
    /// Processing is left untouched.
    pub fn finish_processing(&self) {
        let mut s = self.state.lock();
        if matches!(*s, VoiceState::Processing) {
            *s = VoiceState::Idle;
        }
    }

    /// Force the controller back to a clean Idle. Used when audio capture
    /// fails to start so the next hotkey event isn't stuck in Listening.
    pub fn reset(&self) {
        *self.state.lock() = VoiceState::Idle;
        *self.last_event_at.lock() = None;
    }

    /// Returns `true` if the event arrived too quickly after the last one.
    fn debounced(&self) -> bool {
        let now = Instant::now();
        let mut slot = self.last_event_at.lock();
        if let Some(prev) = *slot {
            if now.duration_since(prev) < self.debounce {
                return true;
            }
        }
        *slot = Some(now);
        false
    }

    /// Handle a hotkey "press" edge.
    pub fn on_press(&self, mode: RecordMode) -> Action {
        if self.debounced() {
            return Action::Ignore;
        }
        let mut state = self.state.lock();
        match (mode, *state) {
            // PTT: only the Idle→Listening transition reacts to press.
            (RecordMode::PushToTalk, VoiceState::Idle) => {
                *state = VoiceState::Listening;
                Action::Start
            }
            (RecordMode::PushToTalk, _) => Action::Ignore,

            // Toggle: press cycles Idle → Listening → Processing.
            (RecordMode::Toggle, VoiceState::Idle) => {
                *state = VoiceState::Listening;
                Action::Start
            }
            (RecordMode::Toggle, VoiceState::Listening) => {
                *state = VoiceState::Processing;
                Action::StopAndTranscribe
            }
            (RecordMode::Toggle, VoiceState::Processing) => {
                *state = VoiceState::Idle;
                Action::Cancel
            }
        }
    }

    /// Handle a hotkey "release" edge.
    pub fn on_release(&self, mode: RecordMode) -> Action {
        if self.debounced() {
            return Action::Ignore;
        }
        match mode {
            // Toggle ignores releases entirely.
            RecordMode::Toggle => Action::Ignore,
            RecordMode::PushToTalk => {
                let mut state = self.state.lock();
                if matches!(*state, VoiceState::Listening) {
                    *state = VoiceState::Processing;
                    Action::StopAndTranscribe
                } else {
                    Action::Ignore
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zero-debounce controller so tests can fire events back-to-back
    /// without sleeping or mocking a clock.
    fn ctrl() -> ModeController {
        ModeController::with_debounce(Duration::ZERO)
    }

    #[test]
    fn ptt_happy_path() {
        let c = ctrl();
        assert_eq!(c.on_press(RecordMode::PushToTalk), Action::Start);
        assert_eq!(c.state(), VoiceState::Listening);
        assert_eq!(
            c.on_release(RecordMode::PushToTalk),
            Action::StopAndTranscribe
        );
        assert_eq!(c.state(), VoiceState::Processing);
        c.finish_processing();
        assert_eq!(c.state(), VoiceState::Idle);
    }

    #[test]
    fn toggle_press_press_flow() {
        let c = ctrl();
        // First press starts capture.
        assert_eq!(c.on_press(RecordMode::Toggle), Action::Start);
        assert_eq!(c.state(), VoiceState::Listening);
        // Releases are inert in toggle mode.
        assert_eq!(c.on_release(RecordMode::Toggle), Action::Ignore);
        // Second press stops capture and kicks off transcription.
        assert_eq!(c.on_press(RecordMode::Toggle), Action::StopAndTranscribe);
        assert_eq!(c.state(), VoiceState::Processing);
        c.finish_processing();
        assert_eq!(c.state(), VoiceState::Idle);
    }

    #[test]
    fn toggle_press_during_processing_cancels() {
        let c = ctrl();
        // Start.
        assert_eq!(c.on_press(RecordMode::Toggle), Action::Start);
        // Stop -> Processing.
        assert_eq!(c.on_press(RecordMode::Toggle), Action::StopAndTranscribe);
        assert_eq!(c.state(), VoiceState::Processing);
        // Third press during Processing aborts the in-flight transcription
        // and returns to Idle so the next press starts a fresh recording.
        assert_eq!(c.on_press(RecordMode::Toggle), Action::Cancel);
        assert_eq!(c.state(), VoiceState::Idle);
        // Fresh press starts again.
        assert_eq!(c.on_press(RecordMode::Toggle), Action::Start);
        assert_eq!(c.state(), VoiceState::Listening);
    }
}
