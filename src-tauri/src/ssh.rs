//! SSH presets (#14).
//!
//! Lightweight wrapper around the system `ssh` binary that lets a user save
//! reusable connection profiles ("prod-bastion", "raspberry-pi" etc.) and
//! launch them as ordinary PTY agents — no extra crate, the host shell does
//! all the work. Stored argv is the user's escape hatch; if a preset needs
//! `-L 8080:localhost:8080` we just put it there verbatim.

use crate::agent::{Agent, AgentManager, AgentType};
use crate::db::DbPool;
use crate::error::{Error, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshPreset {
    pub id: String,
    pub name: String,
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity: Option<String>,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct CreatePresetArgs {
    pub name: String,
    pub host: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub identity: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

fn from_row(row: &rusqlite::Row) -> rusqlite::Result<SshPreset> {
    let args_json: String = row.get(6)?;
    let args: Vec<String> = serde_json::from_str(&args_json).unwrap_or_default();
    let port: Option<i64> = row.get(4)?;
    Ok(SshPreset {
        id: row.get(0)?,
        name: row.get(1)?,
        host: row.get(2)?,
        user: row.get(3)?,
        port: port.map(|p| p as u16),
        identity: row.get(5)?,
        args,
        cwd: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

pub fn create(db: &DbPool, args: CreatePresetArgs) -> Result<SshPreset> {
    if args.name.trim().is_empty() {
        return Err(Error::Invalid("preset name required".into()));
    }
    if args.host.trim().is_empty() {
        return Err(Error::Invalid("host required".into()));
    }
    let id = Uuid::new_v4().to_string();
    let ts = Utc::now().to_rfc3339();
    let args_json = serde_json::to_string(&args.args)?;
    let port = args.port.map(|p| p as i64);
    let conn = db.get()?;
    conn.execute(
        "INSERT INTO ssh_presets(id,name,host,user,port,identity,args_json,cwd,created_at,updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?9)",
        rusqlite::params![
            &id, &args.name, &args.host, &args.user, &port,
            &args.identity, &args_json, &args.cwd, &ts,
        ],
    )
    .map_err(|e| {
        if let rusqlite::Error::SqliteFailure(_, Some(msg)) = &e {
            if msg.contains("UNIQUE") {
                return Error::Invalid(format!("preset '{}' already exists", args.name));
            }
        }
        Error::from(e)
    })?;
    Ok(SshPreset {
        id,
        name: args.name,
        host: args.host,
        user: args.user,
        port: args.port,
        identity: args.identity,
        args: args.args,
        cwd: args.cwd,
        created_at: ts.clone(),
        updated_at: ts,
    })
}

pub fn delete(db: &DbPool, id: &str) -> Result<bool> {
    let conn = db.get()?;
    let n = conn.execute("DELETE FROM ssh_presets WHERE id=?1", [id])?;
    Ok(n == 1)
}

pub fn get(db: &DbPool, id: &str) -> Result<SshPreset> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id,name,host,user,port,identity,args_json,cwd,created_at,updated_at
         FROM ssh_presets WHERE id=?1",
    )?;
    let mut rows = stmt.query([id])?;
    let row = rows
        .next()?
        .ok_or_else(|| Error::NotFound(format!("ssh_preset {}", id)))?;
    Ok(from_row(row)?)
}

pub fn list(db: &DbPool) -> Result<Vec<SshPreset>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id,name,host,user,port,identity,args_json,cwd,created_at,updated_at
         FROM ssh_presets ORDER BY name ASC",
    )?;
    let rows = stmt.query_map([], from_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Build the argv handed to `ssh`. Order:
///   [-p PORT] [-i IDENTITY] (extra args) (user@)host
pub fn build_argv(p: &SshPreset) -> Vec<String> {
    let mut argv = Vec::new();
    if let Some(port) = p.port {
        argv.push("-p".into());
        argv.push(port.to_string());
    }
    if let Some(id) = &p.identity {
        if !id.trim().is_empty() {
            argv.push("-i".into());
            argv.push(id.clone());
        }
    }
    for extra in &p.args {
        argv.push(extra.clone());
    }
    let target = match &p.user {
        Some(u) if !u.trim().is_empty() => format!("{}@{}", u, p.host),
        _ => p.host.clone(),
    };
    argv.push(target);
    argv
}

/// Spawn a PTY agent that runs `ssh` with the preset's argv.
pub fn spawn_preset(
    db: &DbPool,
    mgr: &Arc<AgentManager>,
    workspace_id: &str,
    preset_id: &str,
) -> Result<Agent> {
    let preset = get(db, preset_id)?;
    let argv = build_argv(&preset);
    mgr.spawn_with_args(workspace_id, AgentType::Ssh, preset.cwd.clone(), argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use r2d2_sqlite::SqliteConnectionManager;

    fn pool() -> DbPool {
        let manager = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(2).build(manager).unwrap();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE ssh_presets (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                host TEXT NOT NULL,
                user TEXT,
                port INTEGER,
                identity TEXT,
                args_json TEXT NOT NULL DEFAULT '[]',
                cwd TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL);",
        )
        .unwrap();
        pool
    }

    #[test]
    fn create_then_list() {
        let p = pool();
        create(
            &p,
            CreatePresetArgs {
                name: "prod".into(),
                host: "ex.com".into(),
                user: Some("deploy".into()),
                port: Some(22),
                identity: Some("/home/me/.ssh/id_ed25519".into()),
                args: vec!["-L".into(), "8080:localhost:80".into()],
                cwd: None,
            },
        )
        .unwrap();
        let v = list(&p).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "prod");
        assert_eq!(v[0].port, Some(22));
        assert_eq!(v[0].args.len(), 2);
    }

    #[test]
    fn duplicate_name_rejected() {
        let p = pool();
        create(&p, CreatePresetArgs { name: "x".into(), host: "h".into(), ..Default::default() }).unwrap();
        let err = create(&p, CreatePresetArgs { name: "x".into(), host: "h2".into(), ..Default::default() }).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)));
    }

    #[test]
    fn build_argv_with_user_and_port() {
        let preset = SshPreset {
            id: "1".into(), name: "p".into(),
            host: "ex.com".into(), user: Some("deploy".into()),
            port: Some(2222),
            identity: Some("/k".into()),
            args: vec!["-L".into(), "8080:localhost:80".into()],
            cwd: None,
            created_at: "".into(), updated_at: "".into(),
        };
        let argv = build_argv(&preset);
        assert_eq!(
            argv,
            vec![
                "-p".to_string(),
                "2222".to_string(),
                "-i".to_string(),
                "/k".to_string(),
                "-L".to_string(),
                "8080:localhost:80".to_string(),
                "deploy@ex.com".to_string(),
            ]
        );
    }

    #[test]
    fn build_argv_without_user_and_port() {
        let preset = SshPreset {
            id: "1".into(), name: "p".into(),
            host: "ex.com".into(), user: None,
            port: None, identity: None, args: vec![],
            cwd: None, created_at: "".into(), updated_at: "".into(),
        };
        let argv = build_argv(&preset);
        assert_eq!(argv, vec!["ex.com".to_string()]);
    }

    #[test]
    fn empty_name_or_host_rejected() {
        let p = pool();
        assert!(matches!(
            create(&p, CreatePresetArgs { name: "".into(), host: "h".into(), ..Default::default() }).unwrap_err(),
            Error::Invalid(_)
        ));
        assert!(matches!(
            create(&p, CreatePresetArgs { name: "n".into(), host: "".into(), ..Default::default() }).unwrap_err(),
            Error::Invalid(_)
        ));
    }
}
