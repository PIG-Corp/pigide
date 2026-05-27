//! Phase 1 fast-lane ingest pipeline.
//!
//! Two writers, both deterministic and LLM-free:
//!  - `task_complete` — `tasks/<task-id>.md` on every task→complete
//!  - `chat_chunk`    — `chats/<agent>/<yyyy-mm-dd>.md` from PTY stdout
//!
//! Each writer ends by emitting `memory://note.created` so the frontend
//! graph can animate.

pub mod chat_chunk;
pub mod events;
pub mod task_complete;
