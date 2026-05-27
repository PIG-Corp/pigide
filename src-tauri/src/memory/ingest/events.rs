//! Event payload + emit helper for `memory://note.created`.
//!
//! Frontend listens for this event to play the ingest-pulse animation
//! and refresh the graph.

use crate::events::EV_MEMORY_NOTE_CREATED;
use crate::memory::folders::Kind;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone, Serialize)]
pub struct NoteCreatedPayload {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub kind: Kind,
    pub source_kind: String,
}

pub fn emit_note_created(app: &AppHandle, payload: &NoteCreatedPayload) {
    if let Err(e) = app.emit(EV_MEMORY_NOTE_CREATED, payload) {
        tracing::debug!("failed to emit {}: {}", EV_MEMORY_NOTE_CREATED, e);
    }
}
