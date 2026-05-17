//! Helpers for auto-registering the local PigMCP server with spawned Claude
//! Code tiles.
//!
//! Two surfaces:
//!   * [`build_mcp_config_arg`] — produces the JSON value passed via
//!     `claude --mcp-config <json>` so a freshly-spawned tile sees the
//!     `pigide` MCP server without any user-level setup. Uses an in-process
//!     bearer token cached in the `settings` table; never touches
//!     `~/.claude.json` and so cannot clobber existing user servers
//!     (bottle, etc.).
//!   * [`merge_project_mcp_json`] — idempotent merge into `<cwd>/.mcp.json`
//!     used by the one-shot fix-up command for already-running tiles. Adds
//!     a `pigide` entry if absent and otherwise leaves the file alone.
//!
//! Both share [`tile_mcp_token`] which mints (and caches) a `tile-claude`
//! API key with the `read,mutate,dangerous` scopes — every tool the
//! orchestrator exposes through `tools/call` is reachable through it.
//!
//! The token is cached in the `settings` row `mcp.tile_token` so subsequent
//! tile spawns reuse the same bearer instead of bloating the
//! `mcp_api_keys` table.
//!
//! NOTE: registration is a no-op if the MCP server isn't running; in that
//! case Claude Code starts without `pigide` and a later `mcp_register_cwd`
//! fix-up call (or a tile restart) will wire it up.

use crate::db::{self, DbPool};
use crate::error::{Error, Result};
use crate::mcp::auth;
use crate::mcp::server::McpServerHandle;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

/// Settings key under which the cached tile bearer is stored.
const TILE_TOKEN_KEY: &str = "mcp.tile_token";
/// Label written to `mcp_api_keys.label` for the auto-minted tile key.
const TILE_KEY_LABEL: &str = "tile-claude";

/// Mint (or recover) the bearer used by Claude tiles.
///
/// We can't recover the plaintext from the DB once it's hashed, so the
/// freshly-minted plaintext is stashed in `settings.mcp.tile_token`. If a
/// later run finds the cached token but the matching `mcp_api_keys` row was
/// deleted (e.g. user revoked from the UI), we transparently re-mint.
fn tile_mcp_token(db: &DbPool) -> Result<String> {
    if let Ok(Some(cached)) = db::get_setting(db, TILE_TOKEN_KEY) {
        if !cached.trim().is_empty() {
            if let Ok(Some(_info)) = auth::validate(db, &cached) {
                return Ok(cached);
            }
        }
    }
    let created = auth::create(
        db,
        TILE_KEY_LABEL,
        vec!["read".into(), "mutate".into(), "dangerous".into()],
    )?;
    db::set_setting(db, TILE_TOKEN_KEY, &created.plaintext)?;
    Ok(created.plaintext)
}

/// JSON-RPC URL of the running MCP server, including bearer-as-query so the
/// transport layer doesn't have to thread auth headers separately.
fn mcp_url(addr: std::net::SocketAddr, token: &str) -> String {
    let host = match addr {
        std::net::SocketAddr::V4(v4) if v4.ip().is_unspecified() => "127.0.0.1".to_string(),
        std::net::SocketAddr::V6(v6) if v6.ip().is_unspecified() => "127.0.0.1".to_string(),
        other => other.ip().to_string(),
    };
    format!("http://{}:{}/mcp?apiKey={}", host, addr.port(), token)
}

fn server_block(addr: std::net::SocketAddr, token: &str) -> Value {
    json!({
        "type": "http",
        "url": mcp_url(addr, token),
        "headers": {
            "Authorization": format!("Bearer {}", token)
        }
    })
}

/// Returns the value to pass as `--mcp-config <json>` when spawning a
/// Claude tile. `Ok(None)` means the MCP server isn't running yet — caller
/// should skip the flag rather than fail the spawn.
pub fn build_mcp_config_arg(
    db: &DbPool,
    handle: &Arc<McpServerHandle>,
) -> Result<Option<String>> {
    let Some(addr) = handle.current_addr() else {
        return Ok(None);
    };
    let token = tile_mcp_token(db)?;
    let cfg = json!({
        "mcpServers": {
            "pigide": server_block(addr, &token)
        }
    });
    Ok(Some(cfg.to_string()))
}

/// Idempotently merge a `pigide` entry into `<cwd>/.mcp.json`. Used by the
/// `mcp_register_cwd` Tauri/CLI command to wire up tiles that were spawned
/// before this feature shipped, without restarting them — the next `/mcp`
/// reload inside Claude Code will pick the file up.
///
/// Returns `true` if the file was written (added or updated), `false` if
/// the existing entry already pointed at the running server (no-op).
pub fn merge_project_mcp_json(
    db: &DbPool,
    handle: &Arc<McpServerHandle>,
    cwd: &Path,
) -> Result<bool> {
    let addr = handle
        .current_addr()
        .ok_or_else(|| Error::Other("MCP server is not running".into()))?;
    let token = tile_mcp_token(db)?;
    let target = cwd.join(".mcp.json");

    let mut root: Value = if target.exists() {
        let raw = std::fs::read_to_string(&target)
            .map_err(|e| Error::Other(format!("read {}: {}", target.display(), e)))?;
        if raw.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&raw)
                .map_err(|e| Error::Other(format!("parse {}: {}", target.display(), e)))?
        }
    } else {
        json!({})
    };

    if !root.is_object() {
        return Err(Error::Other(format!(
            "{} is not a JSON object",
            target.display()
        )));
    }
    let obj = root.as_object_mut().expect("checked above");
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| json!({}));
    if !servers.is_object() {
        return Err(Error::Other(format!(
            "{}: mcpServers must be an object",
            target.display()
        )));
    }
    let new_block = server_block(addr, &token);
    let servers_obj = servers.as_object_mut().expect("checked above");
    if servers_obj.get("pigide") == Some(&new_block) {
        return Ok(false);
    }
    servers_obj.insert("pigide".into(), new_block);

    let serialized = serde_json::to_string_pretty(&root)
        .map_err(|e| Error::Other(format!("serialize: {}", e)))?;
    std::fs::write(&target, serialized)
        .map_err(|e| Error::Other(format!("write {}: {}", target.display(), e)))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    #[test]
    fn url_collapses_unspecified_to_loopback() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 20129);
        assert_eq!(mcp_url(addr, "tok"), "http://127.0.0.1:20129/mcp?apiKey=tok");
    }

    #[test]
    fn url_uses_explicit_loopback_as_is() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 20129);
        assert_eq!(mcp_url(addr, "tok"), "http://127.0.0.1:20129/mcp?apiKey=tok");
    }
}
