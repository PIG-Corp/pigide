# PIGIDE Project Audit Report

This report presents the findings of a comprehensive, no-holds-barred security, logic, and architectural audit of the `pigide` codebase. The audit covers the Rust Tauri backend (`src-tauri/src`), the TypeScript/React frontend (`frontend/src`), the MCP protocol layer, memory system, task system, and agent management.

---

## 1. Executive Summary

The audit analyzed security boundaries, race conditions, error handling, logic consistency, and UX flows. The findings are categorized by severity levels:

*   **CRITICAL**: Vulnerabilities that allow unauthorized filesystem access or arbitrary execution across systems (e.g., path traversal).
*   **HIGH**: Major security bypasses, race conditions that cause resource leaks, and critical validation gaps.
*   **MEDIUM**: Logic errors that break essential flows or cause multi-client state collisions.
*   **LOW**: Flaky tests, minor UX issues, and dead code.

### Findings by Severity

| Severity | Count | Status / Impact |
| :--- | :---: | :--- |
| **CRITICAL** | **1** | Platform-specific path traversal on Windows (exposing host files) |
| **HIGH** | **3** | Workspace authorization bypass, Tauri file command root validation bypass, and zombie agent handle leaks |
| **MEDIUM** | **2** | Multi-client workspace state collisions, and SSE parser loop control bug |
| **LOW** | **5** | FTS5 hyphen token strip error, flaky concurrent tests (env mutations), toast queue block, and dead/unreachable code |
| **Total** | **11** | |

---

## 2. Detailed Findings

| ID | Severity | Component | Title | Description | File:Line | Fix Recommendation |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **SEC-01** | **CRITICAL** | Backend (Memory) | Platform-Specific Path Traversal | `slug_to_path` splits the `slug` exclusively by `/`. If a Windows separator `\` is passed (e.g., `..\..\etc\passwd`), it is not split on Unix, but on Windows platforms, the backslashes are resolved as directory separators by `PathBuf::push`. This permits escaping out of the `.pigmemory/` root on Windows. | [src-tauri/src/memory/storage.rs:35-45](file:///home/camer/pigide/src-tauri/src/memory/storage.rs#L35-L45) | Replace `slug.split('/')` with a platform-independent splitting check, or normalize/replace all backslashes with forward slashes before splitting. |
| **SEC-02** | **HIGH** | Backend (MCP) | Workspace ID Authorization Bypass | API keys only validate coarse scopes (`read`, `mutate`, `dangerous`). Mutating tools like `create_task`, `claim_files`, `release_files`, etc., accept an arbitrary `workspace_id` parameter without verifying that the authenticated key is authorized to access or modify that specific workspace. Any client key with the `mutate` scope can operate on any workspace's state. | [src-tauri/src/mcp/server.rs:321-330](file:///home/camer/pigide/src-tauri/src/mcp/server.rs#L321-L330) and [src-tauri/src/orchestrator/tools.rs:528](file:///home/camer/pigide/src-tauri/src/orchestrator/tools.rs#L528) | Bind API keys to specific workspace IDs in the database (`mcp_api_keys` table) and assert key-to-workspace mapping during tool dispatch. |
| **SEC-03** | **HIGH** | Backend (Files) | Tauri Command Allowed Roots Bypass | The Tauri commands `read_file` and `write_file` invoke the file helpers passing an empty slice `&[]` as `allowed_roots`. When `allowed_roots` is empty, `validate_path` returns `Ok(p.to_path_buf())` early, skipping boundary checks. A compromised frontend can read/write arbitrary host files. | [src-tauri/src/commands.rs:812-825](file:///home/camer/pigide/src-tauri/src/commands.rs#L812-L825) and [src-tauri/src/files.rs:71-84](file:///home/camer/pigide/src-tauri/src/files.rs#L71-L84) | Enforce active workspace path boundaries in Tauri commands by resolving the current workspace path and passing it as `allowed_roots`. |
| **CONC-01** | **HIGH** | Backend (Agent) | Zombie Agent Handle Leak | The PTY reader thread is spawned *before* the agent's handle is inserted into `self.handles`. If the child process exits immediately (e.g., invalid directory or execution failure), the reader thread completes and executes `handles.lock().remove(&agent_id)` before the main thread does `handles.lock().insert(...)`. This leaves a dead runtime handle in the map indefinitely, resulting in a zombie agent state. | [src-tauri/src/agent.rs:716-764](file:///home/camer/pigide/src-tauri/src/agent.rs#L716-L764) | Insert the handle placeholder into `self.handles` *before* spawning the reader thread. |
| **CONC-02** | **MEDIUM** | Backend (Workspace) | Global Workspace State Collisions | Current workspace is stored as a global database setting `current_workspace_id`. Simultaneous MCP connections from different agent instances will overwrite each other's active workspaces, leading to state/memory isolation leakage. | [src-tauri/src/db.rs](file:///home/camer/pigide/src-tauri/src/db.rs) and [src-tauri/src/memory/tools.rs](file:///home/camer/pigide/src-tauri/src/memory/tools.rs) | Transition away from global DB state settings for active workspace routing; pass workspace IDs explicitly with every client session context. |
| **LOG-01** | **MEDIUM** | Backend (SSE) | SSE Parser Done Loop Escape Bug | When `[DONE]` sentinel is encountered, the code executes `break`. This only breaks the inner line-processing loop. The outer chunk-processing loop continues, executing and accumulating subsequent data payloads (e.g. `NOPE`), failing the test `parse_sse_tolerates_done_keepalive_and_malformed`. | [src-tauri/src/orchestrator/client.rs:322](file:///home/camer/pigide/src-tauri/src/orchestrator/client.rs#L322) | Use a labeled break (`'outer: while ...`) or set a boolean flag to break the outer loop. |
| **LOG-02** | **LOW** | Backend (Memory) | FTS5 Query Sanitizer Hyphen Bug | The hyphen character `-` is replaced by a space during character mapping. Because of this, the filter `.filter(|s| !s.starts_with('-'))` is never triggered, allowing hyphenated terms to bypass removal (though safely escaped as spaces, it fails test assertions expecting term removal). | [src-tauri/src/memory/service.rs:566](file:///home/camer/pigide/src-tauri/src/memory/service.rs#L566) | Perform the starts-with hyphen check on tokens before stripping non-alphanumeric characters, or allow hyphens in the alphanumeric whitelist and strip leading hyphens in the token filter. |
| **LOG-03** | **LOW** | Backend (Skills) | Thread-Unsafe Environment Mutation | The test alters the global `HOME` environment variable via `std::env::set_var`. Since Cargo runs tests concurrently by default, this creates a race condition with other tests that read `dirs::home_dir()`, causing flaky test failures. | [src-tauri/src/skills/claude_import.rs:682](file:///home/camer/pigide/src-tauri/src/skills/claude_import.rs#L682) | Introduce a locking mechanism to serialize tests that mutate env variables, or refactor `import` to accept an explicit home path. |
| **UX-01** | **LOW** | Frontend | Toast Auto-Dismiss Queue Block | Auto-dismiss logic only handles `toasts[0]` at a time, resulting in sequential dismissal (4 seconds per toast) rather than independent timers. | [frontend/src/App.tsx:210-216](file:///home/camer/pigide/frontend/src/App.tsx#L210-L216) | Track independent timers per toast id or delegate dismissal logic to individual Toast component instances. |
| **ARCH-01** | **LOW** | Backend (SSE) | Dead SSE Parser Function | `parse_sse_payload` is defined in `client.rs` but is not used in the production binary path, producing unused compiler warning. | [src-tauri/src/orchestrator/client.rs:305](file:///home/camer/pigide/src-tauri/src/orchestrator/client.rs#L305) | Either clean up the unused function or expose/integrate it within public client features if it is planned for future use. |
| **ARCH-02** | **LOW** | Backend (Tasks) | Dead Task Deletion Interface | `delete_task` is listed in mutating scopes check, but is absent from tool definitions and execution dispatch, making it unreachable by agents. | [src-tauri/src/mcp/server.rs:50](file:///home/camer/pigide/src-tauri/src/mcp/server.rs#L50) and [src-tauri/src/orchestrator/tools.rs](file:///home/camer/pigide/src-tauri/src/orchestrator/tools.rs) | Expose the tool schema for `delete_task` and dispatch it to `task_mgr.delete` in `orchestrator/tools.rs`. |

---

## 3. Fix Plan & Implementation Roadmap

The proposed remediation strategy is divided into three logical phases based on severity, risk, and impact.

### Phase 1: Security & Boundary Reinforcement (Critical/High)
*Focus: Close path traversal vulnerabilities and enforce strict workspace security borders.*

1.  **Resolve Windows Path Traversal (`SEC-01`)**
    *   *Approach*: Refactor `slug_to_path` to split slugs on both `/` and `\`, or normalize separators to `/` before checking for traversal segments (`..` and `.`):
        ```rust
        pub fn slug_to_path(root: &std::path::Path, slug: &str) -> PathBuf {
            let mut p = root.to_path_buf();
            let normalized = slug.replace('\\', "/");
            for seg in normalized.split('/') {
                if seg == ".." || seg == "." || seg.is_empty() {
                    continue;
                }
                p.push(seg);
            }
            p.set_extension("md");
            p
        }
        ```
    *   *Effort Estimate*: **0.5 hours**
    *   *Priority*: **1**

2.  **Bind API Keys to Workspace Scope (`SEC-02`)**
    *   *Approach*: Update the `mcp_api_keys` database table to include a `workspace_id` column. Modify key creation and validation to return this scope, and update the MCP tool dispatcher to reject requests where the target `workspace_id` does not match the key's bound workspace.
    *   *Effort Estimate*: **2 hours**
    *   *Priority*: **2**

3.  **Enforce Workspace Boundary Validation in Tauri Commands (`SEC-03`)**
    *   *Approach*: Modify `read_file` and `write_file` commands in `src-tauri/src/commands.rs` to fetch the current active workspace directory path and supply it to `validate_path`'s `allowed_roots` parameter instead of passing an empty slice `&[]`.
    *   *Effort Estimate*: **1.5 hours**
    *   *Priority*: **3**

---

### Phase 2: Logic, Concurrency & State Correctness (High/Medium)
*Focus: Eliminate race conditions in agent spawning, data corruption risks, and SSE parser failures.*

1.  **Fix Agent Handle Race Condition (`CONC-01`)**
    *   *Approach*: In `agent.rs:spawn_internal`, insert the `AgentRuntime` placeholder handle into the `self.handles` map *before* spawning the reader thread. That way, if the child exits immediately, the reader thread can successfully remove it, avoiding memory leaks.
    *   *Effort Estimate*: **1 hour**
    *   *Priority*: **4**

2.  **Transition Workspace ID from Global State to Session-scoped Context (`CONC-02`)**
    *   *Approach*: Refactor the database structure to avoid relying on a single global `current_workspace_id` setting for agent operations. Pass the contextually correct `workspace_id` explicitly inside tool arguments or session headers.
    *   *Effort Estimate*: **3 hours**
    *   *Priority*: **5**

3.  **Fix SSE Parser Outer Loop Break (`LOG-01`)**
    *   *Approach*: Refactor the SSE parser loop in `src-tauri/src/orchestrator/client.rs` using a labeled loop so the `[DONE]` check escapes the entire parsing loop:
        ```rust
        'outer: while let Some(end) = buf.find("\n\n") {
            let event_block: String = buf.drain(..end + 2).collect();
            for line in event_block.lines() {
                ...
                if data == "[DONE]" {
                    break 'outer;
                }
                ...
            }
        }
        ```
    *   *Effort Estimate*: **0.5 hours**
    *   *Priority*: **6**

---

### Phase 3: Reliability, UX & Clean-up (Low)
*Focus: Fix flaky unit tests, optimize the toast notifications, and delete unreachable/dead code.*

1.  **Fix FTS5 Sanitizer Hyphen Bug (`LOG-02`)**
    *   *Approach*: Refactor `sanitize_fts_query` to perform token filtering on hyphens before replacing non-alphanumeric characters, restoring the expected test behavior:
        ```rust
        fn sanitize_fts_query(q: &str) -> String {
            let toks: Vec<String> = q
                .split_whitespace()
                .filter(|s| !s.starts_with('-'))
                .map(|s| {
                    s.chars()
                        .filter(|c| c.is_alphanumeric() || *c == '_')
                        .collect::<String>()
                        .to_lowercase()
                })
                .filter(|s| s.len() >= 2)
                .collect();
            if toks.is_empty() {
                "x".to_string()
            } else {
                toks.join(" OR ")
            }
        }
        ```
    *   *Effort Estimate*: **0.5 hours**
    *   *Priority*: **7**

2.  **De-flake Environment Variable Unit Tests (`LOG-03`)**
    *   *Approach*: Avoid global env mutation in concurrent test suites. Inject home directory paths explicitly into the `import` function logic or serialize tests utilizing a lazy static mutex to protect the `HOME` variable.
    *   *Effort Estimate*: **1 hour**
    *   *Priority*: **8**

3.  **Upgrade Toast Auto-Dismiss Timer Logic (`UX-01`)**
    *   *Approach*: Modify the frontend toast component inside `frontend/src/App.tsx` to handle auto-dismiss timers independently for each toast ID (or delegate timeout triggers to individual Toast sub-components).
    *   *Effort Estimate*: **1 hour**
    *   *Priority*: **9**

4.  **Expose or Remove Dead Code / Tools (`ARCH-01`, `ARCH-02`)**
    *   *Approach*: Properly expose `delete_task` in `orchestrator/tools.rs` to allow agent orchestration, and clean up or verify usage of `parse_sse_payload`.
    *   *Effort Estimate*: **1 hour**
    *   *Priority*: **10**
