//! PigSwarm: agent roles, mailbox, side-chat threads, roll-call.
//!
//! All persistence lives in tables created by db migration v7. Public surface
//! mirrors the orchestrator-tools naming so adding new tools later is just a
//! matter of wiring `tools.rs` to call into here.

pub mod mailbox;
pub mod ownership;
pub mod prompts;
pub mod review;
pub mod role;
pub mod rollcall;
pub mod tools;

pub use role::Role;
