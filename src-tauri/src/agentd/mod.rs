//! Broker module — wire protocol and (later) client stub.
//!
//! Layout:
//! - `proto`: serde types for the JSON-RPC protocol over unix socket.
//! - `framing`: NDJSON framer with `MAX_FRAME_BYTES` enforcement.
//! - `client` (later): `AgentClient` that mirrors `AgentManager`'s public API.
//!
//! The broker binary itself lives in `src/bin/pigide-agentd.rs` and uses
//! these modules through the `pigide_lib` crate.

pub mod client;
pub mod engine;
pub mod framing;
pub mod proto;
pub mod resolve;
pub mod server;
pub mod supervisor;
