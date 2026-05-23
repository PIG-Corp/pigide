# Audit: Orchestrator Core — Turn-Loop, Prompt Pipeline & LLM Providers

**Auditor:** Backend Auditor  
**Task:** 89a15396-8fb2-4237-9d1b-19c3bea88664  
**Date:** 2026-05-20  
**Scope:** Turn-loop FSM, system prompt assembly, phantom-tool-call detector, tool_call parser, tool dispatch/routing, LLM providers (Anthropic + OmniRouter), streaming, retries, timeouts, cancellation, token/context budgeting.

---

## Files Reviewed

| File | LOC | Role |
|------|-----|------|
| `src-tauri/src/orchestrator/mod.rs` | 828 | Turn-loop FSM, cancellation, prompt assembly, skill injection |
| `src-tauri/src/orchestrator/client.rs` | 447 | OmniClient — HTTP + SSE stream parser (OpenAI-compat) |
| `src-tauri/src/orchestrator/phantom.rs` | 326 | Phantom-tool-call detector (regex, retry nag, JSONL logging) |
| `src-tauri/src/orchestrator/prompt.rs` | 263 | Static system prompt (`SYSTEM_PROMPT_BASE`) |
| `src-tauri/src/orchestrator/tools.rs` | 738 | Tool definitions + dispatch router |
| `src-tauri/src/orchestrator/providers/mod.rs` | 155 | `LlmProvider` trait, settings keys, `build_provider` |
| `src-tauri/src/orchestrator/providers/anthropic.rs` | 848 | Anthropic Messages API: translate, stream, retry/fallback |
| `src-tauri/src/orchestrator/providers/omni.rs` | 98 | OmniRouter adapter (wraps OmniClient) |

---

## Turn-Loop FSM Diagram

```mermaid
stateDiagram-v2
    [*] --> UserMessage: run_chat_with_attachments()
    UserMessage --> InstallCancel: fresh CancelHandle
    InstallCancel --> PersistUser: chat::insert(user_msg)
    PersistUser --> EmitThinking: emit("thinking")
    EmitThinking --> ToolLoop: tool_loop()

    state ToolLoop {
        [*] --> CheckCancel
        CheckCancel --> BuildMessages: !cancelled
        CheckCancel --> ReturnOk: cancelled
        BuildMessages --> InsertPlaceholder
        InsertPlaceholder --> StreamLLM
        StreamLLM --> CancelledMidStream: cancel.wait() wins select!
        StreamLLM --> StreamError: provider error
        StreamLLM --> Assembled: stream completes
        CancelledMidStream --> DeleteAfter
        DeleteAfter --> ReturnOk
        StreamError --> DeleteAfterErr
        DeleteAfterErr --> ReturnErr
        Assembled --> CheckCancelPost
        CheckCancelPost --> DeleteAfterCancel: cancelled
        DeleteAfterCancel --> ReturnOk
        CheckCancelPost --> PatchPlaceholder: !cancelled
        PatchPlaceholder --> PhantomCheck
        PhantomCheck --> PhantomRetry: is_phantom && attempts < cap
        PhantomCheck --> PhantomExhausted: is_phantom && attempts >= cap
        PhantomCheck --> HasTools: !phantom
        PhantomRetry --> CheckCancel: phantom_nag=true, continue
        PhantomExhausted --> EmitWarning
        EmitWarning --> ReturnOk
        HasTools --> ReturnOk: no tool_calls (model done)
        HasTools --> DispatchTools: has tool_calls
        DispatchTools --> PerToolCancel
        PerToolCancel --> NextTool: !cancelled
        PerToolCancel --> ReturnOk: cancelled
        NextTool --> PersistToolResult
        PersistToolResult --> PerToolCancel: more calls
        PersistToolResult --> EmitThinkingNext: all done
        EmitThinkingNext --> CheckCancel: next iteration
    }

    ToolLoop --> ErrorHandler: Err(e)
    ToolLoop --> CancelMessage: cancel.is_cancelled()
    ToolLoop --> EmitIdle: Ok
    ErrorHandler --> EmitIdle: rollback + system error msg
    CancelMessage --> EmitIdle: "(остановлено пользователем)"
    EmitIdle --> [*]
```

**Key parameters:**
- `DEFAULT_MAX_ITERATIONS = 20` (configurable via `orchestrator.max_iterations`, clamped 1..100)
- `HISTORY_LIMIT = 60` messages loaded per iteration
- `max_tokens = 8192` per LLM request
- `DEFAULT_MAX_PHANTOM_RETRIES = 2` (configurable, clamped to max 5)

---

## Findings (ranked by severity)

### CRITICAL

#### C1. No token/context budgeting — unbounded prompt growth
**Path:** `mod.rs:284-363` (`build_messages`)  
**Issue:** Each iteration rebuilds the full message array from the last 60 DB rows. As tool results accumulate (up to 20 iterations × N tool_calls each), the prompt can exceed the model's context window. There is no token counting, no compaction, no truncation of the message array — only individual tool results are truncated to 4000 chars (`truncate_for_model`).  
**Impact:** On iteration 15+ with verbose tool results, the request will exceed context limits (200k for Claude, 128k for GPT-4), causing a hard API error that terminates the turn.  
**Recommendation:** Implement a token estimator (chars/4 heuristic or tiktoken). Before sending, if estimated tokens > 80% of model context, compact older tool-result messages to summaries or drop the oldest assistant+tool pairs.

#### C2. `wait_for_agent_idle` blocks the tool-loop without cancel check
**Path:** `tools.rs:467-489`  
**Issue:** The `wait_for_agent_idle` dispatch runs a `loop` with `tokio::time::sleep(200ms)` polling. This loop does NOT check `cancel.is_cancelled()` — the `CancelHandle` is not passed to `dispatch()`. If the user hits Stop while waiting (timeout up to 600s), the orchestrator hangs until the agent goes idle or the full timeout elapses.  
**Impact:** Stop button appears broken during long agent waits. User must wait up to 10 minutes.  
**Fix:**
```rust
// tools.rs:dispatch should accept &CancelHandle, and wait_for_agent_idle should:
if cancel.is_cancelled() {
    return Ok(json!({"agent_id": agent_id, "status": "cancelled"}));
}
```

### HIGH

#### H1. OmniRouter provider has NO retry logic
**Path:** `providers/omni.rs:40-61`, `client.rs:48-99`  
**Issue:** Unlike the Anthropic provider (3 retries + jittered backoff + fallback model), the OmniRouter provider (`OmniProvider::chat_stream`) delegates directly to `OmniClient::chat_completions_stream` with zero retries. A single transient 5xx or network blip kills the entire turn.  
**Impact:** Since `build_provider` is hardcoded to OmniRouter (line `providers/mod.rs:113`), ALL production traffic has no retry resilience.  
**Recommendation:** Add retry wrapper in `OmniProvider::chat_stream` mirroring the Anthropic pattern (2-3 attempts, jittered backoff, retryable-error classification).

#### H2. Tool dispatch error propagation is asymmetric
**Path:** `mod.rs:663-681`  
**Issue:** When `tools::dispatch` returns `Err(e)`, it's caught and converted to `json!({"error": e.to_string()})` — the loop continues. But the `serde_json::from_str` for arguments (line 663) uses `unwrap_or_else` to default to `{}` on malformed JSON. This means a model that emits `arguments: "not json"` silently gets `{}` passed to the tool, which may succeed with wrong semantics (e.g., `list_tasks` with no filters returns everything).  
**Impact:** Silent wrong behavior on malformed tool_call arguments.  
**Fix:** When argument parsing fails, persist an error tool_result (`"error: malformed arguments"`) instead of defaulting to `{}`.

#### H3. Phantom detector false-positive risk on legitimate text
**Path:** `phantom.rs:21-47`  
**Issue:** Pattern 5 (`r"\bi\s+called\s+\w+"`) matches any sentence like "I called the meeting" or "I called John". Pattern 1 matches "I'll use this approach" (verb `use`). These can fire on legitimate final-text responses where the model is done and has no tools to call.  
**Impact:** Unnecessary phantom retries (up to 2) on benign text, wasting latency and tokens.  
**Recommendation:** Tighten pattern 5 to require tool-like words after `called` (e.g., `\bi\s+called\s+(search_|spawn_|list_|create_|send_)`). Tighten pattern 1's `use` to require a tool-like object.

#### H4. Parallel tool_calls executed sequentially — no parallelism
**Path:** `mod.rs:659-686`  
**Issue:** The model can emit multiple `tool_calls` in one response (parallel intent). The orchestrator iterates them with `for call in assembled.tool_calls` — strictly sequential. Each `wait_for_agent_idle` (up to 600s) blocks the next tool.  
**Impact:** Multi-agent dispatches that could run in parallel (e.g., 4 `send_to_agent` calls) are serialized, multiplying latency.  
**Recommendation:** Classify tools as side-effect-free vs. mutating. For independent calls (multiple `send_to_agent` to different agents), use `tokio::join!` or `JoinSet`.

### MEDIUM

#### M1. `build_provider` ignores all settings — hardcoded to OmniRouter
**Path:** `providers/mod.rs:112-119`  
**Issue:** Despite defining `KEY_PROVIDER`, `KEY_ARCH_MODEL`, `KEY_ANTHROPIC_API_KEY` etc., `build_provider` ignores the `_db` parameter entirely and always returns `OmniProvider` with `DEFAULT_OMNI_MODEL`. The Anthropic provider is dead code in production.  
**Impact:** Users cannot switch providers via settings. The entire Anthropic retry/fallback/caching infrastructure is unused.

#### M2. History rebuild on every iteration is O(n) DB reads
**Path:** `mod.rs:466-489` (tool_loop top), `mod.rs:288-290` (build_messages)  
**Issue:** Each iteration calls `chat::list(db, session_id, 60)` — a full DB query. Over 20 iterations this is 20 round-trips. The messages are also re-serialized to JSON each time.  
**Impact:** Latency overhead ~5-20ms per iteration (acceptable now, but scales poorly with history size).

#### M3. Cancellation race between stream completion and tool dispatch
**Path:** `mod.rs:544-549`  
**Issue:** After `stream_fut` completes but before tool dispatch, there's a cancel check. However, between `PatchPlaceholder` (line 559-569) and the cancel check (line 546), the placeholder is already persisted. If cancel fires in this window, the DB has a patched assistant message but no tool results — leaving an inconsistent history for the next turn.  
**Impact:** Rare race; next turn sees an assistant message with `tool_calls` but no corresponding tool results. The model may hallucinate results or error.  
**Fix:** Move the cancel check BEFORE `PatchPlaceholder`, or include the placeholder in `delete_after` scope.

#### M4. `truncate_for_model` uses char count, not token count
**Path:** `mod.rs:726-731`  
**Issue:** Tool results are truncated at 4000 chars. For CJK/emoji-heavy content, 4000 chars ≈ 4000 tokens. For ASCII, 4000 chars ≈ 1000 tokens. The budget is inconsistent across languages.  
**Impact:** CJK tool results consume 4x more context budget than English ones.

#### M5. SSE parser in `client.rs` doesn't handle `\r\n\r\n` boundaries
**Path:** `client.rs:170`  
**Issue:** The SSE spec allows `\r\n` line endings. The parser splits on `\n\n` only. Some proxies/CDNs rewrite line endings to `\r\n`.  
**Impact:** If OmniRouter is behind a proxy that uses `\r\n`, events are never split and accumulate in `buf` until the stream ends, then all arrive at once (no streaming UX) or the buffer grows unbounded.

### LOW

#### L1. Phantom log writes are best-effort but use synchronous I/O
**Path:** `phantom.rs:123-139`  
**Issue:** `append_event` does synchronous `std::fs::OpenOptions::open` + `writeln!` inside an async context (called from `tool_loop`). On slow filesystems this blocks the tokio runtime thread.  
**Impact:** Negligible in practice (JSONL append is fast), but violates async hygiene.

#### L2. `events_decoded` counter in Anthropic provider undercounts
**Path:** `anthropic.rs:155-156`  
**Issue:** `events_decoded` only increments when `parser.text.len()` changes or `parser.done` is set. Tool-use-only events (no text change, not done) don't increment the counter. This means the "0 events decoded" error check (line 168) could fire even after receiving valid tool_use deltas.  
**Impact:** Extremely unlikely in practice (tool_use blocks are preceded by message_start which sets done-related state), but theoretically possible.

---

## Edge Cases (≥10)

| # | Scenario | Current Behavior | Risk |
|---|----------|-----------------|------|
| 1 | Model returns tool_call with unknown name | `dispatch` returns `Err(Invalid("unknown tool: X"))` → caught, stored as `{"error":"..."}` → model sees error, can retry | OK |
| 2 | Model returns >20 tool_calls in one response | All 20+ execute sequentially; no cap on per-response tool count | Medium — could be used to DoS via expensive tools |
| 3 | Argument JSON is malformed (partial stream cut) | `serde_json::from_str` fails → defaults to `{}` | **H2 above** |
| 4 | Provider 5xx mid-stream after partial text delivered | OmniRouter: if `events_decoded > 0`, partial content is returned as complete response; Anthropic: same | Partial response treated as complete — phantom detector may or may not fire |
| 5 | Model emits tool_call with empty `id` field | Stored with `id: ""` → tool_result references `""` → `lookup_tool_name` can't match → `[Tool result]` without name | Cosmetic; model still sees the result |
| 6 | 20 iterations exhausted with pending tool_calls | System message "Reached max iterations" emitted; tool_calls from last response are NOT executed | OK — clean exit |
| 7 | Concurrent `run_chat_with_attachments` calls | `current_cancel` is a single `RwLock<Option<CancelHandle>>` — second call overwrites the first's handle | **Race**: Stop cancels wrong turn; first turn's cancel handle is orphaned |
| 8 | `cancel.wait()` called after flag already set | Handled: `wait()` checks `is_cancelled()` first and returns immediately | OK |
| 9 | Model returns both `content` (narrative) AND `tool_calls` | Phantom detector returns `false` (has_tools=true) → proceeds normally | OK |
| 10 | `HISTORY_LIMIT=60` exceeded by tool-heavy session | Only last 60 messages loaded; earlier context silently dropped | Model loses early conversation context without explicit compaction signal |
| 11 | System prompt + skills + memory exceeds model context alone | No guard — `build_system_prompt` + `inject_skills` + `build_memory_preamble` can produce 50k+ chars with many skills | Request may fail at API level |
| 12 | `delete_after` fails (DB locked) | Error is swallowed (`let _ = ...`) — inconsistent history persists | Next turn may replay partial state |
| 13 | Two phantom retries succeed on 3rd attempt | `phantom_attempts` reset to 0, loop continues normally | OK |
| 14 | OmniRouter returns HTTP 200 but empty body (no SSE events) | `events_decoded = 0`, `chunk_error = None` → falls through to `Ok(ChatRespMessage { content: None, tool_calls: None })` → model "done" with empty response | Silent no-op turn; user sees empty bubble |

---

## Recommendations (prioritized)

1. **[C1] Implement token budget guard** — estimate tokens before sending; compact or trim if >80% of context window. Start with chars/4 heuristic.
2. **[C2] Pass CancelHandle to tool dispatch** — especially `wait_for_agent_idle`. Add cancel check inside the polling loop.
3. **[H1] Add retry logic to OmniProvider** — 2 attempts with 500ms jittered backoff on 5xx/network errors.
4. **[H2] Fail explicitly on malformed tool_call arguments** — return error tool_result instead of defaulting to `{}`.
5. **[H3] Tighten phantom regex patterns** — reduce false-positive surface on patterns 1 and 5.
6. **[H4] Parallelize independent tool_calls** — at minimum, multiple `send_to_agent` to different agents.
7. **[M1] Wire up `build_provider` to settings** — respect `KEY_PROVIDER` so users can switch between Anthropic and OmniRouter.
8. **[M3] Fix cancel-vs-persist race** — check cancel before patching placeholder, or widen `delete_after` scope.
9. **[M5] Handle `\r\n` in SSE parser** — normalize `\r\n` to `\n` before splitting.
10. **[Edge 7] Prevent concurrent turn execution** — either reject or queue a second `run_chat_with_attachments` while one is in-flight.

---

## Boundary Notes

- **Raw SSE decode internals** (byte-level framing, UTF-8 boundary splits): covered by task `3b787801`. This audit covers the semantic layer above (event→delta→accumulator→response).
- **MCP wire protocol** (JSON-RPC framing, tool schema validation): task `aa2a9743`.
- **Persistence layer** (SQLite schema, migrations, WAL mode): task `18f88517`.
- **Security & concurrency** (auth, TOCTOU, lock contention): task `89ee4d9d`.
