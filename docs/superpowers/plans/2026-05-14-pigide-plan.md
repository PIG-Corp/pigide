# PigIDE — Implementation Plan

**Spec:** [`../specs/2026-05-14-pigide-design.md`](../specs/2026-05-14-pigide-design.md)
**Order:** strict — each milestone leaves a working, demoable state. Verify before moving on.

---

## M1 · Repo & Tauri 2 scaffold (no business logic)

1. `git init`, write `.gitignore` (target/, node_modules/, dist/, .DS_Store, ~/.config/pigide).
2. Create root layout:
   ```
   /Cargo.toml          (workspace root)
   /src-tauri/          (rust crate)
   /frontend/           (vite + react + ts)
   ```
3. `frontend/`: `pnpm create vite . --template react-ts`, then `pnpm add @xterm/xterm @xterm/addon-fit allotment zustand lucide-react`.
4. `src-tauri/Cargo.toml` deps: `tauri = "2"`, `tauri-build = "2"`, `serde`, `serde_json`, `tokio = { features=["full"] }`, `portable-pty = "0.8"`, `rusqlite = { features=["bundled"] }`, `r2d2`, `r2d2_sqlite`, `reqwest = { features=["json","stream"] }`, `cpal = "0.15"`, `whisper-rs = "0.13"`, `dirs`, `uuid = { features=["v4","serde"] }`, `base64`, `tracing`, `tracing-subscriber`, `anyhow`, `thiserror`, `once_cell`, `parking_lot`, `futures`.
5. `src-tauri/tauri.conf.json` minimal: window 1280×800, background `#0e0f12`, dev URL `http://localhost:5173`, distDir `../frontend/dist`, build commands `pnpm dev`/`pnpm build`.
6. Wire `tauri-build`. Empty `main.rs` with `tauri::Builder::default().run(...)`.
7. Frontend `App.tsx`: blank page with text "PigIDE".
8. Verify: `pnpm install` in `frontend/`, `cargo build` in `src-tauri/`, `cargo run` in `src-tauri/` should open empty window (or `pnpm tauri dev` if we add the cli devDep `@tauri-apps/cli`).

**Done when:** window opens with the blank page, no panics, no warnings.

---

## M2 · Backend skeleton: state, db, error

1. `src-tauri/src/error.rs`: `pub type Result<T> = std::result::Result<T, Error>; #[derive(thiserror::Error)] pub enum Error { ... }` plus `From<...>` for sqlite/reqwest/io. `impl From<Error> for String` so commands can return `Result<T, String>`.
2. `src-tauri/src/db.rs`: r2d2 pool initialised at `~/.config/pigide/db.sqlite`. Run inline migration: tables from spec §4. Bump `user_version` PRAGMA per migration step.
3. `src-tauri/src/state.rs`: `pub struct AppState { pub db: Pool, pub agent_mgr: Arc<AgentManager>, pub orch: Arc<Orchestrator>, pub voice: Arc<VoicePipeline> }`. Stub managers with `new()`s only.
4. `main.rs`: build state, `tauri::Builder::default().manage(state)`, register a single test command `ping` that returns `"pong"`.
5. Verify: `tauri::invoke("ping")` from frontend devtools returns `"pong"`.

**Done when:** db file is created on first run, `ping` round-trips.

---

## M3 · WorkspaceManager + sidebar

1. `workspace.rs`: `WorkspaceManager` with methods `list`, `get`, `create`, `rename`, `delete`, `update_layout`, `set_paths`, `get_current`, `set_current`. All sync rusqlite (fast, ms-scale).
2. Commands: `list_workspaces`, `get_workspace`, `create_workspace`, `rename_workspace`, `delete_workspace`, `update_layout`, `set_current_workspace`.
3. Frontend `state/store.ts` — zustand slices `workspaces`, `currentId`, `layout`, `chat` (empty for now), `voice` (idle).
4. Frontend `state/ipc.ts` — wrapped `invoke` calls.
5. `WorkspaceSidebar.tsx`: list, "+" button, click selects (calls `set_current_workspace` + reloads layout), context menu rename/delete.
6. On app boot: `list_workspaces`. If empty, auto-create "default".

**Done when:** sidebar lists workspaces, creating/renaming/deleting persists across restart.

---

## M4 · AgentManager + xterm tile (single agent first)

1. `agent.rs`:
   - `AgentManager { handles: Mutex<HashMap<Uuid, AgentHandle>>, app_handle: tauri::AppHandle }`.
   - `spawn(workspace_id, agent_type, cwd) -> Agent`: open PTY, build `CommandBuilder`, spawn child, store handle, persist row, kick reader task.
   - Reader task reads up to 4 KiB → emit `agent://stdout` `{agent_id, data_b64}` via `app_handle.emit_all`. On EOF: emit `agent://exit`, mark exited.
   - `write(agent_id, data)`, `resize(agent_id, cols, rows)`, `kill(agent_id)`.
   - Drop guard kills children on app exit.
2. Commands: `spawn_agent` (supports `count`), `kill_agent`, `write_to_agent` (base64), `resize_agent`.
3. Frontend `AgentTile.tsx`:
   - `useEffect` on mount: create xterm, attach FitAddon, listen to `agent://stdout` for own id, decode b64 → `term.write`.
   - `term.onData(data => invoke('write_to_agent', { agent_id, data_b64: btoa(data) }))`.
   - ResizeObserver → `fit.fit()` → invoke `resize_agent`.
   - Header buttons (split-h, split-v, max, close) wired but only `close` and `max` functional in this milestone.
4. Temporary: bottom toolbar of tiling area has `+ kiro-cli` / `+ claude` buttons that spawn into root if empty, else replace the focused leaf for now (full tiling lands in M5).

**Done when:** click "+ kiro-cli", a tile opens with a live `kiro-cli` running. Typing works. Closing kills the process.

---

## M5 · Tiling layout (binary tree + allotment)

1. `frontend/src/layout/tree.ts` — pure functions: `insertSplit(tree, leafId, dir, newAgentId)`, `closeLeaf(tree, leafId)`, `setRatio(tree, path, ratio)`, `findFocusable(tree)`, `forEachLeaf(tree, fn)`.
2. `frontend/src/layout/render.tsx` — recursive renderer. `Split` → `<Allotment vertical={dir==='h'}>` with `<Allotment.Pane preferredSize={ratio*100+'%'}>`. `Leaf` → `<AgentTile>`. `Empty` → placeholder with "+ kiro-cli / + claude".
3. Persist layout: zustand subscribe → debounced 200 ms → `update_layout`.
4. Header buttons in `AgentTile` now call `splitLeaf(id, 'h'|'v')`. The new sibling spawns a fresh agent (same type by default; modifier-click for other type later).
5. Maximize: store `maximizedLeafId` in zustand; renderer short-circuits to that leaf when set.

**Done when:** can split tiles arbitrarily, drag dividers, close, maximize. State survives refresh (via persisted layout).

---

## M6 · Orchestrator HTTP + tools

1. `orchestrator/client.rs`: `pub struct OmniClient { base_url, model, api_key, http: reqwest::Client }`. `chat_completions(messages, tools, stream=false)` returns parsed response.
2. `orchestrator/tools.rs`:
   - `pub fn tool_definitions() -> Vec<serde_json::Value>` — exact schema from spec §8.
   - `pub async fn dispatch(state: &AppState, name: &str, args: serde_json::Value) -> Result<serde_json::Value>` — match on tool name, call corresponding manager, return JSON-friendly result.
3. `orchestrator/mod.rs`: `run_chat(workspace_id, user_text)`:
   - Load chat history from db (last 20 messages, simple cap).
   - Append user message (persist).
   - Loop: POST to OmniRouter; if `tool_calls` present, dispatch each, append `assistant` message (with tool_calls) and `tool` messages (with tool_call_id + result). If plain text, append assistant message and break.
   - Emit `chat://message` after each persistence; emit `chat://status` transitions.
4. Command: `send_chat`. It spawns a tokio task and returns immediately; events stream back.
5. Settings: store `omnirouter.base_url`, `omnirouter.model`, `omnirouter.api_key` (read at startup, hot-reload on settings command).

**Done when:** typing "Hi" in chat input gets a reply from `kr/claude-opus-4.7`. Typing "list my workspaces" triggers a tool call, results show up.

---

## M7 · Right panel UI (chat + PTT)

1. `OrchestratorPanel.tsx`:
   - Status pill (idle/thinking/tool).
   - `<ChatList>` — auto-scrolling list. Tool calls collapsed (`▸ list_workspaces` → expand to show args+result).
   - Textarea input with Enter→send, Shift+Enter newline.
   - `<VoiceButton>` — large round button, mic icon. Mousedown: `start_voice`. Mouseup: `stop_voice`. While `recording`: pulse animation. While `transcribing`: spinner.
2. Listen to `voice://transcript` → append to textarea draft (don't send).
3. Listen to `chat://message` and `chat://status` → update store.
4. Wire `send_chat` on Enter.

**Done when:** can chat with orchestrator, can hold the round button + speak (button reacts but no text yet — Whisper lands in M8).

---

## M8 · Voice pipeline (cpal + whisper-rs)

1. `voice/capture.rs`: `Capture` struct holds optional `cpal::Stream`. `start()` builds 16 kHz mono i16 input stream from default device, pushes samples (resampled if needed) into a shared `Mutex<Vec<f32>>`. `stop()` returns the buffer.
2. `voice/download.rs`: ensure `~/.cache/pigide/ggml-small.bin` exists. If not, stream from `https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin` with progress events `voice://download`.
3. `voice/whisper.rs`: lazy-init `WhisperContext` per process; `transcribe(samples) -> Result<String>` runs `WhisperState::full` on `spawn_blocking`.
4. `voice/mod.rs`: orchestrates lifecycle. Commands `start_voice` / `stop_voice` flip state, emit `voice://state`, on stop: `transcribe` → emit `voice://transcript`.
5. Frontend: handle `voice://download` for first-run progress UI in the panel header.

**Done when:** hold button, say "create a workspace called demo", release → text appears in input. User edits if needed and presses Enter.

---

## M9 · End-to-end flow & polish

1. End-to-end test scenario (manual checklist):
   - Fresh app boot.
   - Voice: "Создай новый workspace под названием test, запусти в нём 12 kiro cli" → orchestrator calls `create_workspace` + `switch_workspace` + `spawn_agent count=12`.
   - Tiling area shows 12 live `kiro-cli` tiles in a roughly 4×3 grid.
   - Each tile is interactive.
   - Close all → workspace empty.
2. Polish:
   - Dark theme tweaks, focus ring, keyboard shortcut `Ctrl+Shift+K` for orchestrator focus.
   - Toast component for errors.
   - README.md with run instructions.
3. `cargo tauri build` (or `pnpm tauri build`) produces a `.deb` / AppImage. Smoke-launch the binary.

**Done when:** verification commands all pass and the demo scenario works.

---

## Verification commands (per milestone)

| Milestone | Command |
|-----------|---------|
| M1 | `cd frontend && pnpm install && cd ../src-tauri && cargo build` |
| M2 | `cargo test -p pigide --lib`, plus manual `ping` |
| M3 | manual: create/rename/delete a workspace, restart, see persistence |
| M4 | manual: spawn kiro-cli, type, see output, close it |
| M5 | manual: split, drag, close; reload, layout persists |
| M6 | manual: chat with orch, tool call works |
| M7 | manual: PTT button responds (no text yet) |
| M8 | manual: PTT → text drafts |
| M9 | full demo scenario; `cargo tauri build` succeeds |

## Risks & mitigations

| Risk | Mitigation |
|------|------------|
| `kiro-cli` or `claude` may behave oddly under non-tty env vars | set `TERM=xterm-256color`, inherit `HOME`, `PATH`, `LANG`; allow per-agent env override later |
| Whisper model download slow / blocked on first run | show progress, allow user to point to a pre-downloaded file via settings |
| OmniRouter auth changes (saw `AUTH_001` on `/api/health`) | `/v1/*` worked unauth; if auth needed, settings `api_key` is sent as Bearer |
| 12 PTYs * stdout streaming = lots of events | event payloads are base64 chunks; keep <= 8 KiB; coalesce reads if needed |
| webkit2gtk-4.1 only (no 4.0) | Tauri 2 already targets 4.1, fine |
| sqlite contention from many agents writing status | only writes on spawn / kill — minimal |
