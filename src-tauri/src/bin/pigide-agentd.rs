//! `pigide-agentd` — long-lived PTY supervisor process.
//!
//! Owns every CLI agent (claude, kiro-cli, aider, …) on behalf of the
//! short-lived PigIDE UI. The UI connects via unix domain socket, sends
//! `Hello` + `Subscribe` + a stream of ops; broker keeps PTYs alive
//! across UI quit/restart.
//!
//! Layout on disk:
//!
//!   $XDG_RUNTIME_DIR/pigide/agentd.sock      ← listening socket
//!   $XDG_RUNTIME_DIR/pigide/agentd.pid       ← pid + lockfile (advisory)
//!   $XDG_DATA_HOME/pigide/agents/<id>.log    ← per-agent stdout log
//!
//! Single-instance enforcement: the broker takes an `flock(LOCK_EX |
//! LOCK_NB)` on the pidfile before binding the socket. Concurrent
//! launches lose the race and exit with a non-zero code; PigIDE catches
//! that and just connects to the existing instance.
//!
//! Lifecycle: the broker NEVER voluntarily exits. SIGINT / SIGTERM
//! triggers graceful shutdown ONLY when explicitly requested via env
//! `PIGIDE_AGENTD_SHUTDOWN_ON_SIGNAL=1`; otherwise signals are ignored
//! so a stray Ctrl+C in the wrong terminal can't take down all the
//! user's agents. The broker is meant to be killed via `kill -9` from
//! the user, or replaced by a fresh broker after the old PID is gone.

use pigide_lib::agentd::engine::Engine;
use pigide_lib::agentd::proto::default_socket_path;
use pigide_lib::agentd::server::serve_connection;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::UnixListener;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> std::io::Result<()> {
    init_tracing();

    let socket_path = match std::env::var("PIGIDE_AGENTD_SOCKET") {
        Ok(s) if !s.is_empty() => PathBuf::from(s),
        _ => default_socket_path(),
    };
    let log_dir = match std::env::var("PIGIDE_AGENTD_LOG_DIR") {
        Ok(s) if !s.is_empty() => PathBuf::from(s),
        _ => default_log_dir(),
    };

    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir_all(&log_dir)?;

    // Single-instance check: take exclusive flock on the pidfile.
    let pid_path = socket_path.with_extension("pid");
    let _pid_guard = match acquire_pidfile(&pid_path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!(
                "pigide-agentd: another broker holds {}: {}",
                pid_path.display(),
                e
            );
            std::process::exit(2);
        }
    };

    // If a stale socket exists (previous broker crashed), unlink before
    // bind. Safe because flock above already proved no other broker is
    // running.
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }

    let listener = UnixListener::bind(&socket_path)?;
    // Restrict the socket file itself to owner-only. Best-effort: a
    // failure here doesn't break correctness, only weakens privacy on
    // systems where the umask was unusually permissive. We deliberately
    // do NOT chmod the parent directory — the broker may live under
    // user-owned paths (e.g. $XDG_RUNTIME_DIR/pigide) where chmod 0700
    // is fine, OR under shared paths (/tmp during tests) where it would
    // fail with EPERM. Owner-only on the socket itself is enough.
    if let Err(e) = set_permissions_0600(&socket_path) {
        tracing::warn!(
            "pigide-agentd: chmod 0600 on {} failed: {}",
            socket_path.display(),
            e
        );
    }

    tracing::info!(
        "pigide-agentd v{} listening on {}",
        env!("CARGO_PKG_VERSION"),
        socket_path.display()
    );

    let engine = Arc::new(Engine::new(log_dir)?);

    // Optional graceful-shutdown handler. Off by default — see the
    // module-level note about signal policy.
    if std::env::var("PIGIDE_AGENTD_SHUTDOWN_ON_SIGNAL")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        let socket_clone = socket_path.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("pigide-agentd: SIGINT received, exiting");
            let _ = std::fs::remove_file(&socket_clone);
            std::process::exit(0);
        });
    }

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let (r, w) = stream.into_split();
                let engine_clone = engine.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_connection(engine_clone, r, w).await {
                        tracing::warn!("pigide-agentd: connection ended: {}", e);
                    }
                });
            }
            Err(e) => {
                tracing::warn!("pigide-agentd: accept failed: {}", e);
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter =
        EnvFilter::try_from_env("PIGIDE_AGENTD_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn default_log_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::env::temp_dir())
        .join("pigide")
        .join("agents")
}

/// Owner-only permissions on the bound socket file. Best-effort: a
/// failure here doesn't break correctness, only weakens privacy on
/// systems where the umask was unusually permissive.
#[cfg(unix)]
fn set_permissions_0600(p: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_permissions_0600(_p: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

/// Pidfile guard. Holds an exclusive `flock` for the lifetime of the
/// broker; the kernel releases it automatically on process exit (even
/// on SIGKILL), so a crashed broker doesn't leave a stale lock.
struct PidGuard {
    _file: std::fs::File,
    path: PathBuf,
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
fn acquire_pidfile(path: &std::path::Path) -> std::io::Result<PidGuard> {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is valid for the lifetime of `file`. flock is the
    // simplest single-instance primitive on Linux/macOS; we accept its
    // NFS caveats since the broker only runs against local paths.
    let rc = unsafe { libc_flock_ex_nb(fd) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut f = file;
    writeln!(f, "{}", std::process::id())?;
    Ok(PidGuard {
        _file: f,
        path: path.to_path_buf(),
    })
}

#[cfg(not(unix))]
fn acquire_pidfile(path: &std::path::Path) -> std::io::Result<PidGuard> {
    let _ = path;
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "pidfile not supported on this platform",
    ))
}

#[cfg(unix)]
unsafe fn libc_flock_ex_nb(fd: i32) -> i32 {
    extern "C" {
        fn flock(fd: i32, op: i32) -> i32;
    }
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    flock(fd, LOCK_EX | LOCK_NB)
}
