//! `pigide-cli` — small companion binary that hands a workspace path to a
//! running PigIDE instance over the local IPC socket. Behaves as
//!     pigide-cli .                  → open current dir
//!     pigide-cli /path/to/project   → open the given dir
//!     pigide-cli --ping             → liveness check
//! Exits non-zero with a one-line stderr message on failure so it's safe to
//! chain in shell scripts.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::ExitCode;

#[cfg(unix)]
use std::os::unix::net::UnixStream;

use serde_json::json;

fn socket_path() -> PathBuf {
    if let Ok(rt) = std::env::var("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(rt);
        if p.is_dir() {
            return p.join("pigide.sock");
        }
    }
    let uid = std::env::var("UID").unwrap_or_else(|_| "default".into());
    PathBuf::from(format!("/tmp/pigide-{}.sock", uid))
}

#[cfg(unix)]
fn send(req: serde_json::Value) -> Result<serde_json::Value, String> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path)
        .map_err(|e| format!("connect {}: {}", path.display(), e))?;
    let mut payload = req.to_string();
    payload.push('\n');
    stream
        .write_all(payload.as_bytes())
        .map_err(|e| format!("write: {}", e))?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("read: {}", e))?;
    serde_json::from_str(line.trim()).map_err(|e| format!("parse: {}", e))
}

#[cfg(not(unix))]
fn send(_req: serde_json::Value) -> Result<serde_json::Value, String> {
    Err("Unix socket IPC is not supported on this platform".into())
}

fn print_help() {
    eprintln!(
        "pigide-cli {}\n\
         Hand a workspace path to a running PigIDE instance.\n\n\
         Usage:\n  \
            pigide-cli <path>          Open or focus the workspace at <path>\n  \
            pigide-cli .               Same, current dir\n  \
            pigide-cli --ping          Liveness check\n  \
            pigide-cli -h | --help     Show this help",
        env!("CARGO_PKG_VERSION")
    );
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print_help();
        return ExitCode::from(2);
    }
    if args[0] == "-h" || args[0] == "--help" {
        print_help();
        return ExitCode::SUCCESS;
    }
    if args[0] == "--ping" {
        match send(json!({ "kind": "ping" })) {
            Ok(v) => {
                println!("{}", v);
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("pigide-cli: {}", e);
                ExitCode::from(1)
            }
        }
    } else {
        let raw = &args[0];
        let path = match std::fs::canonicalize(raw) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("pigide-cli: {}: {}", raw, e);
                return ExitCode::from(1);
            }
        };
        match send(json!({ "kind": "open_path", "path": path.to_string_lossy() })) {
            Ok(v) => {
                println!("{}", v);
                if v.get("kind").and_then(|s| s.as_str()) == Some("error") {
                    return ExitCode::from(1);
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("pigide-cli: {}", e);
                ExitCode::from(1)
            }
        }
    }
}
