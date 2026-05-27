//! Fast-lane writer triggered when a task transitions to `complete`.
//! Composes a `tasks/<task-id>.md` stub with title, instructions,
//! knowledge, agent, and files-touched. No LLM calls.
