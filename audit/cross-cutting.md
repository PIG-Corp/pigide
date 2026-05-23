# Cross-Cutting Audit — PigIDE Backend

**Auditor:** Backend Auditor (cross-cutting)  
**Date:** 2026-05-20  
**Scope:** Security, Concurrency, Error Handling, Build/Test Hygiene, Dependency Hygiene  
**Task:** 89ee4d9d-f736-483b-bdc2-85337b1469ab

---

## 1. SECURITY

### Finding S1 — PATH TRAVERSAL in `memory/storage.rs:slug_to_path` [CRITICAL]

**File:** `src-tauri/src/memory/storage.rs:34`

`slug_to_path` splits the slug on `/` and pushes each segment into the path — but never rejects `..` segments. An orchestrator tool call like `create_memory { slug: "../../etc/cron.d/backdoor" }` would write a file outside `.pigmemory/`.

```rust
// BEFORE (vulnerable)
pub fn slug_to_path(root: &std::path::Path, slug: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for seg in slug.split('/') {
        p.push(seg);
    }
    p.set_extension("md");
    p
}
```

**Fix:**
```rust
pub fn slug_to_path(root: &std::path::Path, slug: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for seg in slug.split('/') {
        if seg == ".." || seg == "." || seg.is_empty() {
            continue;
        }
        p.push(seg);
    }
    p.set_extension("md");
    p
}
```

**Impact:** Arbitrary file write on the host filesystem via orchestrator tool calls.

---

### Finding S2 — NO PATH VALIDATION in `files.rs:read_file` / `write_file` [HIGH]

**File:** `src-tauri/src/files.rs:50-68`

`read_file` and `write_file` accept arbitrary absolute paths with zero boundary checks. Any MCP tool call or orchestrator dispatch that routes through these functions can read/write any file the process user can access (e.g., `~/.ssh/id_rsa`, `/etc/shadow`).

The module comment mentions "workspace's `paths[0]`" but no enforcement exists.

**Recommendation:** Add a guard that canonicalizes the requested path and asserts it starts with one of the workspace's allowed roots:
```rust
fn validate_path(requested: &str, allowed_roots: &[&Path]) -> Result<PathBuf> {
    let canon = std::fs::canonicalize(requested)?;
    if !allowed_roots.iter().any(|r| canon.starts_with(r)) {
        return Err(Error::Invalid(format!("path {} outside workspace", requested)));
    }
    Ok(canon)
}
```

---

### Finding S3 — MCP TOKEN IN QUERY STRING (URL leak) [MEDIUM]

**File:** `src-tauri/src/mcp/launcher.rs:71`

```rust
format!("http://{}:{}/mcp?apiKey={}", host, addr.port(), token)
```

The bearer token is embedded in the URL query string. This means:
- It appears in server access logs (axum tracing layer if enabled)
- It may appear in browser history / referer headers if the URL is ever opened in a browser
- It's visible in process listings (`/proc/*/cmdline`) since it's passed as `--mcp-config <json>`

The `Authorization: Bearer` header is also set (line 79), making the query param redundant.

**Recommendation:** Remove `?apiKey=` from the URL; rely solely on the `Authorization` header already present in the config block.

---

### Finding S4 — UNIX SOCKET PERMISSIONS NOT RESTRICTED [LOW]

**File:** `src-tauri/src/ipc.rs:130`

The Unix domain socket is created with default umask permissions. Any local user on a shared machine can connect and issue `OpenPath` commands to manipulate workspaces.

**Recommendation:** After `UnixListener::bind`, set socket permissions to `0o600`:
```rust
use std::os::unix::fs::PermissionsExt;
std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
```

---

### Finding S5 — `claim_files` ACCEPTS ARBITRARY PATHS (no workspace scoping) [MEDIUM]

**File:** `src-tauri/src/commands.rs:1084`

The `path` field in `claim_files` is stored as-is in the DB. There's no validation that the path is relative or within the workspace. An attacker (or misbehaving agent) could claim `/etc/passwd` and block other tasks from "releasing" it, or use it as a confusion vector.

**Recommendation:** Normalize paths to workspace-relative form and reject absolute paths or `..` traversals.

---

## 2. CONCURRENCY

### Finding C1 — NESTED LOCK RISK in `agent.rs` reader thread [LOW]

**File:** `src-tauri/src/agent.rs:624-657`

The reader thread acquires `last_stdout` lock (line 624), then later acquires `handles` lock (line 657). The main `spawn_internal` acquires `handles` lock (line 687) without holding `last_stdout`. No deadlock today because the reader thread drops `last_stdout` before acquiring `handles`, but the ordering is implicit and fragile.

**Recommendation:** Document lock ordering invariant: `handles` → `last_stdout` (never reverse). Consider extracting the handle-removal into a method that asserts ordering.

---

### Finding C2 — `parking_lot::Mutex` in `AgentManager` blocks OS threads [INFO]

**File:** `src-tauri/src/agent.rs:169`

`AgentManager::handles` uses `parking_lot::Mutex` (blocking). Since `write()` is called from `tokio::spawn` contexts (via orchestrator tool dispatch), a contended lock could block a tokio worker thread. Currently mitigated by per-agent writer locks reducing contention on the global map, but worth noting.

**Recommendation:** For the global `handles` map, consider `tokio::sync::Mutex` or keep critical sections minimal (current approach is acceptable for now).

---

### Finding C3 — `CancelHandle::wait()` LOST WAKEUP edge case [LOW]

**File:** `src-tauri/src/orchestrator/mod.rs:60-64`

```rust
async fn wait(&self) {
    if self.is_cancelled() { return; }
    self.notify.notified().await;
}
```

There's a TOCTOU gap: if `cancel()` fires between the `is_cancelled()` check and `notified().await`, the notification is lost. `Notify::notified()` only captures notifications that arrive *after* the future is created.

**Fix:**
```rust
async fn wait(&self) {
    let future = self.notify.notified();
    if self.is_cancelled() { return; }
    future.await;
}
```

---

## 3. ERROR HANDLING

### Finding E1 — `.expect()` in production paths (panics in async context) [MEDIUM]

**Files:**
- `src-tauri/src/orchestrator/client.rs:44` — `reqwest::Client::builder().build().expect("reqwest client")`
- `src-tauri/src/orchestrator/providers/anthropic.rs:67` — same pattern

If TLS backend fails to initialize (rare but possible on misconfigured systems), this panics inside a `tokio::spawn` task, aborting the task silently.

**Recommendation:** Replace with `?` propagation or graceful error return.

---

### Finding E2 — SWALLOWED ERRORS in `rooms.rs:126` [LOW]

**File:** `src-tauri/src/rooms.rs:126`

```rust
let _ = conn.execute(...);
```

If the DB insert for agent-room association fails, the error is silently discarded. The room appears to spawn successfully but the association is lost.

**Recommendation:** At minimum log the error: `if let Err(e) = conn.execute(...) { tracing::warn!(...) }`

---

### Finding E3 — `parse_sse_payload` DEAD CODE [LOW]

**File:** `src-tauri/src/orchestrator/client.rs:294` (clippy: `function is never used`)

Dead code that compiles but is never called. If it was meant to be the SSE parser, the actual parser may be duplicated elsewhere.

**Recommendation:** Remove or wire up.

---

## 4. BUILD / TEST HYGIENE

### Finding B1 — 34 CLIPPY ERRORS (with `-D warnings`) [HIGH]

Clippy run (`--no-default-features --features custom-protocol`) produces 34 errors. Key categories:

| Category | Count | Example |
|----------|-------|---------|
| clamp-like pattern | 6 | Manual min/max instead of `.clamp()` |
| dead code | 1 | `parse_sse_payload` |
| derivable impl | 2 | Manual `Default`/`Clone` |
| identical if-blocks | 1 | Copy-paste logic |
| too many arguments | 1 | 9/7 limit |
| `&PathBuf` instead of `&Path` | 1 | Unnecessary allocation |
| loop that never loops | 1 | Likely logic bug |

**Recommendation:** Add `cargo clippy -- -D warnings` to CI gate. Fix the "loop that never loops" immediately — it's likely a logic bug.

---

### Finding B2 — NO CI WORKFLOW FOUND [MEDIUM]

No `.github/workflows/` directory or equivalent CI config detected. The project has no automated gate for:
- Clippy
- Tests
- Format check
- Dependency audit

**Recommendation:** Add a minimal CI workflow with `cargo clippy`, `cargo test`, `cargo fmt --check`.

---

### Finding B3 — CRITICAL PATHS WITHOUT TESTS [MEDIUM]

The following critical paths have no unit/integration tests:
- `files.rs` — `read_file`, `write_file` (no path validation tests)
- `orchestrator/tools.rs` — `dispatch()` function (tool routing)
- `mcp/server.rs` — `handle_rpc` (auth + dispatch integration)
- `agent.rs` — `spawn_internal` (PTY spawn, only `write_runtime` is tested)

See task 89a15396 (orchestrator-core) for orchestrator-specific test gaps.

---

### Finding B4 — DEFAULT FEATURE INCLUDES `gpu-cuda` [LOW]

**File:** `src-tauri/Cargo.toml:70`

```toml
default = ["custom-protocol", "gpu-cuda", "watcher"]
```

`gpu-cuda` as default means `cargo build` fails on any machine without CUDA toolkit. This broke clippy in this audit. Most developers and CI runners don't have CUDA.

**Recommendation:** Remove `gpu-cuda` from default features; document GPU selection in README.

---

## 5. DEPENDENCY HYGIENE

### Finding D1 — `cargo audit` NOT INSTALLED [INFO]

`cargo audit` is not available on this system. Cannot verify known CVEs.

**Recommendation:** Install `cargo-audit` and add to CI: `cargo install cargo-audit && cargo audit`.

---

### Finding D2 — DUPLICATE `bitflags` (v1 + v2) [LOW]

`cargo tree -d` shows `bitflags v1.3.2` (via `inotify`, `portable-pty`, `webkit2gtk`) alongside `bitflags v2.11.1`. This is normal for the Tauri ecosystem and not actionable, but increases binary size.

---

### Finding D3 — BROAD VERSION SPECIFIERS [MEDIUM]

Several dependencies use major-only versions:

```toml
tauri = "2"
serde = "1"
tokio = "1"
reqwest = "0.12"
```

While Cargo.lock pins exact versions, the Cargo.toml allows any semver-compatible update. For a desktop app shipping binaries, pinning to minor versions (e.g., `"2.11"`) reduces surprise breakage on `cargo update`.

**Recommendation:** Pin to `major.minor` for critical deps (tauri, reqwest, tokio).

---

### Finding D4 — `once_cell` IS REDUNDANT (Rust 1.80+ has `std::sync::LazyLock`) [LOW]

**File:** `src-tauri/Cargo.toml:48`, used in `sanitize.rs:1`

Since Rust 1.80, `std::sync::LazyLock` replaces `once_cell::sync::Lazy`. One fewer dependency.

**Recommendation:** Migrate to `std::sync::LazyLock` and drop `once_cell`.

---

## SUMMARY — TOP 7 FINDINGS (by severity)

| # | Severity | Finding | File |
|---|----------|---------|------|
| 1 | CRITICAL | Path traversal in `slug_to_path` | `memory/storage.rs:34` |
| 2 | HIGH | No path validation in `files.rs` | `files.rs:50-68` |
| 3 | HIGH | 34 clippy errors, no CI gate | project-wide |
| 4 | MEDIUM | MCP token in URL query string | `mcp/launcher.rs:71` |
| 5 | MEDIUM | `claim_files` accepts arbitrary paths | `commands.rs:1084` |
| 6 | MEDIUM | Lost wakeup in `CancelHandle::wait()` | `orchestrator/mod.rs:60` |
| 7 | MEDIUM | `.expect()` panics in async spawns | `orchestrator/client.rs:44` |

---

## APPENDIX: Clippy Output (trimmed)

```
error: function `parse_sse_payload` is never used
error: manual `!RangeInclusive::contains` implementation
error: this `impl` can be derived (×2)
error: clamp-like pattern without using clamp function (×6)
error: this function has too many arguments (9/7)
error: writing `&PathBuf` instead of `&Path`
error: this `if` has identical blocks
error: this loop never actually loops
error: could not compile `pigide` (lib) due to 34 previous errors
```

Full build with default features fails due to missing `hipcc` (whisper-rs GPU backend). See B4.

---

## APPENDIX: `cargo tree -d` (duplicates)

Key duplicates: `bitflags` v1/v2 (ecosystem split, not actionable).  
No suspicious typosquats detected. All crate names match well-known packages.
