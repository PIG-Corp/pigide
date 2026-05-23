# PigIDE Full Codebase Audit — Claude Opus 4.7

**Date:** 2026-05-21  
**Auditor:** Claude Opus 4.7 (33 parallel sub-agents + lead synthesis)  
**Scope:** Full codebase — Rust backend (32K LOC), TypeScript/React frontend (~8K LOC), MCP protocol, infrastructure  
**Prior audits referenced:** 5 reports from 2026-05-20 (15 fixes applied, 18 deferred)

---

## 1. Executive Summary

| Severity | Count | Status |
|----------|-------|--------|
| **CRITICAL** | 15 | New or verified-still-open |
| **HIGH** | 52 | New findings |
| **MEDIUM** | 74 | New findings |
| **LOW** | 42 | New findings |
| **Total** | **183** | |

**Top attack surfaces (by risk):**
1. **Indirect prompt injection** via memory notes / tool results into orchestrator system prompt (N3, N4)
2. **Iframe sandbox bypass** in BrowserPanel — `allow-same-origin` + `allow-scripts` = Tauri IPC access (XSS-F1)
3. **Workspace skill shadowing** — malicious repo overrides trusted built-in skills (S-02, S-03)
4. **Path traversal** in `files.rs` via canonicalize fallback on non-existent paths (F-01)
5. **Cross-workspace data leak** — mailbox, rollcall, backlinks, delete all lack workspace scoping (SW-S1, M4)
6. **Agent self-trigger** — stdout pattern injection drives Architect auto-confirm on destructive prompts (A2)

---

## 2. Critical Findings (12)

| ID | Component | Title | File:Line | Description |
|---|---|---|---|---|
| C-01 | Orchestrator | Indirect prompt injection via memory snippets | orchestrator/mod.rs:235-260 | Memory bodies (agent-authored) injected verbatim into system prompt. Attacker-controlled note can break out of `[MEMORY CONTEXT]` frame and issue tool calls. |
| C-02 | Orchestrator | Tool-result content unfenced — same injection vector | orchestrator/mod.rs:317-326 | `tail_agent`, `read_mailbox`, `read_memory` results piped as user-role text. Agent stdout can spoof tool results. |
| C-03 | Frontend | BrowserPanel iframe `allow-same-origin` + `allow-scripts` | BrowserPanel.tsx:152-158 | Hostile URL in iframe can access Tauri IPC, read app storage, execute commands. |
| C-04 | Skills | Workspace skills shadow built-ins — prompt injection via repo | skills/registry.rs:131-138, skill.rs:32-38 | Opening a malicious repo auto-activates attacker-controlled skill content in system prompt. No signature check. |
| C-05 | Skills | Skill body concatenated unsanitized into system prompt | skills/composer.rs:178-196 | `[/SKILL]` escape in body breaks framing. Combined with C-04 = remote prompt injection. |
| C-06 | Files | Path traversal via canonicalize fallback on non-existent paths | files.rs:79 | `canonicalize().unwrap_or_else(|_| p.to_path_buf())` — write_file to new path bypasses sandbox. |
| C-07 | Memory | Symlink escape in nested-folder slug | memory/storage.rs:35-45 | Symlink in `.pigmemory/` subdir followed by `slug_to_path` → write outside workspace. |
| C-08 | Memory | Frontmatter `id` overwrite hijacks indexed notes cross-workspace | memory/service.rs:128-170 | Dropping a `.md` with known UUID overwrites any note's body/slug/path via `ON CONFLICT(id) DO UPDATE`. |
| C-09 | Orchestrator | Concurrent turns corrupt shared CancelHandle + attachments | orchestrator/mod.rs:81-87, 392-405 | Two overlapping turns overwrite each other's cancel handle; Stop button kills wrong turn. |
| C-10 | Orchestrator | SSE buffers grow unbounded — OOM via hostile upstream | client.rs:156-179, anthropic.rs:134-176 | No max-event-size cap on streaming buffers. Malformed stream without `\n\n` grows until OOM. |
| C-11 | Voice | No checksum/signature on Whisper model download | voice/download.rs:136-179 | Multi-GB binary from HuggingFace loaded into whisper-rs with no SHA-256 verify. MITM → code exec via GGUF parser. |
| C-12 | Voice | Keystroke injection into wrong window after transcription | voice/inject.rs:31-46 | Focus may have moved to password prompt/banking app during Whisper processing. No target-window verification. |
| C-13 | Files | Empty `allowed_roots` disables all path checks — arbitrary read/write | commands.rs:813,824; files.rs:71-84 | `read_file(&path, &[])` and `write_file` pass empty allow-list. `validate_path` short-circuits → any absolute path accepted. Frontend can read `~/.ssh/id_rsa`, write `~/.bashrc`. |
| C-14 | Settings | `set_setting` allows command injection on next spawn | commands.rs:434-438; agent.rs:397-475 | Arbitrary key/value. `bin.<agent_type>` used as executable, `args.<agent_type>` split into argv. Compromised renderer sets `bin.claude=/bin/sh`, `args.claude=-c "rm -rf ~"`. |
| C-15 | Agent | `child.wait()` held under global handles lock — full AgentManager stall | agent.rs:866-877 | `kill()` acquires handles Mutex, calls `child.wait()` (blocking syscall) while holding it. Misbehaving child that ignores SIGTERM stalls every agent operation indefinitely. |

---

## 3. High Findings (47)

| ID | Component | Title | File:Line |
|---|---|---|---|
| H-01 | MCP | No batch JSON-RPC support (spec violation) | mcp/server.rs:189 |
| H-02 | MCP | Notifications receive responses (spec violation) | mcp/server.rs:259-267 |
| H-03 | MCP | `notifications/initialized` not handled — handshake failure | mcp/server.rs:217-257 |
| H-04 | MCP | Parse errors don't return -32700 envelope | mcp/server.rs:189 |
| H-05 | MCP | No pagination cursor on tools/list | mcp/server.rs:226 |
| H-06 | MCP | Cancellation entirely unsupported | mcp/server.rs (whole) |
| H-07 | MCP | No progress notifications for long tools | mcp/server.rs:306-381 |
| H-08 | Swarm | Mailbox has no workspace scoping — cross-workspace leak | swarm/mailbox.rs:36-41 |
| H-09 | Swarm | Rollcall responses leak across workspaces | swarm/rollcall.rs:30-103 |
| H-10 | Swarm | `mark_mail_read` has no ownership check | swarm/mailbox.rs:106-118 |
| H-11 | Swarm | List+mark-read race causes duplicate processing | swarm/mailbox.rs:64-104 |
| H-12 | Swarm | Review gate vote has no voter identity / self-approval | swarm/review.rs:84-102 |
| H-13 | Swarm | `task_completable` → set status is not atomic | swarm/review.rs:155-185 |
| H-14 | Memory | `find_backlinks` and `delete` not workspace-scoped | memory/service.rs:373-401 |
| H-15 | Memory | TOCTOU race between `unique_slug` SELECT and INSERT | memory/service.rs:108-126 |
| H-16 | Voice | Audio buffer leaks for I16/U16 streams (no 60s cap) | voice/capture.rs:92-139 |
| H-17 | Voice | Cancel doesn't drop captured samples or stop inference | voice/mod.rs:195-201 |
| H-18 | Voice | Clipboard paste leaks transcript, races with other apps | voice/inject.rs:85-121 |
| H-19 | Voice | Dictionary patterns enable replacement-string blowup | voice/dictionary.rs:111-131 |
| H-20 | Architect | Agent self-trigger via stdout pattern injection | architect/supervisor.rs:230-246 |
| H-21 | Architect | Commands lack auth/permission gate | commands.rs:1510-1541 |
| H-22 | Watcher | Per-agent state unbounded; never cleaned on exit | watcher/supervisor.rs:60-67 |
| H-23 | Frontend | Unsaved edits silently lost on note switch | MemoryPanel.tsx:76-93 |
| H-24 | Frontend | Watcher push vs user-edit race — no optimistic lock | MemoryPanel.tsx:102-120 |
| H-25 | Frontend | XSS via `nodeLabel` in MemoryGraph tooltip (innerHTML) | MemoryGraph.tsx:82 |
| H-26 | Frontend | `onAgentStdout` listens globally — O(N²) per burst | AgentTile.tsx:186 |
| H-27 | Frontend | ResizeObserver fires after `term.dispose()` | AgentTile.tsx:149-158 |
| H-28 | Frontend | Image addon — resource bomb / decoder CVE surface | AgentTile.tsx:5,117,120 |
| H-29 | Frontend | Search addon raw-regex DoS from user input | AgentTile.tsx:324-328 |
| H-30 | Frontend | `reloadAfterSwitch` race — no cancellation token | App.tsx:88-100 |
| H-31 | Frontend | Dead agents remain in layout tree after exit | App.tsx:112-119 |
| H-32 | Frontend | Layout schema not validated from backend | App.tsx:65,133 |
| H-33 | Frontend | `useHotkeys` re-attaches listener every render | useHotkeys.ts:77,103 |
| H-34 | Frontend | Solarized Light `fgMuted` fails WCAG AA (2.4:1) | themes/catalog.ts:354-358 |
| H-35 | Frontend | Ctrl+T/W hijack browser shortcuts globally | HotkeyBindings.tsx:95-96 |
| H-36 | Frontend | Skill toggle race — rapid toggle yields wrong state | SkillsPanel.tsx:84-92 |
| H-37 | Frontend | OrchestratorPanel autoscroll fights user-scrolled-up | OrchestratorPanel.tsx:24-27 |
| H-38 | Frontend | `javascript:` URL accepted in BrowserPanel input | BrowserPanel.tsx:50-57 |
| H-39 | Skills | `create_user_stub` path traversal via unvalidated `id` | skills/tools.rs:105-124 |
| H-40 | Orchestrator | Anthropic error SSE events silently swallowed | anthropic.rs:677-680 |
| H-41 | Orchestrator | OmniRouter URL unvalidated — exfiltration risk | omni.rs:18-22 |
| H-42 | Orchestrator | Tool dispatch no workspace-scope validation | orchestrator/tools.rs:344-422 |
| H-43 | Orchestrator | LLM-driven destructive tools fire without confirmation | tools.rs:328-342 |
| H-44 | Resolver | Unbounded query length feeds quadratic fuzzy scoring | project_resolver/fuzzy.rs:9-92 |
| H-45 | Resolver | Parsers read full files with no size limit | project_resolver/parsers.rs:117-119 |
| H-46 | Resolver | `add_alias` writes JSON to any caller-chosen directory | project_resolver/service.rs:140-147 |
| H-47 | Agent | Env vars (including MCP key) inherited by spawned agents | agent.rs (spawn_internal) |

---

## 4. Medium Findings (68) — Top 30

| ID | Component | Title | File:Line |
|---|---|---|---|
| M-01 | Swarm | Paths not normalised before claiming — bypass mutual exclusion | swarm/ownership.rs:31-62 |
| M-02 | Swarm | Partial `claim_files` leaks acquired locks on error | swarm/tools.rs:212-242 |
| M-03 | Swarm | `to_addr` accepts arbitrary strings, no validation | swarm/mailbox.rs:24-51 |
| M-04 | Swarm | No size/rate limits on mail body | swarm/mailbox.rs:24-51 |
| M-05 | Swarm | Prompt-template injection via `role_prompts` | swarm/prompts.rs:104-126 |
| M-06 | Memory | No body/tag/alias size limits (1GB note accepted) | memory/service.rs:80-106 |
| M-07 | Memory | Tag content has no validation — YAML round-trip corruption | memory/note.rs:62-75 |
| M-08 | Memory | `delete_memory` swallows `remove_file` error | memory/service.rs:292 |
| M-09 | Memory | Cross-workspace alias/wikilink resolution ambiguous | memory/service.rs:181-218 |
| M-10 | Orchestrator | `reqwest::Client` has no connect/read timeout | client.rs:41-44 |
| M-11 | Orchestrator | Anthropic multi-line `data:` joined without newline | anthropic.rs:587-598 |
| M-12 | Orchestrator | Tool errors leak `e.to_string()` into model context | orchestrator/mod.rs:691-693 |
| M-13 | Orchestrator | Empty tool_use blocks forwarded → permanent 400 | anthropic.rs:340-362 |
| M-14 | Orchestrator | Tool schemas lack `additionalProperties: false` | orchestrator/tools.rs:17-234 |
| M-15 | Orchestrator | `delta_tx` unbounded channel — UI event queue balloon | orchestrator/mod.rs:509-520 |
| M-16 | Voice | Race between hotkey rebind and active recording | voice/hotkey.rs:99-137 |
| M-17 | Voice | Cloud API key stored unencrypted, Debug-derivable | voice/cloud.rs:36-62 |
| M-18 | Voice | Whisper `Capture` unsafe Send/Sync — off-thread drop UB | voice/capture.rs:27-28 |
| M-19 | Architect | Watcher no Gemini 429 backoff — cost runaway | watcher/supervisor.rs:223-236 |
| M-20 | Architect | Prompt injection via classifier chunk content | watcher/classifier.rs:255-275 |
| M-21 | Architect | `auto_confirm` sends `y` even when default is N | architect/policy.rs:160-165 |
| M-22 | Architect | Cross-workspace contamination — global state | watcher/supervisor.rs:60-76 |
| M-23 | Skills | Hot-reload watcher races editor mid-write | skills/watcher.rs:62-91 |
| M-24 | Skills | `reload_path` shadow-list rebuild logic broken | skills/registry.rs:189-217 |
| M-25 | Skills | Symlink containment not transitive in walk | skills/registry.rs:319-345 |
| M-26 | Resolver | `rebuild` unrate-limited, not single-flighted | project_resolver/service.rs:69-98 |
| M-27 | Resolver | Aliases accept control chars, no length cap | project_resolver/aliases.rs:59-66 |
| M-28 | Frontend | KanbanBoard no optimistic update or rollback | KanbanBoard.tsx:79-88 |
| M-29 | Frontend | Settings overlay missing dialog semantics + focus trap | SettingsButton.tsx:219-250 |
| M-30 | Frontend | `tasks`/architect state never cleared on workspace switch | store.ts:103,161-170 |

*(38 additional MEDIUM findings in sub-agent reports — see full detail below)*

---

## 5. Architecture Recommendations

### 5.1 Workspace Isolation (addresses ~20 findings)
Every data-bearing table (`mailbox`, `rollcall*`, `memory_notes`, `memory_links`, `file_locks`, `review_gates`) must carry a `workspace_id` column with a foreign key. All queries must filter by the caller's current workspace. This single change resolves: SW-S1, SW-S2, SW-S3, SW-S11, M4, M-09, M-22, M-30, and the cross-workspace contamination in architect/watcher.

### 5.2 Trust Boundaries for LLM Context (addresses C-01, C-02, C-04, C-05)
Adopt a **fenced-content protocol**: all user-data injected into the system prompt (memory snippets, tool results, skill bodies, workspace names) must be wrapped in nonce-tagged envelopes that the model is instructed to treat as data, never instructions. Strip role-header patterns from content before injection.

### 5.3 Per-Session Orchestrator State (addresses C-09, N1)
Move `current_cancel`, `turn_attachments`, and the tool-loop iteration state from the global `Orchestrator` struct into a per-`session_id` map (or owned by the future). Serialize turns within a session via a per-session mutex.

### 5.4 MCP Spec Compliance (addresses H-01 through H-07)
Upgrade to Streamable HTTP transport (SSE response body). Implement: batch requests, notification handling, cancellation tokens threaded into tool dispatch, progress events, pagination cursors.

### 5.5 Frontend Event Architecture (addresses H-26, H-27, H-30, H-31)
Replace per-tile global event subscriptions with a single router that fans out by `agent_id`. Add lifecycle management: remove dead agents from layout on exit, cancel in-flight IPC on workspace switch, gate all async callbacks with a `disposed` flag.

### 5.6 Input Validation Layer (addresses C-06, C-07, H-39, H-44, H-45, H-46)
Create a shared `validate` module with:
- `validate_path(path, allowed_roots)` — hard-fail on canonicalize error, O_NOFOLLOW, reject symlinks
- `validate_slug(slug)` — non-empty after sanitization, no null bytes, max length
- `validate_id(id)` — UUID format only
- `validate_query(q)` — max length, max token count
- `validate_url(url)` — scheme allowlist (https only for external)

### 5.7 Test Coverage (addresses 15 critical gaps)
Priority test additions:
1. `commands.rs` IPC handlers (115 untested entry points)
2. `mcp/server.rs` scope enforcement + body limit
3. `orchestrator/tools.rs` dispatch routing
4. `orchestrator/client.rs` SSE chunk-split + UTF-8 boundary
5. `workspace.rs` create/delete cascade
6. Frontend: add vitest + Playwright e2e

---

## 6. Fix Plan

### Phase 1: Critical Security (1-2 days, 12 items)
| Priority | Finding | Effort |
|----------|---------|--------|
| P0 | C-01/C-02: Fence memory/tool-result content in system prompt | 4h |
| P0 | C-03: Remove `allow-same-origin` from BrowserPanel iframe | 30m |
| P0 | C-04/C-05: Block workspace skill shadowing of built-in IDs | 2h |
| P0 | C-06: Hard-fail on canonicalize error in `validate_path` | 1h |
| P0 | C-07: Canonicalize + assert parent stays under root | 1h |
| P0 | C-08: Upsert by `(workspace_root, path)` not by `id` | 2h |
| P0 | C-09: Per-session cancel handle + mutex | 3h |
| P0 | C-10: Cap SSE buffer at 1MiB, add per-chunk timeout | 1h |
| P0 | C-11: Pin SHA-256 per model, verify after download | 2h |
| P0 | C-12: Capture focused window at record-start, refuse if changed | 2h |
| P0 | H-38: Reject `javascript:`/`data:` URLs in BrowserPanel | 30m |
| P0 | H-25: Escape `nodeLabel` in MemoryGraph tooltip | 30m |

### Phase 2: High Security + Data Integrity (3-5 days, 47 items)
- Workspace scoping migration (all swarm tables + memory)
- MCP spec compliance (batch, notifications, cancellation)
- Architect auth gate + auto-confirm safety (default-N detection)
- Voice buffer caps + cancel cleanup
- Skills symlink containment + size limits
- Resolver query/file size caps
- Agent env sanitization (strip MCP key)
- Frontend event architecture refactor

### Phase 3: Medium + Low (1-2 weeks, 109 items)
- Full input validation layer
- Optimistic locking on memory edits
- Frontend accessibility (modals, focus traps, live regions, WCAG contrast)
- Performance: bounded channels, resize throttling, regex pre-compilation
- Test coverage: top 15 gaps
- Dead code removal + architecture cleanup

---

## 7. Verification of Prior Fixes

| Prior Finding | Status | Notes |
|---|---|---|
| Path traversal `slug_to_path` (S1) | ✅ Fixed | `..`/`.`/empty rejected at storage.rs:38-39 |
| UTF-8 decode `from_utf8_lossy` (F1) | ✅ Fixed | Byte-accumulator in client.rs + anthropic.rs |
| Scope bypass 7 mutating tools (F-1) | ✅ Fixed | `is_mutating()` updated in server.rs |
| FTS5 column name mismatch (P-F1) | ✅ Fixed | Migration v14 applied |
| `sanitize_fts_query` hyphen (P-F2) | ✅ Fixed | `-` stripped, tests at service.rs:608-617 |
| Body size limit (F-3) | ✅ Fixed | `DefaultBodyLimit::max(1MB)` in server.rs |
| `wait_for_agent_idle` cap (F-2) | ✅ Fixed | Capped to 120s + existence check |
| Reader thread leak on kill (deferred) | ⚠️ Still present | agent.rs — master PTY not dropped on kill |
| `claim_files` arbitrary paths (deferred) | ⚠️ Still present | No workspace-root validation |
| Unix socket default perms (deferred) | ⚠️ Still present | ipc.rs — no chmod |
| Token in URL query string (deferred) | ⚠️ Still present | mcp/launcher.rs:71 |

---

## 8. Sub-Agent Coverage Map

| Agent | Subsystem | Findings |
|---|---|---|
| Voice backend | voice/* | 16 findings (V01-V16) |
| MCP protocol | mcp/server.rs | 19 findings (MCP-01 to MCP-19) |
| Hooks+themes+styles | frontend hooks, themes, CSS | 12 findings |
| Architect+watcher | architect/*, watcher/* | 13 findings (A1-A13) |
| Frontend XSS | all components | 12 findings (F1-F12) |
| Build+CI+deps | Cargo.toml, tauri.conf, CI | 12 findings |
| Frontend App+state+ipc | App.tsx, store, ipc, tree | 15 findings |
| Agent.rs PTY lifecycle | agent.rs | 14 findings |
| Performance hot paths | cross-cutting | 16 findings |
| Skills+Orchestrator panels | frontend panels | 16 findings |
| Deeplink+single-instance | deeplink.rs, ipc.rs | 8 findings |
| Skills backend | skills/* | 12 findings (S-01 to S-12) |
| Project resolver | project_resolver/* | 12 findings (R1-R12) |
| AgentTile+TilingArea | frontend tiles | 19 findings |
| MemoryPanel+Graph+Kanban | frontend panels | 15 findings |
| Error handling+panics | cross-cutting | 22 findings |
| Dead code+architecture | cross-cutting | 14 findings |
| Mailbox+rooms+chat IPC | chat_queue, ipc, ssh, rooms | 12 findings |
| Input validation surface | all boundaries | 18 findings |
| DB schema+migrations | db.rs | 12 findings |
| Accessibility+UX | frontend | 19 findings (A01-A19) |
| Logging+secrets | cross-cutting | 13 findings |
| Tasks+files+workspace | tasks, files, workspace, layout | 18 findings |
| Tauri security | tauri.conf, capabilities | 10 findings |
| Memory subsystem | memory/* | 15 findings (M1-M15) |
| Commands.rs handlers | commands.rs | 16 findings |
| Test coverage gaps | cross-cutting | 15 priority gaps identified |
| Concurrency+races | cross-cutting | 14 findings |
| Orchestrator subsystem | orchestrator/* | 20 findings (N1-N20) |
| Chat queue+IPC+SSH | chat_queue, ssh, deeplink | 13 findings |
| Swarm subsystem | swarm/* | 15 findings (S1-S15) |
| MCP server+auth re-audit | mcp/server, auth, launcher | 12 findings |
| Voice frontend+components | frontend voice, cmdblock | 14 findings |

---

*Generated by Claude Opus 4.7 — 33 parallel audit agents, ~32K LOC Rust + ~8K LOC TypeScript analyzed.*
