# Audit: Agent Process Management & Streaming

**Auditor:** Backend Auditor  
**Date:** 2026-05-20  
**Task:** 3b787801-f294-440a-8486-6efad266173d  
**Related bug:** task 34b33c78 — "error: orchestrator: stream: error decoding response body"

---

## Scope

| Area | Files |
|------|-------|
| Agent lifecycle (spawn/kill) | `src-tauri/src/agent.rs` |
| PTY reader thread | `agent.rs:612–670` |
| send_to_agent / write | `agent.rs:194–234`, `agent.rs:738–778` |
| wait_for_agent_idle | `orchestrator/tools.rs:454–489` |
| tail_agent | `orchestrator/tools.rs:491–511` |
| LLM stream decode (OmniRouter) | `orchestrator/client.rs:156–253` |
| LLM stream decode (Anthropic) | `orchestrator/providers/anthropic.rs:134–179` |
| Watcher stdout decode | `watcher/supervisor.rs:210–214` |
| Sanitize | `src-tauri/src/sanitize.rs` |

---

## Lifecycle Diagram

```
spawn_internal()
  │
  ├─ openpty() → (master, slave)
  ├─ slave.spawn_command(cmd) → child
  ├─ drop(slave)
  ├─ master.take_writer() → Arc<Mutex<Writer>>
  ├─ master.try_clone_reader() → reader
  ├─ std::thread::spawn(reader_loop)  ← OS thread, not tokio
  │     │
  │     ├─ loop { reader.read(&mut [0u8; 4096]) }
  │     ├─ emit EV_AGENT_STDOUT (base64-encoded raw bytes)
  │     ├─ append to log file
  │     ├─ update last_stdout timestamp
  │     └─ on EOF: remove handle, update DB, emit EV_AGENT_EXIT
  │
  ├─ INSERT/UPSERT agents row
  └─ store AgentRuntime in handles HashMap

write(agent_id, data)
  │
  ├─ snapshot (writer, child, readiness, spawned_at) from handles
  ├─ write_runtime():
  │     ├─ is_dead() check
  │     ├─ readiness condvar wait (bounded by spawn-relative grace)
  │     ├─ is_dead() re-check
  │     └─ writer.lock() → write_all + flush
  └─ return bytes written

kill(agent_id)
  │
  ├─ handles.remove(agent_id)
  ├─ child.kill() + child.wait()
  └─ UPDATE agents SET status='exited'
```

---

## Findings

### FINDING-1 [CRITICAL] — `from_utf8_lossy` on raw byte-stream chunks causes data corruption

**Location:**
- `orchestrator/client.rs:168` — `buf.push_str(&String::from_utf8_lossy(&bytes));`
- `orchestrator/providers/anthropic.rs:148` — `buf.push_str(&String::from_utf8_lossy(&bytes));`

**Problem:**  
`resp.bytes_stream()` yields `Bytes` chunks at arbitrary TCP segment boundaries. A multi-byte UTF-8 sequence (e.g. `é` = `0xC3 0xA9`) can be split across two chunks. `String::from_utf8_lossy` replaces the trailing incomplete byte(s) with `U+FFFD` (replacement character), and the leading continuation byte(s) in the next chunk are also replaced. This corrupts the SSE text.

When the corrupted bytes land inside a JSON `data:` line, `serde_json::from_str` fails → the event is skipped (OmniRouter) or the parser misses a delta (Anthropic). If corruption hits the `\n\n` delimiter itself, the SSE framing breaks and the stream stalls until the next valid double-newline, potentially losing all remaining events.

**This is the RCA for task 34b33c78.** The error "error decoding response body" is the `reqwest` chunk error surfaced when the connection is dropped mid-stream (timeout or server-side), but the *silent* data loss from `lossy` decode is the more insidious variant — it produces garbled tool-call JSON that fails downstream parsing.

**Reproduction:**  
Any SSE response containing non-ASCII (model outputs in Russian, Chinese, emoji, or even `—` em-dash) where a TCP segment boundary falls mid-codepoint. More likely on slow/congested connections or when the response is large.

**Fix:**
```rust
// Replace:
buf.push_str(&String::from_utf8_lossy(&bytes));

// With a raw-bytes buffer that only decodes complete UTF-8:
let mut raw_buf = Vec::<u8>::new();  // lives outside the loop
// Inside the loop:
raw_buf.extend_from_slice(&bytes);
// Find the last valid UTF-8 boundary:
let valid_up_to = match std::str::from_utf8(&raw_buf) {
    Ok(_) => raw_buf.len(),
    Err(e) => e.valid_up_to(),
};
if valid_up_to > 0 {
    // SAFETY: we just validated this prefix
    let s = unsafe { std::str::from_utf8_unchecked(&raw_buf[..valid_up_to]) };
    buf.push_str(s);
    raw_buf.drain(..valid_up_to);
}
// After the loop: if raw_buf is non-empty, it's a truncated codepoint (connection dropped mid-char) — log and discard.
```

---

### FINDING-2 [MEDIUM] — `tail_agent` byte-slice can split UTF-8 codepoint

**Location:** `orchestrator/tools.rs:504–505`

```rust
let start = bytes.len().saturating_sub(n);
let raw = String::from_utf8_lossy(&bytes[start..]);
```

**Problem:**  
`start` is computed as a byte offset. If it lands in the middle of a multi-byte UTF-8 sequence, `from_utf8_lossy` replaces the leading bytes with `U+FFFD`. The orchestrator LLM then sees a `�` at the start of the tail, which can confuse tool-call parsing.

**Fix:**
```rust
let mut start = bytes.len().saturating_sub(n);
// Advance past any continuation bytes (0x80..0xBF) to a char boundary.
while start < bytes.len() && (bytes[start] & 0xC0) == 0x80 {
    start += 1;
}
let raw = std::str::from_utf8(&bytes[start..]).unwrap_or("");
```

---

### FINDING-3 [MEDIUM] — `wait_for_agent_idle` has a false-positive race on first call

**Location:** `orchestrator/tools.rs:470–489`

**Problem:**  
If `wait_for_agent_idle` is called *before* the agent has ever produced stdout (e.g. immediately after `send_to_agent` to a slow agent), `last_stdout_age()` returns `None`. The loop then sleeps 200ms and checks again. But if the agent produces its first byte during that 200ms window, `last_stdout_age()` returns `Some(~0ms)` which is `< quiet_ms` — correct. However, if the agent produces output *just before* the check and then goes silent, the age could already exceed `quiet_ms` on the very first successful check, returning "idle" before the agent has actually finished processing.

This is a **semantic** issue: the quiet period should be measured from the *last* output, but if the agent was never tracked before, the first timestamp insertion immediately satisfies the quiet threshold if the poll lands late.

**Mitigation:** Record a "send timestamp" in `last_stdout` when `write()` succeeds, so the quiet timer resets on input, not just output:
```rust
// In AgentManager::write(), after successful write_runtime():
self.last_stdout.lock().insert(agent_id.to_string(), Instant::now());
```

---

### FINDING-4 [LOW] — Reader thread is an OS thread, not a tokio task

**Location:** `agent.rs:612` — `std::thread::spawn(move || { ... })`

**Problem:**  
Each agent spawns a dedicated OS thread for the PTY reader loop. With 32 agents (the max), that's 32 OS threads permanently blocked on `read()`. This is acceptable for the current scale but doesn't compose well with tokio's cooperative scheduling. The thread also holds `Arc<AgentManager>` which prevents the manager from being dropped until all reader threads exit.

**Risk:** On app shutdown, if a PTY doesn't produce EOF promptly after `kill()`, the reader thread hangs indefinitely. `kill()` calls `child.kill()` + `child.wait()` but does NOT close the master PTY fd — the reader thread is still blocked on `read()` from the master's cloned reader. The handle is removed from the map, but the thread leaks until the process exits.

**Fix:** After `child.wait()` in `kill()`, drop the `master` (which closes the PTY fd and unblocks the reader):
```rust
pub fn kill(&self, agent_id: &str) -> Result<()> {
    let mut handles = self.handles.lock();
    if let Some(h) = handles.remove(agent_id) {
        let mut child = h.child.lock();
        let _ = child.kill();
        let _ = child.wait();
        drop(child);
        drop(h.master); // <-- unblocks reader thread
    }
    // ...
}
```

---

### FINDING-5 [LOW] — Watcher uses `from_utf8_lossy` on agent stdout chunks

**Location:** `watcher/supervisor.rs:214`

```rust
let chunk = String::from_utf8_lossy(&bytes).to_string();
```

**Problem:**  
Same class as FINDING-1 but lower severity: the watcher classifies chunks for decision-request detection. A corrupted `U+FFFD` in the middle of a prompt won't cause a crash but may cause the classifier to miss a decision-request pattern. Since the watcher is best-effort (drops on rate-limit), this is acceptable but worth noting.

---

## Cross-boundary notes

| Boundary | Ref task | Note |
|----------|----------|------|
| LLM turn-loop / provider selection | 89a15396 | The stream decode bug (FINDING-1) lives in the provider layer. The turn-loop in `mod.rs` correctly handles cancel/drop semantics. |
| MCP server | aa2a9743 | `mcp/server.rs` gates dangerous tools but doesn't touch streaming. |
| Persistence | 18f88517 | Agent DB rows are correctly managed; no persistence bugs found. |

---

## Summary for task 34b33c78

**RCA:** `String::from_utf8_lossy` applied to raw TCP chunks in both `client.rs:168` and `anthropic.rs:148`. When a multi-byte UTF-8 codepoint spans two TCP segments, the lossy decode inserts `U+FFFD` replacement characters, corrupting SSE event data. This causes:
1. Silent JSON parse failures (events skipped)
2. Garbled tool-call arguments (downstream "error decoding")
3. In worst case, broken `\n\n` framing → stream hangs

**Localized to:** `orchestrator/client.rs:168` and `orchestrator/providers/anthropic.rs:148` (2 lines, same pattern).

**Patch:** Replace `from_utf8_lossy` with a raw-byte accumulator that only decodes validated UTF-8 prefixes (see FINDING-1 fix above).

---

## Recommendations (priority order)

1. **[P0]** Fix FINDING-1 in both files — this is the production bug.
2. **[P1]** Fix FINDING-2 — `tail_agent` byte-boundary alignment.
3. **[P1]** Fix FINDING-3 — reset `last_stdout` on write to prevent false idle.
4. **[P2]** Fix FINDING-4 — drop master PTY in `kill()` to unblock reader thread.
5. **[P3]** Consider FINDING-5 — watcher lossy decode (low priority, best-effort system).
