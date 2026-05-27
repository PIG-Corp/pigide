//! Fast-lane writer that buffers PTY stdout per agent and flushes a
//! `chats/<agent>/<yyyy-mm-dd>.md` chunk on threshold or agent exit.
