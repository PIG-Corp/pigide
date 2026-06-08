# PigIDE Audit - Devin

## Executive summary

This audit reviewed the PigIDE Rust/Tauri backend, React frontend, MCP server, agent runtime, workspace/task/swarm systems, memory system, and skills subsystem. Socraticode was unavailable (`fetch failed`), so findings are based on local static review plus verification commands.

### Severity summary

| Severity | Count | Main risk |
|---|---:|---|
| High | 5 | Host filesystem access, powerful MCP token exposure, workspace authorization gaps, user-skill path traversal, untrusted prompt injection |
| Medium | 8 | Sensitive argument logging, path traversal variants, cross-workspace state integrity gaps, ownership bypasses, ignored provider settings |
| Low | 5 | UI desync, red tests/lint, dead parser/helper behavior |
| Total | 18 | Multiple security boundaries need tightening before production use |

### Verification results

| Check | Result | Notes |
|---|---|---|
| `cargo test --manifest-path ./src-tauri/Cargo.toml --no-default-features --features custom-protocol` | Failed | 258 passed, 4 failed: FTS hyphen tests, SSE parser test, Claude skill import idempotency test |
| `pnpm --dir ./frontend build` | Passed | Build succeeds; Vite reports a large chunk warning for `index-*.js` |
| `pnpm --dir ./frontend lint` | Failed | 17 ESLint errors across memory/skills/voice/settings components |
| Secret-pattern scan with `rg` | Passed | No matches for common API key/private key patterns outside ignored build artifacts |

## Findings

| ID | Severity | Component | Title | Description | File:Line | Fix recommendation |
|---|---|---|---|---|---|---|
| SEC-01 | High | MCP launcher/auth | Auto-minted tile MCP token is plaintext, full-scope, and placed in URL query | Claude tile auto-registration creates or reuses a `tile-claude` API key with `read`, `mutate`, and `dangerous` scopes, stores the plaintext in `settings.mcp.tile_token`, and sends it both as `Authorization` and as `?apiKey=` in the server URL. Query-string secrets are likely to leak through process args, local configs, logs, screenshots, and copied `.mcp.json` files. The server also accepts query tokens. | `src-tauri/src/mcp/launcher.rs:46-60`, `src-tauri/src/mcp/launcher.rs:63-80`, `src-tauri/src/mcp/server.rs:176-183` | Stop putting tokens in URLs. Use headers only, store only a hash or OS-keychain reference for auto-registered tokens, rotate/revoke old `tile-claude` keys, and mint least-privilege per-tile/session keys instead of `dangerous` by default. |
| SEC-02 | High | MCP authorization | MCP keys are scoped only by capability, not workspace | The MCP server checks only coarse scopes (`read`, `mutate`, `dangerous`). Tools can still target arbitrary `workspace_id`, `task_id`, `agent_id`, or global state. A client with a mutating key can operate across all workspaces, not just the workspace that spawned or owns it. | `src-tauri/src/mcp/server.rs:321-330`, `src-tauri/src/orchestrator/tools.rs:528-559`, `src-tauri/src/orchestrator/tools.rs:614-623` | Add workspace binding to MCP keys or request context, then enforce target-workspace checks in dispatch before calling tools. Reject cross-workspace task, memory, swarm, and agent operations unless the key explicitly has global admin scope. |
| SEC-03 | Medium | MCP audit logging | Full tool arguments are persisted in MCP audit rows | Every MCP tool call stores `args_json` verbatim. Arguments can contain user prompts, memory bodies, file paths, `send_to_agent` text, project aliases, and potentially pasted secrets. This creates a long-lived sensitive-data store in SQLite. | `src-tauri/src/mcp/server.rs:383-394`, `src-tauri/src/orchestrator/tools.rs:423-452` | Redact or hash sensitive fields before audit insert. Store metadata (`tool`, `key_id`, status, size, redacted-field names) rather than full arguments; add retention/clear policy. |
| SEC-04 | High | Tauri file commands | `read_file` and `write_file` bypass workspace roots | The file helper supports `allowed_roots`, but the exposed Tauri commands pass an empty slice. `validate_path` returns any absolute path unchanged when roots are empty, so any renderer compromise or overly-powerful frontend path can read/write arbitrary host files. | `src-tauri/src/commands.rs:812-824`, `src-tauri/src/files.rs:71-83` | Resolve the active workspace roots and pass canonicalized roots into file helpers. Treat empty `allowed_roots` as deny-by-default for exposed commands; add symlink-safe parent canonicalization for writes. |
| SEC-05 | High | Skills commands | `create_user_skill` can write outside the user skills directory | `create_user_stub` joins `~/.pigide/skills/{id}.md` without validating `id`. A crafted `id` containing `/`, `..`, or an absolute-path prefix can escape the intended directory before writing. Frontmatter validation happens only later when skills are parsed, not before file creation. | `src-tauri/src/commands.rs:1451-1462`, `src-tauri/src/skills/tools.rs:103-123` | Reuse the skill id validator before constructing the path, reject path separators and dot segments, canonicalize the final parent, and assert it remains under `default_user_dir()`. |
| SEC-06 | Medium | Agent logs | Agent log tail paths are built from raw `agent_id` | `agent_log_path`, `agent_log_tail`, and the orchestrator `tail_agent` tool build paths with `format!("{}.log", agent_id)` and no id validation. `agent_id` values with path separators or absolute prefixes can read unintended `.log` paths outside the agents directory. | `src-tauri/src/agent.rs:23-29`, `src-tauri/src/agent.rs:282-296`, `src-tauri/src/commands.rs:218-227`, `src-tauri/src/orchestrator/tools.rs:497-515` | Validate `agent_id` as UUID or look it up in the `agents` table before reading logs. Canonicalize the log path and assert it starts with the agents log directory. |
| SEC-07 | Medium | Skills registry | Symlink escape defense is ineffective in skill walker | The skill registry intends to reject symlinks escaping the root, but it calls `entry.metadata()`, which follows symlinks. As a result, `meta.file_type().is_symlink()` is normally false for symlink targets, allowing traversal into symlinked directories or files outside the configured skill root. | `src-tauri/src/skills/registry.rs:311-343` | Use `entry.file_type()` or `symlink_metadata()` to detect symlinks before following them. Canonicalize every visited path and skip any target outside the canonical root. Add tests for symlinked files and directories. |
| SEC-08 | High | Skills/prompt trust boundary | Untrusted workspace skills can shadow built-ins and inject system prompt content | Workspace skills are loaded from `<workspace>/.pigide/skills`, have higher precedence than user and built-in skills, and are composed directly into the system prompt. Opening an untrusted repository can therefore alter agent behavior or shadow trusted skills without an explicit trust prompt. | `src-tauri/src/skills/skill.rs:31-45`, `src-tauri/src/skills/registry.rs:366-370`, `src-tauri/src/skills/composer.rs:178-188`, `src-tauri/src/lib.rs:178-186` | Treat workspace skills as untrusted by default. Require explicit per-workspace enablement, show source/path in UI before activation, prevent silent shadowing of built-ins, and consider sandboxing or signing trusted skills. |
| AUTH-01 | Medium | Swarm review gates | Any caller can vote any review gate by id | `vote_review_gate` updates a gate solely by `gate_id`; it does not verify that the caller is the assigned `reviewer_id` or even that the reviewer exists. Any tool caller with mutate scope can pass or fail any review gate. | `src-tauri/src/swarm/review.rs:84-101`, `src-tauri/src/swarm/tools.rs:281-285` | Include caller identity in tool context and require it to match `reviewer_id` or an admin role. Also validate gate/task workspace against the caller's workspace scope. |
| AUTH-02 | Medium | Tasks | Task parent and assignment checks ignore workspace boundaries | Task creation verifies that a parent exists but not that the parent is in the same workspace. Assignment verifies that an agent exists but not that the agent belongs to the same workspace as the task. This can corrupt task hierarchy and leak work between workspaces. | `src-tauri/src/tasks.rs:104-112`, `src-tauri/src/tasks.rs:281-291` | When creating a child task, query the parent's `workspace_id` and require equality. When assigning an agent, query both task and agent workspace ids and reject mismatches. |
| STATE-01 | Medium | Workspace/session state | Global `current_workspace_id` causes cross-client state collisions | Current workspace is stored as a single global setting. MCP clients, frontend sessions, and orchestrator turns all read/write the same value. Concurrent clients can switch each other's workspace context, causing memory/tool operations to execute in the wrong workspace. | `src-tauri/src/orchestrator/mod.rs:166-170`, `src-tauri/src/orchestrator/tools.rs:316-323`, `src-tauri/src/memory/tools.rs:211-214` | Make workspace context explicit per chat session/MCP key/request. Avoid global mutable routing state for operations that can be triggered concurrently. |
| INT-01 | Medium | Swarm ownership | File ownership locks use raw path strings | File locks are keyed by the unnormalized `path` argument. Equivalent paths such as `src/a.rs`, `./src/a.rs`, absolute paths, case variants on case-insensitive filesystems, or symlinked paths can bypass exclusive ownership. | `src-tauri/src/swarm/ownership.rs:31-61`, `src-tauri/src/swarm/tools.rs:231-251` | Normalize paths relative to a canonical workspace root before lock insert/release. Store canonical workspace-relative paths and reject paths outside the workspace. |
| CONF-01 | Medium | LLM provider config | Provider settings and Anthropic key path are ignored | Provider constants and Anthropic key resolution exist, but `build_provider` hardcodes OmniRouter at `DEFAULT_OMNI_BASE` and `DEFAULT_OMNI_MODEL` with no API key. The test `provider_ignores_settings` locks this behavior in. UI/settings for provider, model, and API keys are therefore misleading or dead. | `src-tauri/src/orchestrator/providers/mod.rs:86-119`, `src-tauri/src/orchestrator/providers/mod.rs:147-152` | Implement provider selection from settings/env with validation and migration. Update tests to assert configured provider/model/key precedence instead of ignored settings. |
| STATE-02 | Low | MCP/UI integration | MCP-originated state changes do not emit Tauri UI events | MCP dispatch calls orchestrator tools with `None` for `AppHandle`, so state-changing tools such as workspace switches, layout changes, and agent spawns do not emit the events the frontend relies on for live updates. UI can desynchronize until manual refresh/reload. | `src-tauri/src/mcp/server.rs:351-359`, `src-tauri/src/orchestrator/tools.rs:305-308`, `src-tauri/src/orchestrator/tools.rs:382-388` | Pass an event bridge or `AppHandle` into MCP state, or enqueue UI refresh events after successful MCP mutations. Add tests for MCP-triggered workspace/layout updates. |
| REL-01 | Low | SSE parser/tests | Dead SSE parser helper fails `[DONE]` handling | Production streaming uses a labeled outer break, but the test-only `parse_sse_payload` helper breaks only the inner loop on `[DONE]`, so it parses data after the sentinel. Cargo test fails at `parse_sse_tolerates_done_keepalive_and_malformed`. | `src-tauri/src/orchestrator/client.rs:305-323`, `src-tauri/src/orchestrator/client.rs:417-420` | Either remove the unused helper or make it share the production parser logic with a labeled break. Keep the existing regression test. |
| REL-02 | Low | Memory search/tests | FTS hyphen sanitizer contradicts its tests | `sanitize_fts_query` replaces `-` with spaces before filtering tokens that start with `-`, so negative tokens survive as normal terms. Two unit tests fail: expected `-test` to become `x` and `hello -world --backend` to become `hello`. | `src-tauri/src/memory/service.rs:564-587`, `src-tauri/src/memory/service.rs:608-617` | Preserve leading hyphen until token filtering or filter raw whitespace tokens before punctuation stripping. Decide whether hyphenated words are search terms or negation and encode that contract in tests. |
| REL-03 | Low | Skills import/tests | Claude skill import tests mutate global `HOME` and are flaky | Import tests call `std::env::set_var("HOME", ...)` and restore it at the end. Rust tests run concurrently, so other tests using `dirs::home_dir()` can race. The observed failure was `Updated` instead of `Unchanged` on the second import. | `src-tauri/src/skills/claude_import.rs:675-702`, `src-tauri/src/skills/claude_import.rs:717-720` | Inject the import destination or home path into import functions for tests. If env mutation remains, guard all such tests with a global mutex and restore state via RAII even on panic. |
| VERIFY-01 | Low | Frontend quality gate | ESLint suite is red | `pnpm --dir frontend lint` reports 17 errors, including React 19 `set-state-in-effect`, `no-explicit-any` in `MemoryGraph`, and empty catch blocks. This blocks a clean production quality gate even though the build passes. | `frontend/src/components/MemoryGraph.tsx:43`, `frontend/src/components/MemoryGraph.tsx:82-94`, `frontend/src/components/SettingsButton.tsx:51`, `frontend/src/hooks/useInputHistory.ts:12` | Fix lint errors or intentionally adjust the ESLint policy. Keep `pnpm lint` in CI and require it before release. |

## Fix plan

### Phase 1 - Immediate security boundary fixes

1. **Harden MCP token handling**
   - Remove `apiKey` query-string support from generated configs; keep header-only auth.
   - Stop persisting plaintext bearer tokens in SQLite settings.
   - Rotate existing `tile-claude` keys and add a migration/cleanup path.
   - Default auto-registered tile keys to least privilege; require explicit escalation for `dangerous` tools.

2. **Constrain filesystem access**
   - Make `files::validate_path` deny empty `allowed_roots` for exposed commands.
   - Pass current workspace roots to `read_file`, `write_file`, and `walk_files`.
   - Canonicalize existing files and parent directories for writes to prevent symlink/TOCTOU escapes.
   - Add regression tests for absolute paths, `..`, symlinks, and non-existent write targets.

3. **Validate path-like identifiers**
   - Enforce UUID format for agent log reads or require DB lookup before accessing logs.
   - Reuse skill id validation before `create_user_stub` writes any file.
   - Canonicalize final paths and assert they remain under intended base directories.

4. **Gate workspace skills**
   - Add an explicit trust toggle for workspace skills.
   - Disable workspace skill shadowing by default, or require user confirmation when a workspace skill shadows a built-in or user skill.
   - Fix symlink detection in the skill walker.

### Phase 2 - Authorization and state model

1. **Workspace-scoped MCP authorization**
   - Add workspace bindings to `mcp_api_keys` or to the MCP session context.
   - Enforce workspace checks for task, memory, agent, swarm, and project tools.
   - Add tests for attempts to mutate a different workspace.

2. **Make active workspace session-scoped**
   - Replace global `current_workspace_id` for agent/MCP operations with explicit workspace parameters or per-session context.
   - Keep UI convenience state separate from backend authorization/routing decisions.

3. **Enforce task and review ownership**
   - Validate parent task workspace equality.
   - Validate task-agent workspace equality on assignment.
   - Require reviewer identity or role authorization when voting review gates.

4. **Normalize swarm file locks**
   - Resolve paths against canonical workspace roots.
   - Store canonical workspace-relative paths in `file_ownership`.
   - Add duplicate-equivalence tests (`./`, absolute, symlink, case variants where applicable).

### Phase 3 - Correctness, config, and UX consistency

1. **Provider configuration**
   - Implement settings/env-driven provider selection.
   - Validate API key/base/model settings before use.
   - Replace `provider_ignores_settings` with tests for configured behavior and fallback behavior.

2. **MCP UI event bridge**
   - Give MCP dispatch an event emitter or post-mutation notification path.
   - Verify frontend state updates after MCP `open_project`, `spawn_agent`, and `switch_workspace`.

3. **Fix failing Rust tests**
   - Align FTS sanitizer behavior with tests.
   - Remove or fix the dead SSE parser helper.
   - Refactor Claude import tests to avoid global environment races.

4. **Fix frontend quality gate**
   - Address the 17 ESLint errors.
   - Re-run `pnpm --dir frontend lint` and `pnpm --dir frontend build`.
   - Consider code-splitting for the 1.6 MB minified bundle warning.

### Phase 4 - Release verification

Run and require the following before marking the project production-ready:

```bash
cargo test --manifest-path ./src-tauri/Cargo.toml --no-default-features --features custom-protocol
pnpm --dir ./frontend lint
pnpm --dir ./frontend build
rg -n "(?i)(api[_-]?key|secret|token|password)\s*[:=]\s*['\"][^'\"]{8,}|pk_[A-Za-z0-9_-]{20,}|ghp_[A-Za-z0-9_]{20,}|AKIA[0-9A-Z]{16}|BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY" /home/camer/pigide -g '!target' -g '!frontend/node_modules' -g '!frontend/dist'
```

