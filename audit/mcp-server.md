# MCP Server Audit Report

**Task:** aa2a9743-a589-4998-ab96-4473b5ec55ac  
**Date:** 2026-05-20  
**Auditor:** Backend Auditor agent  
**Scope:** `src-tauri/src/mcp/*`, `src-tauri/src/orchestrator/tools.rs`, `src-tauri/src/swarm/tools.rs`, `src-tauri/src/memory/tools.rs`, `src-tauri/src/tasks.rs`

---

## Files Inspected

| File | Lines | Purpose |
|------|-------|---------|
| `src-tauri/src/mcp/mod.rs` | 15 | Module root |
| `src-tauri/src/mcp/server.rs` | 395 | HTTP/JSON-RPC endpoint, auth, dispatch |
| `src-tauri/src/mcp/auth.rs` | 169 | API key CRUD, SHA-256 hash validation |
| `src-tauri/src/mcp/launcher.rs` | 181 | Auto-registration for Claude tiles |
| `src-tauri/src/orchestrator/tools.rs` | 738 | Tool definitions + dispatch |
| `src-tauri/src/swarm/tools.rs` | 300 | Swarm tool definitions + dispatch |
| `src-tauri/src/memory/tools.rs` | 215 | Memory tool definitions + dispatch |
| `src-tauri/src/swarm/mailbox.rs` | 206 | Mailbox storage |
| `src-tauri/src/swarm/ownership.rs` | 225 | File lock mechanism |
| `src-tauri/src/swarm/review.rs` | 247 | Review gates |
| `src-tauri/src/tasks.rs` | 397 | Task manager |
| `src-tauri/src/sanitize.rs` | 117 | ANSI strip for tail_agent |

---

## Surface Map

| Tool | Defined in | Dispatched in | Scope check | Notes |
|------|-----------|---------------|-------------|-------|
| `list_workspaces` | tools.rs:20 | tools.rs:261 | none (read) | OK |
| `create_workspace` | tools.rs:25 | tools.rs:273 | `mutate` | OK |
| `switch_workspace` | tools.rs:36 | tools.rs:316 | `mutate` | OK |
| `delete_workspace` | tools.rs:43 | tools.rs:328 | `dangerous` | OK |
| `list_agents` | tools.rs:54 | tools.rs:344 | none (read) | OK |
| `spawn_agent` | tools.rs:62 | tools.rs:354 | `dangerous` | OK |
| `close_agent` | tools.rs:76 | tools.rs:394 | `mutate` | OK |
| `send_to_agent` | tools.rs:84 | tools.rs:423 | `dangerous` | OK |
| `wait_for_agent_idle` | tools.rs:98 | tools.rs:454 | **NONE** | **BUG: no scope, holds thread** |
| `tail_agent` | tools.rs:110 | tools.rs:491 | none (read) | OK |
| `get_layout` | tools.rs:122 | tools.rs:513 | none (read) | OK |
| `create_task` | tools.rs:127 | tools.rs:519 | `mutate` | OK |
| `list_tasks` | tools.rs:142 | tools.rs:553 | none (read) | OK |
| `get_task` | tools.rs:154 | tools.rs:566 | none (read) | OK |
| `update_task` | tools.rs:163 | tools.rs:573 | `mutate` | OK |
| `assign_task_to_agent` | tools.rs:178 | tools.rs:605 | `mutate` | OK |
| `delete_task` | **NOT DEFINED** | **NOT DISPATCHED** | `mutate`+`dangerous` | **BUG: dead scope check** |
| `create_memory` | memory/tools.rs:9 | memory/tools.rs:122 | `mutate` | OK |
| `read_memory` | memory/tools.rs:24 | memory/tools.rs:151 | none (read) | OK |
| `update_memory` | memory/tools.rs:33 | memory/tools.rs:155 | `mutate` | OK |
| `delete_memory` | memory/tools.rs:47 | memory/tools.rs:175 | `dangerous` | OK |
| `list_memories` | memory/tools.rs:56 | memory/tools.rs:180 | none (read) | OK |
| `search_memories` | memory/tools.rs:67 | memory/tools.rs:186 | none (read) | OK |
| `find_backlinks` | memory/tools.rs:79 | memory/tools.rs:192 | none (read) | OK |
| `suggest_connections` | memory/tools.rs:87 | memory/tools.rs:196 | none (read) | OK |
| `send_mail` | swarm/tools.rs:10 | swarm/tools.rs:168 | `mutate` | OK |
| `broadcast` | swarm/tools.rs:22 | swarm/tools.rs:175 | `mutate` | OK (but see F-5) |
| `read_mailbox` | swarm/tools.rs:36 | swarm/tools.rs:181 | **NONE** | **BUG: reads any agent's mail** |
| `mark_mail_read` | swarm/tools.rs:48 | swarm/tools.rs:190 | `mutate` | OK |
| `start_rollcall` | swarm/tools.rs:59 | swarm/tools.rs:203 | `mutate` | OK |
| `collect_rollcall` | swarm/tools.rs:69 | swarm/tools.rs:209 | none (read) | OK |
| `claim_files` | swarm/tools.rs:80 | swarm/tools.rs:212 | **NONE** | **BUG: mutates, no scope** |
| `release_files` | swarm/tools.rs:94 | swarm/tools.rs:243 | **NONE** | **BUG: mutates, no scope** |
| `list_file_owners` | swarm/tools.rs:107 | swarm/tools.rs:264 | none (read) | OK |
| `open_review_gate` | swarm/tools.rs:118 | swarm/tools.rs:275 | **NONE** | **BUG: mutates, no scope** |
| `vote_review_gate` | swarm/tools.rs:129 | swarm/tools.rs:281 | **NONE** | **BUG: mutates, no scope** |
| `list_review_gates` | swarm/tools.rs:143 | swarm/tools.rs:288 | none (read) | OK |
| `resolve_project` | tools.rs:193 | tools.rs:629 | none (read) | OK |
| `open_project` | tools.rs:204 | tools.rs:637 | **NONE** | **BUG: mutates (creates ws, sets setting)** |
| `remember_project_alias` | tools.rs:216 | tools.rs:702 | **NONE** | **BUG: writes to filesystem** |
| `rebuild_project_index` | tools.rs:228 | tools.rs:718 | **NONE** | Low risk but mutates in-memory state |
| `watcher_status` | server.rs:286 | server.rs:326 | none (read) | OK (feature-gated) |

---

## Findings

### F-1 [CRITICAL] — Scope bypass: 7 mutating tools lack scope enforcement

**Location:** `src-tauri/src/mcp/server.rs:39-60` (`is_mutating` / `is_dangerous`)

**Problem:** The following tools INSERT/UPDATE/DELETE rows or write to the filesystem but are NOT listed in `is_mutating()`:
- `claim_files`
- `release_files`
- `open_review_gate`
- `vote_review_gate`
- `open_project` (creates workspace, sets `current_workspace_id`)
- `remember_project_alias` (writes `.pigmemory/aliases.json`)
- `rebuild_project_index` (mutates in-memory index)

Any API key with only `read` scope can call these tools and mutate state.

**Impact:** A read-only key (intended for monitoring/dashboards) can hijack file ownership, flip review gates to `pass`, create workspaces, and write arbitrary aliases to disk.

**Fix:**
```diff
 fn is_mutating(name: &str) -> bool {
     matches!(
         name,
         "create_workspace"
             | "switch_workspace"
             | "delete_workspace"
             | "spawn_agent"
             | "close_agent"
             | "send_to_agent"
             | "create_task"
             | "update_task"
             | "delete_task"
             | "assign_task_to_agent"
             | "create_memory"
             | "update_memory"
             | "delete_memory"
             | "send_mail"
             | "broadcast"
             | "mark_mail_read"
             | "start_rollcall"
+            | "claim_files"
+            | "release_files"
+            | "open_review_gate"
+            | "vote_review_gate"
+            | "open_project"
+            | "remember_project_alias"
+            | "rebuild_project_index"
     )
 }
```

---

### F-2 [HIGH] — `wait_for_agent_idle` unbounded server-side busy-loop (DoS)

**Location:** `src-tauri/src/orchestrator/tools.rs:454-489`

**Problem:** `wait_for_agent_idle` accepts `timeout_ms` up to 600,000 (10 minutes) and loops with 200ms sleeps. A single MCP request holds a tokio task and an HTTP connection for up to 10 minutes. An attacker with any valid key can open N concurrent requests to exhaust the thread pool / connection limit.

Additionally, there is no scope check — even a `read`-only key can trigger this.

**Impact:** Denial of service. 50 concurrent calls = 50 tokio tasks blocked for 10 minutes each.

**Fix:**
```diff
+// Add to is_mutating — it's a blocking operation even if it doesn't write.
+// Or better: add a server-wide concurrent-request limit.

 "wait_for_agent_idle" => {
     let agent_id = ...;
-    let timeout_ms = args.get("timeout_ms").and_then(|v| v.as_u64()).unwrap_or(60_000);
+    let timeout_ms = args.get("timeout_ms").and_then(|v| v.as_u64()).unwrap_or(60_000).min(120_000);
     // Also: validate agent_id exists BEFORE entering the loop.
+    if agent_mgr.last_stdout_age(agent_id).is_none() {
+        return Err(Error::NotFound(format!("agent {}", agent_id)));
+    }
```

Also add `tower::limit::ConcurrencyLimitLayer` or equivalent to the router.

---

### F-3 [HIGH] — No request body size limit

**Location:** `src-tauri/src/mcp/server.rs:124-127` (Router construction)

**Problem:** The axum Router has no `DefaultBodyLimit` or `RequestBodyLimit` layer. A client can POST an arbitrarily large JSON body, causing OOM.

**Impact:** Single request with a multi-GB body crashes the process.

**Fix:**
```diff
+use axum::extract::DefaultBodyLimit;
+
 let app = Router::new()
     .route("/mcp", post(handle_rpc))
     .route("/healthz", get(|| async { "ok" }))
+    .layer(DefaultBodyLimit::max(1024 * 1024)) // 1 MB
     .with_state(state);
```

---

### F-4 [HIGH] — `delete_task` scope check is dead code (tool unreachable)

**Location:** `src-tauri/src/mcp/server.rs:50,70` vs `src-tauri/src/orchestrator/tools.rs`

**Problem:** `delete_task` is listed in both `is_mutating` and `is_dangerous`, but:
1. It has no entry in `tool_definitions()` — clients never see it in `tools/list`.
2. It has no match arm in `dispatch()` — calling it returns "unknown tool".

The `TaskManager::delete()` method exists (tasks.rs:267) and correctly releases file locks, but is unreachable via MCP.

**Impact:** Functional gap — tasks cannot be deleted via MCP. The scope check is misleading dead code.

**Fix:** Either add the tool to definitions + dispatch, or remove the dead scope entries.

```diff
+// In tool_definitions():
+function_tool(
+    "delete_task",
+    "Delete a task and release its file locks.",
+    json!({
+        "type": "object",
+        "properties": {"id": {"type": "string"}},
+        "required": ["id"]
+    }),
+),

+// In dispatch():
+"delete_task" => {
+    let id = args.get("id").and_then(|v| v.as_str())
+        .ok_or_else(|| Error::Invalid("id required".into()))?;
+    task_mgr.delete(id)?;
+    Ok(json!({"deleted": id}))
+}
```

---

### F-5 [MEDIUM] — `send_mail` / `broadcast` have no sender identity

**Location:** `src-tauri/src/swarm/tools.rs:168-179`

**Problem:** `send_mail` dispatches with `from_agent_id: None`. The MCP caller's identity (API key label/id) is never threaded into the mail's `from_agent_id`. Any MCP client can send mail that appears to come from nobody — or worse, there's no way to attribute messages.

**Impact:** Spoofing / non-repudiation failure. A malicious key holder can send instructions to builders that appear system-generated.

**Fix:** Thread `key.id` or `key.label` into the dispatch context and use it as `from_agent_id`.

---

### F-6 [MEDIUM] — `read_mailbox` has no access control on `to` address

**Location:** `src-tauri/src/swarm/tools.rs:181-188`

**Problem:** Any authenticated caller can read any agent's mailbox by passing an arbitrary `to` address. There is no check that the caller owns or is associated with the target address.

**Impact:** Information disclosure — any key holder can read all inter-agent communications.

**Fix:** Either scope `read_mailbox` to the caller's own agent_id, or add an explicit `admin` scope requirement for reading others' mail.

---

### F-7 [MEDIUM] — JSON-RPC: empty `jsonrpc` field accepted silently

**Location:** `src-tauri/src/mcp/server.rs:182-183`

**Problem:**
```rust
if req.jsonrpc != "2.0" && !req.jsonrpc.is_empty() {
    return error_response(req.id, -32600, "invalid jsonrpc version");
}
```
When `jsonrpc` is empty (or missing due to `#[serde(default)]`), the request is processed normally. Per JSON-RPC 2.0 spec, the field MUST be exactly `"2.0"`. Accepting empty means non-conformant clients work silently, and the server cannot distinguish JSON-RPC 1.0 from 2.0 requests.

**Impact:** Protocol conformance violation. Low practical risk but breaks spec compliance.

**Fix:**
```diff
-if req.jsonrpc != "2.0" && !req.jsonrpc.is_empty() {
+if req.jsonrpc != "2.0" {
     return error_response(req.id, -32600, "invalid jsonrpc version");
 }
```

---

### F-8 [MEDIUM] — No JSON-RPC batch request support

**Location:** `src-tauri/src/mcp/server.rs:176-258`

**Problem:** The handler deserializes `Json<JsonRpcRequest>` (single object). JSON-RPC 2.0 spec requires servers to accept arrays of requests (batch). Sending `[{...}, {...}]` will return a 422 deserialization error.

**Impact:** Spec non-compliance. Clients that batch requests (some MCP SDKs do) will fail.

**Fix:** Accept `Json<Value>`, check if array or object, dispatch accordingly.

---

### F-9 [MEDIUM] — JSON-RPC notifications (no `id`) processed as requests

**Location:** `src-tauri/src/mcp/server.rs:161,250-258`

**Problem:** When `id` is `None` (a JSON-RPC notification), the server still processes the method and returns a response with `"id": null`. Per spec, notifications MUST NOT receive a response.

**Impact:** Protocol violation. Wasted work on notifications that should be fire-and-forget.

**Fix:** If `req.id.is_none()`, process the method for side effects but return 204 No Content (or empty body).

---

### F-10 [LOW] — `tail_agent` reads file synchronously on async handler

**Location:** `src-tauri/src/orchestrator/tools.rs:503` — `std::fs::read(&log)`

**Problem:** `std::fs::read` is a blocking syscall called inside an async dispatch path. For large log files (up to 65KB read), this blocks the tokio worker thread.

**Impact:** Minor latency spike under load. Not critical since the file is local and small.

**Fix:** Use `tokio::fs::read` or `spawn_blocking`.

---

### F-11 [LOW] — `auth::validate` does a write (UPDATE last_used_at) on every request

**Location:** `src-tauri/src/mcp/auth.rs:126-129`

**Problem:** Every authenticated request triggers an UPDATE to bump `last_used_at`. Under high concurrency this creates write contention on the SQLite WAL.

**Impact:** Performance degradation under load. Not a correctness bug.

**Fix:** Debounce the update (e.g., only update if >60s since last bump), or move to a separate async batch.

---

### F-12 [LOW] — Audit log silently swallows errors

**Location:** `src-tauri/src/mcp/server.rs:374-391`

**Problem:** The `audit()` function wraps the INSERT in `let _ = (|| -> Result<()> { ... })();` — any DB error (pool exhausted, disk full) is silently discarded.

**Impact:** Audit trail can have gaps without any indication. Acceptable for non-critical audit, but should at least `tracing::warn!`.

**Fix:**
```diff
 fn audit(db: &DbPool, key: Option<&KeyInfo>, tool: &str, args: &Value, status: &str) {
-    let _ = (|| -> Result<()> {
+    if let Err(e) = (|| -> Result<()> {
         // ...
-    })();
+    })() {
+        tracing::warn!("mcp audit insert failed: {}", e);
+    }
 }
```

---

## Cross-references

- Agent streaming / PTY lifecycle: see task 3b787801
- Persistence layer (SQLite schema, migrations, pool config): see task 18f88517
- Orchestrator core (LLM dispatch, tool loop): see task 89a15396
- Cross-cutting (error types, state management): see task 89ee4d9d

---

## Recommendations (Priority Order)

1. **Immediately** add all mutating swarm/project tools to `is_mutating()` (F-1).
2. **Add `DefaultBodyLimit`** to the axum Router (F-3).
3. **Cap `wait_for_agent_idle` timeout** and validate agent existence before loop (F-2).
4. **Implement `delete_task`** in tool_definitions + dispatch, or remove dead scope entries (F-4).
5. **Thread caller identity** into `send_mail` / `broadcast` (F-5).
6. **Add access control** to `read_mailbox` (F-6).
7. Consider adding `ConcurrencyLimitLayer` to prevent connection exhaustion.
8. Fix JSON-RPC spec compliance (F-7, F-8, F-9) for interop with standard MCP clients.
