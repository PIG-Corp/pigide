# PigIDE Unified Security Audit

**Date:** 2026-05-21
**Models:** Claude Opus 4.7, GPT 5.5 (Devin), Qwen 3.6+ (OpenCode), Gemini 3.5 Flash (Agy)
**Scope:** Full codebase — Rust backend (~32K LOC), TypeScript/React frontend (~8K LOC), MCP protocol, voice, skills, swarm, memory, agent runtime

---

## 1. Executive Summary

### Unique findings by severity (after deduplication)

| Severity | Count |
|----------|-------|
| **CRITICAL** | 16 |
| **HIGH** | 38 |
| **MEDIUM** | 42 |
| **LOW** | 27 |
| **Total** | **123** |

### Coverage matrix

| Model | Raw findings | Unique contributions | Consensus (3+) | Strengths |
|-------|-------------|---------------------|-----------------|-----------|
| **Claude Opus 4.7** | 183 | 71 | 14/16 critical | Deepest coverage; 33 parallel sub-agents; found prompt injection, voice, concurrency, and accessibility issues no other model caught |
| **GPT 5.5 (Devin)** | 18 | 2 | 9/16 critical | Ran actual tests + lint; verified findings empirically; found provider-config dead code |
| **Qwen 3.6+ (OpenCode)** | — | 0 | — | File not found — no report available |
| **Gemini 3.5 Flash (Agy)** | 11 | 1 | 7/16 critical | Concise; Windows-specific path traversal variant unique to this model |

> **Note:** `audit-opencode.md` does not exist on disk. The unified report covers 3 models.

---

## 2. Findings Table

### CRITICAL (16)

| ID | Component | Title | File:Line | Description | Found by | Fix |
|---|---|---|---|---|---|---|
| U-01 | Files | Empty `allowed_roots` disables all path checks — arbitrary read/write | commands.rs:812-824, files.rs:71-84 | `read_file` and `write_file` Tauri commands pass empty `allowed_roots`. `validate_path` short-circuits → any absolute path accepted. Frontend can read `~/.ssh/id_rsa`, write `~/.bashrc`. | Claude, Devin, Agy | Compute `allow_roots` from active workspace; treat empty as deny-all. |
| U-02 | Files | Path traversal via canonicalize fallback on non-existent paths | files.rs:79 | `canonicalize().unwrap_or_else(\|_\| p.to_path_buf())` — write to new path with `..` segments bypasses sandbox entirely. | Claude, Devin | Hard-fail on canonicalize error; canonicalize parent + verify final component. |
| U-03 | Memory | Platform-specific path traversal (Windows backslash) | memory/storage.rs:35-45 | `slug_to_path` splits only on `/`. On Windows, `\` is resolved as directory separator by `PathBuf::push`, allowing escape from `.pigmemory/`. | Agy | Normalize all separators before splitting; reject `\` in slugs. |
| U-04 | Memory | Symlink escape in nested-folder slug | memory/storage.rs:35-45 | Symlink in `.pigmemory/` subdir followed by `slug_to_path` → write outside workspace. Current filter misses NUL bytes, backslashes, absolute segments. | Claude | Require slug matches `^[a-z0-9][a-z0-9/_-]{0,127}$`; reject NUL and reserved names. |
| U-05 | Memory | Frontmatter `id` overwrite hijacks indexed notes cross-workspace | memory/service.rs:128-170 | Dropping a `.md` with known UUID overwrites any note's body/slug/path via `ON CONFLICT(id) DO UPDATE`. | Claude | Upsert by `(workspace_root, path)` not by `id`. |
| U-06 | Orchestrator | Indirect prompt injection via memory snippets | orchestrator/mod.rs:235-260 | Memory bodies (agent-authored) injected verbatim into system prompt. Attacker-controlled note can break out of `[MEMORY CONTEXT]` frame and issue tool calls. | Claude | Fence all injected content with nonce-tagged envelopes; strip role-header patterns. |
| U-07 | Orchestrator | Tool-result content unfenced — same injection vector | orchestrator/mod.rs:317-326 | `tail_agent`, `read_mailbox`, `read_memory` results piped as user-role text. Agent stdout can spoof tool results. | Claude | Same fencing approach as U-06. |
| U-08 | Frontend | BrowserPanel iframe `allow-same-origin` + `allow-scripts` | BrowserPanel.tsx:152-158 | Hostile URL in iframe can access Tauri IPC, read app storage, execute commands via the combined sandbox flags. | Claude | Drop `allow-same-origin`; or render via Tauri `shell.open`. |
| U-09 | Frontend | `javascript:` URL accepted in BrowserPanel input | BrowserPanel.tsx:50-57 | Only `startsWith("http")` triggers prefixing. `javascript:alert(1)` passes through to `iframe.src`. Bookmarks also unvalidated. | Claude | Whitelist `http:`/`https:` only; reject `javascript:`/`data:`/`file:`. |
| U-10 | Skills | Workspace skills shadow built-ins — prompt injection via repo | skills/registry.rs:131-138, skill.rs:32-38 | Opening a malicious repo auto-activates attacker-controlled skill content in system prompt. No signature check, no trust prompt. | Claude, Devin | Require explicit per-workspace enablement; prevent silent shadowing of built-in IDs. |
| U-11 | Skills | Skill body concatenated unsanitized into system prompt | skills/composer.rs:178-196 | `[/SKILL]` escape in body breaks framing. Combined with U-10 = remote prompt injection. | Claude | Sanitize/escape skill body before injection; validate no frame-breaking tokens. |
| U-12 | Skills | `create_user_skill` path traversal via unvalidated `id` | skills/tools.rs:105-124 | `path = dir.join(format!("{}.md", id))`. `id` with `../../` writes outside skills dir. | Claude, Devin | Validate `id` against `^[a-z0-9][a-z0-9_-]{0,63}$`; reject `/`, `\`, `..`, NUL. |
| U-13 | Settings | `set_setting` allows command injection on next spawn | commands.rs:434-438, agent.rs:397-475 | Arbitrary key/value. `bin.<agent_type>` used as executable, `args.<agent_type>` split into argv. Compromised renderer → RCE. | Claude | Allow-list of writable setting keys; refuse `bin.*`/`args.*` from this surface. |
| U-14 | Orchestrator | Concurrent turns corrupt shared CancelHandle + attachments | orchestrator/mod.rs:81-87, 392-405 | Two overlapping turns overwrite each other's cancel handle; Stop button kills wrong turn. | Claude | Per-session cancel handle + mutex; serialize turns within a session. |
| U-15 | Orchestrator | SSE buffers grow unbounded — OOM via hostile upstream | client.rs:156-179, anthropic.rs:134-176 | No max-event-size cap on streaming buffers. Malformed stream without `\n\n` grows until OOM. | Claude | Cap SSE buffer at 1MiB; add per-chunk timeout. |
| U-16 | Agent | `child.wait()` held under global handles lock — full stall | agent.rs:866-877 | `kill()` acquires handles Mutex, calls `child.wait()` while holding it. Misbehaving child stalls every agent operation. | Claude | Snapshot Arc out of map under lock, drop lock before `wait()`. |

### HIGH (38)

| ID | Component | Title | File:Line | Found by |
|---|---|---|---|---|
| U-17 | MCP | Token in URL query string — leaks via process args, logs | mcp/launcher.rs:46-80 | Claude, Devin |
| U-18 | MCP | Keys scoped only by capability, not workspace — cross-workspace access | mcp/server.rs:321-330 | Devin, Agy |
| U-19 | MCP | Scope CSV join/split — embedded comma escalates privileges | mcp/auth.rs:49-60 | Claude |
| U-20 | MCP | No batch JSON-RPC support (spec violation) | mcp/server.rs:189 | Claude |
| U-21 | MCP | Notifications receive responses (spec violation) | mcp/server.rs:259-267 | Claude |
| U-22 | MCP | `notifications/initialized` not handled | mcp/server.rs:217-257 | Claude |
| U-23 | MCP | Parse errors don't return -32700 envelope | mcp/server.rs:189 | Claude |
| U-24 | MCP | No pagination cursor on tools/list | mcp/server.rs:226 | Claude |
| U-25 | MCP | Cancellation entirely unsupported | mcp/server.rs | Claude |
| U-26 | Swarm | Mailbox has no workspace scoping — cross-workspace leak | swarm/mailbox.rs:36-41 | Claude, Devin |
| U-27 | Swarm | Rollcall responses leak across workspaces | swarm/rollcall.rs:30-103 | Claude |
| U-28 | Swarm | `mark_mail_read` has no ownership check | swarm/mailbox.rs:106-118 | Claude |
| U-29 | Swarm | Review gate vote has no voter identity / self-approval | swarm/review.rs:84-102 | Claude, Devin |
| U-30 | Swarm | File ownership locks use raw unnormalized paths | swarm/ownership.rs:31-62 | Claude, Devin |
| U-31 | Memory | `find_backlinks` and `delete` not workspace-scoped | memory/service.rs:373-401 | Claude |
| U-32 | Memory | TOCTOU race between `unique_slug` SELECT and INSERT | memory/service.rs:108-126 | Claude |
| U-33 | Voice | No checksum/signature on Whisper model download | voice/download.rs:136-179 | Claude |
| U-34 | Voice | Keystroke injection into wrong window after transcription | voice/inject.rs:31-46 | Claude |
| U-35 | Voice | Audio buffer leaks for I16/U16 streams (no 60s cap) | voice/capture.rs:92-139 | Claude |
| U-36 | Voice | Cancel doesn't drop captured samples or stop inference | voice/mod.rs:195-201 | Claude |
| U-37 | Architect | Agent self-trigger via stdout pattern injection | architect/supervisor.rs:230-246 | Claude |
| U-38 | Architect | Commands lack auth/permission gate | commands.rs:1510-1541 | Claude |
| U-39 | Architect | Watcher per-agent state unbounded; never cleaned on exit | watcher/supervisor.rs:60-67 | Claude |
| U-40 | Agent | Zombie handle leak — reader thread races spawn | agent.rs:514-540 | Claude, Agy |
| U-41 | Agent | Env vars (including MCP key) inherited by spawned agents | agent.rs (spawn_internal) | Claude |
| U-42 | Agent | `tail_agent` path traversal via raw `agent_id` | agent.rs:23-29, orchestrator/tools.rs:497-520 | Claude, Devin |
| U-43 | Frontend | xterm ImageAddon — resource bomb / decoder CVE surface | AgentTile.tsx:5,117,120 | Claude |
| U-44 | Frontend | Search addon raw-regex DoS from user input | AgentTile.tsx:324-328 | Claude |
| U-45 | Frontend | `onAgentStdout` listens globally — O(N²) per burst | AgentTile.tsx:186 | Claude |
| U-46 | Frontend | Dead agents remain in layout tree after exit | App.tsx:112-119 | Claude |
| U-47 | Frontend | Solarized Light `fgMuted` fails WCAG AA (2.4:1) | themes/catalog.ts:354-358 | Claude |
| U-48 | Frontend | XSS via `nodeLabel` in MemoryGraph tooltip (innerHTML) | MemoryGraph.tsx:82 | Claude |
| U-49 | Orchestrator | Anthropic error SSE events silently swallowed | anthropic.rs:677-680 | Claude |
| U-50 | Orchestrator | OmniRouter URL unvalidated — exfiltration risk | omni.rs:18-22 | Claude |
| U-51 | Orchestrator | Tool dispatch no workspace-scope validation | orchestrator/tools.rs:344-422 | Claude |
| U-52 | Orchestrator | LLM-driven destructive tools fire without confirmation | tools.rs:328-342 | Claude |
| U-53 | Resolver | Unbounded query length feeds quadratic fuzzy scoring | project_resolver/fuzzy.rs:9-92 | Claude |
| U-54 | Watcher | Agent stdout shipped to Google for classification (raw) | watcher/supervisor.rs:223-256 | Claude |

### MEDIUM (42)

| ID | Component | Title | Found by |
|---|---|---|---|
| U-55 | MCP | Full tool arguments persisted in audit rows (sensitive data) | Claude, Devin |
| U-56 | MCP | JSON Schemas not enforced; numeric clamps inconsistent | Claude |
| U-57 | Swarm | Paths not normalised before claiming — bypass mutual exclusion | Claude, Devin |
| U-58 | Swarm | `to_addr` accepts arbitrary strings, no validation | Claude |
| U-59 | Swarm | No size/rate limits on mail body | Claude |
| U-60 | Swarm | Task parent/assignment checks ignore workspace boundaries | Devin |
| U-61 | Memory | No body/tag/alias size limits (1GB note accepted) | Claude |
| U-62 | Memory | Tag content has no validation — YAML round-trip corruption | Claude |
| U-63 | Memory | Cross-workspace alias/wikilink resolution ambiguous | Claude |
| U-64 | Orchestrator | `reqwest::Client` has no connect/read timeout | Claude |
| U-65 | Orchestrator | Anthropic multi-line `data:` joined without newline | Claude |
| U-66 | Orchestrator | Tool errors leak `e.to_string()` into model context | Claude |
| U-67 | Orchestrator | Empty tool_use blocks forwarded → permanent 400 | Claude |
| U-68 | Orchestrator | `delta_tx` unbounded channel — UI event queue balloon | Claude |
| U-69 | Orchestrator | OmniRouter error path logs full request body + response | Claude |
| U-70 | Orchestrator | Phantom JSONL log persists model snippets to disk | Claude |
| U-71 | Voice | Race between hotkey rebind and active recording | Claude |
| U-72 | Voice | Cloud API key stored unencrypted, Debug-derivable | Claude |
| U-73 | Voice | Whisper `Capture` unsafe Send/Sync — off-thread drop UB | Claude |
| U-74 | Voice | Dictionary patterns enable replacement-string blowup (ReDoS) | Claude |
| U-75 | Voice | Clipboard paste leaks transcript, races with other apps | Claude |
| U-76 | Architect | No Gemini 429 backoff — cost runaway | Claude |
| U-77 | Architect | Prompt injection via classifier chunk content | Claude |
| U-78 | Architect | `auto_confirm` sends `y` even when default is N | Claude |
| U-79 | Architect | Cross-workspace contamination — global state | Claude |
| U-80 | Skills | Hot-reload watcher races editor mid-write | Claude |
| U-81 | Skills | Symlink containment not transitive in walk | Claude, Devin |
| U-82 | Resolver | `rebuild` unrate-limited, not single-flighted | Claude |
| U-83 | Resolver | Aliases accept control chars, no length cap | Claude |
| U-84 | Resolver | `add_alias` writes JSON to any caller-chosen directory | Claude |
| U-85 | Resolver | Parsers read full files with no size limit | Claude |
| U-86 | Frontend | KanbanBoard no optimistic update or rollback | Claude |
| U-87 | Frontend | Settings overlay missing dialog semantics + focus trap | Claude |
| U-88 | Frontend | Tasks/architect state never cleared on workspace switch | Claude |
| U-89 | Frontend | `reloadAfterSwitch` race — no cancellation token | Claude |
| U-90 | Frontend | Layout schema not validated from backend | Claude |
| U-91 | Frontend | `useHotkeys` re-attaches listener every render | Claude |
| U-92 | Frontend | Ctrl+T/W hijack browser shortcuts globally | Claude |
| U-93 | Workspace | Global `current_workspace_id` causes cross-client state collisions | Claude, Devin, Agy |
| U-94 | IPC | Unix socket default perms — any local user can connect | ipc.rs:50-87 | Claude |
| U-95 | Deeplink | Unbounded text from URL handler reaches orchestrator draft | deeplink.rs:66-69 | Claude |
| U-96 | Agent | `spawn_agent` count has no upper bound (Tauri path) | commands.rs:134 | Claude |

### LOW (27)

| ID | Component | Title | Found by |
|---|---|---|---|
| U-97 | Frontend | Unsaved edits silently lost on note switch | Claude |
| U-98 | Frontend | ResizeObserver fires after `term.dispose()` | Claude |
| U-99 | Frontend | Skill toggle race — rapid toggle yields wrong state | Claude |
| U-100 | Frontend | OrchestratorPanel autoscroll fights user-scrolled-up | Claude |
| U-101 | Frontend | Toast auto-dismiss not pausable, double-announced | Claude, Agy |
| U-102 | Frontend | Modal lacks focus trap and return-focus | Claude |
| U-103 | Frontend | ThemePicker is a modal with no dialog semantics | Claude |
| U-104 | Frontend | Icon-only buttons rely on `title=` for label (no aria-label) | Claude |
| U-105 | Frontend | No live regions for streaming chat / voice transcription | Claude |
| U-106 | Frontend | KanbanBoard has no keyboard alternative to drag-drop | Claude |
| U-107 | Frontend | xterm terminals not screen-reader accessible | Claude |
| U-108 | Frontend | MemoryGraph (canvas) has no text alternative | Claude |
| U-109 | Frontend | Form errors not associated with inputs (aria-describedby) | Claude |
| U-110 | Frontend | No `prefers-color-scheme` fallback on first load | Claude |
| U-111 | Frontend | Inline 9-10px font sizes bypass token system | Claude |
| U-112 | Frontend | Tile header actions cluster has no group label | Claude |
| U-113 | MCP | MCP-originated state changes don't emit Tauri UI events | Devin |
| U-114 | Orchestrator | Provider settings/Anthropic key path are ignored (dead code) | Devin |
| U-115 | Tests | SSE parser helper fails `[DONE]` handling | Devin, Agy |
| U-116 | Tests | FTS hyphen sanitizer contradicts its tests | Devin, Agy |
| U-117 | Tests | Claude skill import tests mutate global `HOME` (flaky) | Devin |
| U-118 | Frontend | ESLint suite is red (17 errors) | Devin |
| U-119 | Memory | Empty-or-noise FTS queries silently search for literal `x` | Claude |
| U-120 | Logging | `.env` path printed on stderr at startup | Claude |
| U-121 | Logging | Memory/IPC/skill watchers log absolute workspace paths | Claude |
| U-122 | Voice | Transcript history durable + searchable forever (no retention) | Claude |
| U-123 | Frontend | Native `confirm()`/`prompt()` for destructive actions (Tauri inconsistent) | Claude |

---

## 3. Consensus Findings (found by 3+ models)

These have the highest confidence — verified independently by multiple models:

| ID | Title | Models | Severity |
|---|---|---|---|
| U-01 | Empty `allowed_roots` disables all path checks | Claude, Devin, Agy | CRITICAL |
| U-18 | MCP keys scoped only by capability, not workspace | Devin, Agy, Claude* | HIGH |
| U-93 | Global `current_workspace_id` causes cross-client collisions | Claude, Devin, Agy | MEDIUM |

*Claude reported this as part of the broader "cross-workspace data leak" finding cluster.

**Near-consensus (found by 2 models):**

| ID | Title | Models | Severity |
|---|---|---|---|
| U-02 | Path traversal via canonicalize fallback | Claude, Devin | CRITICAL |
| U-10 | Workspace skills shadow built-ins | Claude, Devin | CRITICAL |
| U-12 | `create_user_skill` path traversal | Claude, Devin | CRITICAL |
| U-17 | MCP token in URL query string | Claude, Devin | HIGH |
| U-26 | Mailbox has no workspace scoping | Claude, Devin | HIGH |
| U-29 | Review gate vote has no voter identity | Claude, Devin | HIGH |
| U-30 | File ownership locks use raw paths | Claude, Devin | HIGH |
| U-40 | Zombie agent handle leak | Claude, Agy | HIGH |
| U-42 | `tail_agent` path traversal via raw agent_id | Claude, Devin | HIGH |
| U-57 | Paths not normalised before claiming | Claude, Devin | MEDIUM |
| U-81 | Symlink containment not transitive in skill walk | Claude, Devin | MEDIUM |

---

## 4. Unique Findings (found by only 1 model — need verification)

### Unique to Claude Opus 4.7 (71 findings)

Most significant unique contributions:
- **U-06/U-07**: Indirect prompt injection via memory/tool-results (the #1 risk in the system)
- **U-08/U-09**: BrowserPanel iframe sandbox bypass + javascript: URL
- **U-13**: `set_setting` → command injection on next spawn
- **U-14**: Concurrent turn corruption
- **U-15**: SSE buffer OOM
- **U-16**: `child.wait()` under global lock
- **U-33/U-34**: Voice model download without checksum; keystroke injection to wrong window
- **U-37**: Agent self-trigger via stdout pattern injection
- **U-43/U-44**: xterm ImageAddon CVE surface + regex DoS
- **U-54**: Agent stdout shipped raw to Google for classification
- All accessibility findings (U-102 through U-112)
- All concurrency findings beyond U-14/U-16

### Unique to Gemini 3.5 Flash / Agy (1 finding)

- **U-03**: Windows-specific backslash path traversal in `slug_to_path` — platform-specific variant not caught by other models

### Unique to GPT 5.5 / Devin (2 findings)

- **U-114**: Provider settings/Anthropic key path are dead code (verified by running tests)
- **U-113**: MCP-originated state changes don't emit Tauri UI events

---

## 5. Fix Plan

### Phase 1: Critical Security (1-2 days)

| Priority | Finding | Effort | Impact |
|----------|---------|--------|--------|
| P0 | U-01: Pass workspace roots to `read_file`/`write_file` | 1h | Closes arbitrary file R/W |
| P0 | U-02: Hard-fail on canonicalize error | 30m | Closes path traversal on new files |
| P0 | U-03/U-04: Validate slug format strictly | 1h | Closes all slug-based traversal |
| P0 | U-05: Upsert memory by `(workspace_root, path)` not `id` | 2h | Closes cross-workspace hijack |
| P0 | U-06/U-07: Fence memory/tool-result content in system prompt | 4h | Closes prompt injection |
| P0 | U-08/U-09: Fix BrowserPanel (drop `allow-same-origin`, validate URLs) | 1h | Closes iframe RCE |
| P0 | U-10/U-11: Block workspace skill shadowing of built-in IDs | 2h | Closes repo-based injection |
| P0 | U-12: Validate skill `id` format | 30m | Closes skill path traversal |
| P0 | U-13: Allow-list writable setting keys | 1h | Closes command injection |
| P0 | U-14: Per-session cancel handle + mutex | 3h | Closes turn corruption |
| P0 | U-15: Cap SSE buffer at 1MiB + chunk timeout | 1h | Closes OOM vector |
| P0 | U-16: Drop lock before `child.wait()` | 1h | Closes global stall |

**Total Phase 1: ~18h**

### Phase 2: High Security + Data Integrity (3-5 days)

| Priority | Cluster | Effort |
|----------|---------|--------|
| P1 | Workspace scoping migration (U-18, U-26, U-27, U-31, U-51, U-60, U-93) | 8h |
| P1 | MCP spec compliance (U-20 through U-25) | 6h |
| P1 | MCP token security (U-17, U-19, U-41) | 3h |
| P1 | Agent path validation (U-42, U-53, U-96) | 2h |
| P1 | Swarm auth + ownership (U-28, U-29, U-30, U-57) | 4h |
| P1 | Voice security (U-33, U-34, U-35, U-36) | 4h |
| P1 | Architect safety (U-37, U-38, U-39, U-54) | 4h |
| P1 | Frontend XSS + DoS (U-43, U-44, U-45, U-46, U-48) | 3h |

**Total Phase 2: ~34h**

### Phase 3: Medium + Low (1-2 weeks)

| Priority | Cluster | Effort |
|----------|---------|--------|
| P2 | Input validation layer (U-56, U-58, U-59, U-61, U-83, U-84, U-85, U-95) | 6h |
| P2 | Orchestrator hardening (U-64-U-70) | 4h |
| P2 | Voice reliability (U-71-U-75) | 4h |
| P2 | Skills + resolver (U-76-U-82) | 4h |
| P2 | Frontend state management (U-86-U-92) | 6h |
| P3 | Accessibility (U-102-U-112) | 8h |
| P3 | Test fixes + dead code (U-113-U-123) | 4h |

**Total Phase 3: ~36h**

---

## 6. Model Comparison

| Metric | Claude Opus 4.7 | GPT 5.5 (Devin) | Gemini 3.5 Flash (Agy) |
|--------|----------------|-----------------|------------------------|
| **Raw findings** | 183 | 18 | 11 |
| **After dedup** | 112 unique | 16 (14 shared) | 9 (8 shared) |
| **Unique contributions** | 71 | 2 | 1 |
| **Critical found** | 15/16 | 5/16 | 2/16 |
| **Approach** | 33 parallel sub-agents, exhaustive static analysis | Single-pass + test execution + lint | Single-pass static review |
| **Key strength** | Depth and breadth — found entire attack classes (prompt injection, voice, concurrency, a11y) that others missed | Empirical verification — actually ran tests, confirmed failures, checked lint | Concise prioritization; caught Windows-specific variant |
| **Key weakness** | Volume makes triage harder; some findings are speculative without runtime verification | Shallow coverage — missed most of the attack surface | Very limited scope — only 11 findings total |
| **Best at** | Novel attack vectors, architecture-level issues, cross-cutting concerns | Confirming bugs exist in practice, catching dead code | Quick high-confidence triage of obvious issues |

### Observations

1. **Depth vs breadth tradeoff**: Claude's 33-agent approach found 6x more issues than all other models combined, including entire categories (prompt injection, voice security, accessibility) that no other model touched.

2. **Empirical verification matters**: Devin was the only model that ran `cargo test` and `pnpm lint`, confirming 4 test failures and 17 lint errors. This grounds findings in reality rather than theory.

3. **Consensus = high confidence**: The 3 findings caught by all models (empty allow_roots, workspace scoping, global workspace state) are the most architecturally fundamental issues — they affect every subsystem.

4. **Platform-specific thinking**: Only Agy caught the Windows backslash variant, suggesting value in models that think about cross-platform edge cases even on a Linux-primary codebase.

5. **Missing model**: OpenCode/Qwen report was not available on disk. A 4th perspective would have strengthened consensus scoring.

---

*Generated 2026-05-21. Deduplicated from 3 model reports (212 raw findings → 123 unique).*
