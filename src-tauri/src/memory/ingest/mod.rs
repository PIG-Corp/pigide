//! Phase 1 fast-lane ingest pipeline + Phase 2 smart-lane queue.
//!
//! Two writers, both deterministic and LLM-free:
//!  - `task_complete` — `tasks/<task-id>.md` on every task→complete
//!  - `chat_chunk`    — `chats/<agent>/<yyyy-mm-dd>.md` from PTY stdout
//!
//! Each writer ends by emitting `memory://note.created` so the frontend
//! graph can animate. Phase 2 adds an `ingest_queue` populated by the
//! fast-lane and drained by the smart-lane worker.

pub mod chat_chunk;
pub mod events;
pub mod prompt;
pub mod queue;
pub mod task_complete;
