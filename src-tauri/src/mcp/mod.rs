//! PigMCP — Model Context Protocol server.
//!
//! Exposes the orchestrator's tool registry over JSON-RPC 2.0 on a local HTTP
//! endpoint so external clients (Cursor, Claude Code, Codex CLI) can drive
//! PigIDE the same way the in-process orchestrator does.
//!
//! Transport: a single `POST /mcp` endpoint that accepts standard MCP JSON-RPC
//! messages (`tools/list`, `tools/call`, `prompts/list`, `prompts/get`,
//! `initialize`). Bearer authentication via `Authorization` header or
//! `?apiKey=` query, scoped per key.

pub mod auth;
pub mod launcher;
pub mod server;
