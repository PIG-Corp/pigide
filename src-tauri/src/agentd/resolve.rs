//! Spawn-arg resolution.
//!
//! PigIDE pulls together (binary path, argv, env) from several sources
//! before handing the result off to the broker:
//!
//!   - `bin.<type>` setting → falls back to install-candidates → PATH
//!   - `args.<type>` setting → falls back to per-type defaults
//!   - explicit caller override (e.g. `crate::ssh::spawn_preset` passes
//!     a fresh argv per host) — beats both
//!   - MCP auto-register for Claude tiles (`--mcp-config <json>`)
//!   - WSL prefix on Windows when `wsl.<type>=true`
//!   - env: TERM, COLORTERM, HOME, PATH (with `~/.local/bin` etc.
//!     prepended), LANG
//!
//! Broker has no DB and no MCP knowledge — it just runs `bin_path` with
//! `argv` in `cwd` with `env`. This module is the bridge.

use crate::agent::AgentType;
use crate::agentd::client::SpawnArgs;
use crate::db::DbPool;
use std::path::Path;
use std::sync::Arc;

/// Build a fully-resolved [`SpawnArgs`] for the broker. `args_override`,
/// when set, supersedes both the per-type setting and the built-in
/// default args. `mcp_handle`, when present and the spawn is a Claude
/// tile, injects the `--mcp-config` arg.
pub fn resolve_spawn(
    db: &DbPool,
    workspace_id: &str,
    agent_type: AgentType,
    cwd: Option<String>,
    args_override: Option<Vec<String>>,
    reuse_id: Option<String>,
    mcp_handle: Option<&Arc<crate::mcp::server::McpServerHandle>>,
) -> SpawnArgs {
    let bin_path = resolve_binary(db, &agent_type);
    let argv = resolve_argv(db, &agent_type, args_override, mcp_handle);
    let env = resolve_env();
    let (final_bin, final_argv) = wrap_with_wsl(db, &agent_type, bin_path, argv);

    SpawnArgs {
        workspace_id: workspace_id.into(),
        agent_type: agent_type.as_str().into(),
        cwd,
        bin_path: final_bin,
        argv: final_argv,
        env,
        reuse_id,
    }
}

/// Resolve the absolute binary path for `agent_type`.
///
/// Precedence: `bin.<type>` setting → install candidates (`~/.local/bin/X`,
/// `/usr/local/bin/X`, `/usr/bin/X`) → PATH lookup → bare command name
/// (relies on broker's `execvp` to do the final lookup).
pub fn resolve_binary(db: &DbPool, agent_type: &AgentType) -> String {
    let key = format!("bin.{}", agent_type.as_str());
    if let Ok(Some(v)) = crate::db::get_setting(db, &key) {
        if !v.trim().is_empty() {
            return v;
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates: Vec<String> = match agent_type {
        AgentType::KiroCli => vec![
            format!("{}/.local/bin/kiro-cli", home),
            "/usr/local/bin/kiro-cli".into(),
            "/usr/bin/kiro-cli".into(),
            "kiro-cli".into(),
        ],
        AgentType::Claude => vec![
            format!("{}/.local/bin/claude", home),
            "/usr/bin/claude".into(),
            "/usr/local/bin/claude".into(),
            "claude".into(),
        ],
        AgentType::Aider => vec![
            format!("{}/.local/bin/aider", home),
            "/usr/local/bin/aider".into(),
            "/usr/bin/aider".into(),
            "aider".into(),
        ],
        AgentType::Goose => vec![
            format!("{}/.local/bin/goose", home),
            "/usr/local/bin/goose".into(),
            "/usr/bin/goose".into(),
            "goose".into(),
        ],
        AgentType::OpenCode => vec![
            format!("{}/.local/bin/opencode", home),
            format!("{}/.opencode/bin/opencode", home),
            "/usr/local/bin/opencode".into(),
            "/usr/bin/opencode".into(),
            "opencode".into(),
        ],
        AgentType::Devin => vec![
            format!("{}/.local/bin/devin", home),
            "/usr/local/bin/devin".into(),
            "/usr/bin/devin".into(),
            "devin".into(),
        ],
        AgentType::Agy => vec![
            format!("{}/.local/bin/agy", home),
            "/usr/local/bin/agy".into(),
            "/usr/bin/agy".into(),
            "agy".into(),
        ],
        AgentType::Ssh => vec![
            "/usr/bin/ssh".into(),
            "/usr/local/bin/ssh".into(),
            "ssh".into(),
        ],
    };
    for c in &candidates {
        if !c.is_empty() && Path::new(c).exists() {
            return c.clone();
        }
    }
    let bin_name = agent_type.as_str();
    let names: &[&str] = match agent_type {
        AgentType::KiroCli => &["kiro-cli"],
        AgentType::Ssh => &["ssh"],
        _ => &[bin_name],
    };
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for name in names {
                let p = dir.join(name);
                if p.exists() {
                    return p.to_string_lossy().into_owned();
                }
            }
        }
    }
    names[0].into()
}

/// Resolve argv, in this precedence order:
/// 1. caller `args_override` (e.g. ssh preset)
/// 2. `args.<type>` setting (whitespace-split)
/// 3. per-type default args
///
/// For Claude tiles, append `--mcp-config <json>` when the in-process
/// MCP server is running so broker-spawned `claude` instances see the
/// `pigide` MCP server without touching `~/.claude.json`.
pub fn resolve_argv(
    db: &DbPool,
    agent_type: &AgentType,
    args_override: Option<Vec<String>>,
    mcp_handle: Option<&Arc<crate::mcp::server::McpServerHandle>>,
) -> Vec<String> {
    let default_args: &[&str] = match agent_type {
        AgentType::KiroCli => &["chat", "--trust-all-tools"],
        AgentType::Claude => &[],
        AgentType::Aider => &["--no-auto-commits"],
        AgentType::Goose => &["session"],
        AgentType::OpenCode => &[],
        AgentType::Devin => &[],
        AgentType::Agy => &[],
        AgentType::Ssh => &[],
    };

    let mut argv: Vec<String> = if let Some(explicit) = args_override {
        explicit
    } else {
        let arg_key = format!("args.{}", agent_type.as_str());
        let from_settings = crate::db::get_setting(db, &arg_key).ok().flatten();
        match from_settings {
            Some(s) => s.split_whitespace().map(String::from).collect(),
            None => default_args.iter().map(|s| String::from(*s)).collect(),
        }
    };

    if matches!(agent_type, AgentType::Claude) {
        if let Some(handle) = mcp_handle {
            match crate::mcp::launcher::build_mcp_config_arg(db, handle) {
                Ok(Some(cfg)) => {
                    argv.push("--mcp-config".into());
                    argv.push(cfg);
                }
                Ok(None) => {
                    tracing::info!(
                        "claude tile: MCP server not running, skipping --mcp-config"
                    );
                }
                Err(e) => {
                    tracing::warn!("claude tile: failed to build --mcp-config: {}", e);
                }
            }
        }
    }
    argv
}

/// Build env vars to inject into the child. Inherited env still applies
/// (broker's own env propagates to children); these override on collision
/// and ensure a sane TERM + a generous PATH that picks up tools the user
/// installed under their home dir.
pub fn resolve_env() -> Vec<(String, String)> {
    let mut env = vec![
        ("TERM".into(), "xterm-256color".into()),
        ("COLORTERM".into(), "truecolor".into()),
    ];
    if let Ok(home) = std::env::var("HOME") {
        env.push(("HOME".into(), home.clone()));
        let mut path = std::env::var("PATH").unwrap_or_default();
        let extra_dirs = [
            format!("{}/.local/bin", home),
            format!("{}/.opencode/bin", home),
            format!("{}/.cargo/bin", home),
            format!("{}/go/bin", home),
            format!("{}/.bun/bin", home),
        ];
        for dir in &extra_dirs {
            if !path.contains(dir.as_str()) && Path::new(dir).is_dir() {
                path = format!("{}:{}", dir, path);
            }
        }
        env.push(("PATH".into(), path));
    }
    if let Ok(lang) = std::env::var("LANG") {
        env.push(("LANG".into(), lang));
    }
    env
}

/// Optionally wrap (bin, argv) with `wsl.exe [-d distro] -- <bin> <argv>`
/// when `wsl.<type>=true` is set on Windows. No-op on non-Windows or when
/// the toggle is off. Returns the (possibly-rewritten) (bin, argv) pair.
pub fn wrap_with_wsl(
    db: &DbPool,
    agent_type: &AgentType,
    bin: String,
    argv: Vec<String>,
) -> (String, Vec<String>) {
    #[cfg(target_os = "windows")]
    {
        let on = crate::db::get_setting(db, &format!("wsl.{}", agent_type.as_str()))
            .ok()
            .flatten()
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if !on {
            return (bin, argv);
        }
        let exe = match resolve_wsl_exe() {
            Some(e) => e,
            None => return (bin, argv),
        };
        let distro = crate::db::get_setting(db, "wsl.distro")
            .ok()
            .flatten()
            .filter(|s| !s.trim().is_empty());
        let mut new_argv = Vec::new();
        if let Some(d) = distro {
            new_argv.push("-d".into());
            new_argv.push(d);
        }
        new_argv.push("--".into());
        new_argv.push(bin);
        new_argv.extend(argv);
        (exe, new_argv)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (db, agent_type);
        (bin, argv)
    }
}

#[cfg(target_os = "windows")]
fn resolve_wsl_exe() -> Option<String> {
    if let Ok(root) = std::env::var("SystemRoot") {
        let p = std::path::PathBuf::from(root)
            .join("System32")
            .join("wsl.exe");
        if p.exists() {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let p = dir.join("wsl.exe");
            if p.exists() {
                return Some(p.to_string_lossy().into_owned());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbPool;
    use rusqlite::params;

    /// Minimal in-memory pool with the only table this module touches:
    /// `settings`. Avoids the full migration path of `db::init_pool`.
    fn empty_pool() -> DbPool {
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(2).build(manager).unwrap();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
             );",
        )
        .unwrap();
        pool
    }

    fn pool_with_setting(key: &str, value: &str) -> DbPool {
        let p = empty_pool();
        let c = p.get().unwrap();
        c.execute(
            "INSERT INTO settings(key,value) VALUES(?1,?2)",
            params![key, value],
        )
        .unwrap();
        p
    }

    #[test]
    fn resolve_binary_uses_explicit_setting() {
        let p = pool_with_setting("bin.claude", "/opt/claude-x/bin/claude");
        let r = resolve_binary(&p, &AgentType::Claude);
        assert_eq!(r, "/opt/claude-x/bin/claude");
    }

    #[test]
    fn resolve_binary_falls_back_to_bare_name_when_nothing_found() {
        let p = empty_pool();
        // Force PATH lookup to whiff so we hit the bare-name fallback.
        let saved = std::env::var("PATH").ok();
        // Also clobber HOME so install candidates don't accidentally
        // resolve via /home/<user>/.local/bin etc.
        let saved_home = std::env::var("HOME").ok();
        std::env::set_var("PATH", "/non/existent");
        std::env::set_var("HOME", "/non/existent/home");
        let r = resolve_binary(&p, &AgentType::Aider);
        match saved {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
        match saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        assert_eq!(r, "aider");
    }

    #[test]
    fn argv_override_beats_settings_and_default() {
        let p = pool_with_setting("args.kiro-cli", "from-settings");
        let argv = resolve_argv(
            &p,
            &AgentType::KiroCli,
            Some(vec!["from-override".into()]),
            None,
        );
        assert_eq!(argv, vec!["from-override".to_string()]);
    }

    #[test]
    fn argv_settings_beat_defaults() {
        let p = pool_with_setting("args.aider", "--watch-files --map-tokens 8192");
        let argv = resolve_argv(&p, &AgentType::Aider, None, None);
        assert_eq!(
            argv,
            vec!["--watch-files".to_string(), "--map-tokens".into(), "8192".into()]
        );
    }

    #[test]
    fn argv_default_used_when_unset() {
        let p = empty_pool();
        let argv = resolve_argv(&p, &AgentType::KiroCli, None, None);
        assert_eq!(argv, vec!["chat".to_string(), "--trust-all-tools".into()]);
    }

    #[test]
    fn argv_claude_with_no_mcp_handle_has_no_mcp_arg() {
        let p = empty_pool();
        let argv = resolve_argv(&p, &AgentType::Claude, None, None);
        assert!(argv.iter().all(|a| a != "--mcp-config"));
    }

    #[test]
    fn env_includes_term_and_path() {
        let env = resolve_env();
        let map: std::collections::HashMap<_, _> = env.into_iter().collect();
        assert_eq!(map.get("TERM").map(String::as_str), Some("xterm-256color"));
        if std::env::var("HOME").is_ok() {
            assert!(map.contains_key("PATH"));
        }
    }

    #[test]
    fn wrap_with_wsl_is_noop_on_non_windows() {
        let p = empty_pool();
        let (bin, argv) =
            wrap_with_wsl(&p, &AgentType::Claude, "/x/claude".into(), vec!["a".into()]);
        assert_eq!(bin, "/x/claude");
        assert_eq!(argv, vec!["a".to_string()]);
    }

    #[test]
    fn resolve_spawn_carries_workspace_and_cwd() {
        let p = pool_with_setting("bin.claude", "/usr/bin/claude");
        let args = resolve_spawn(
            &p,
            "ws-1",
            AgentType::Claude,
            Some("/home/u/proj".into()),
            None,
            Some("fixed-id".into()),
            None,
        );
        assert_eq!(args.workspace_id, "ws-1");
        assert_eq!(args.bin_path, "/usr/bin/claude");
        assert_eq!(args.cwd.as_deref(), Some("/home/u/proj"));
        assert_eq!(args.reuse_id.as_deref(), Some("fixed-id"));
        assert_eq!(args.agent_type, "claude");
    }
}
