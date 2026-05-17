# PigIDE — Design Spec

**Date:** 2026-05-14
**Status:** Approved (brainstorming complete)
**Goal:** Desktop IDE that hosts multiple interactive CLI agents (Kiro CLI, Claude Code) as tiled terminal panes, plus a voice/text orchestrator (LLM via OmniRouter) that can manage workspaces and agents on natural-language command.

This is a from-scratch replacement for the user's previous "BridgeMind" prototype.

---

## 1. High-level architecture

```
┌────────────────────────────────────────────────────────────────────┐
│                       PigIDE (Tauri 2 desktop)                     │
├──────────────────┬──────────────────────────────┬──────────────────┤
│   FRONTEND       │   FRONTEND                   │   FRONTEND       │
│   Workspaces     │   Tiling area (xterm.js × N) │   Voice + Chat   │
│   sidebar (left) │   binary-split tree          │   panel (right)  │
│  React + Vite + TypeScript + zustand + allotment + xterm.js        │
├──────────────────┴──────────────────────────────┴──────────────────┤
│                         RUST CORE (Tauri backend)                  │
│  WorkspaceManager  │  AgentManager (PTY)  │  Orchestrator + Voice  │
└────────────────────────────────────────────────────────────────────┘
                              │
                ┌─────────────┼─────────────┐
                ▼             ▼             ▼
           kiro-cli      claude         OmniRouter
           (PTY)         (PTY)          http://localhost:20128/v1
```

**Stack:**
- Tauri 2 (Rust backend + WebView frontend)
- Frontend: React 18 + TypeScript + Vite, `allotment` (split panes), `@xterm/xterm` + `@xterm/addon-fit`, `zustand` (state), `lucide-react` (icons)
- Rust: `portable-pty` (cross-platform PTY), `rusqlite` + `r2d2` (sqlite), `reqwest` (HTTP), `tokio` (async), `serde`/`serde_json`, `cpal` (audio capture), `whisper-rs` (Whisper.cpp bindings), `dirs` (config paths), `uuid`, `tracing`

**Cross-cutting principles:**
- Backend owns all stateful runtime (PTY, sqlite, audio, HTTP).
- Frontend is pure view + controllers, communicating via `tauri::invoke` (RPC) and `tauri::Event` (streams).
- One Tauri event channel per agent: `agent://<id>/stdout` carries raw bytes (base64) chunks straight into xterm.js.
- All free / local / open-source dependencies. No cloud STT/TTS. Orchestrator goes through user's already-running OmniRouter.

---

## 2. UI layout

Three resizable vertical panes (allotment top-level):

| Pane | Size (default) | Contents |
|------|----------------|----------|
| Left | 220 px | `<WorkspaceSidebar>`: list of workspaces, "+ New" button, context menu (rename, delete) |
| Center | flex | `<TilingArea>`: recursive split tree of `<AgentTile>`s |
| Right | 360 px | `<OrchestratorPanel>`: chat history + input textarea + push-to-talk button |

Tile header (per agent):
```
┌─────────────────────────────────────────────────────────┐
│ [icon] kiro-cli #3 · "feature-x"   [⇣][⇆][⛶][✕]         │
├─────────────────────────────────────────────────────────┤
│ <xterm.js>                                              │
│ ...                                                     │
└─────────────────────────────────────────────────────────┘
```

Header buttons:
- `⇣` split horizontal (new sibling below)
- `⇆` split vertical (new sibling right)
- `⛶` maximize (zoom this leaf to fill the tiling area; toggle)
- `✕` close (kill PTY, remove leaf, simplify tree)

Bottom bar in tiling area: "+ kiro-cli", "+ claude" buttons that add a new tile by splitting the focused leaf.

Right panel layout (top-to-bottom):
1. Status indicator (orchestrator idle / thinking / tool-running).
2. Chat history (scrollable; user messages right, assistant left, tool calls collapsed).
3. Input textarea (multi-line, Enter sends, Shift+Enter newline).
4. Big round push-to-talk button (`<button>` with mic icon, ≥ 96 px, hold to record, release to transcribe).

---

## 3. Domain model

```
Workspace
  id: UUID
  name: string
  created_at: ISO-8601
  layout: LayoutNode (JSON tree)
  agents: Agent[]
  chat: ChatMessage[]
  paths: string[]   // optional folder hints (passed as cwd if any)

Agent
  id: UUID
  workspace_id: UUID
  type: "kiro-cli" | "claude"
  cwd: string?      // resolved working dir
  pid: i32?         // populated at runtime
  status: "starting" | "running" | "exited"
  created_at: ISO-8601

LayoutNode = Leaf | Split
  Leaf  = { type: "leaf",  agent_id: UUID }
  Split = { type: "split", direction: "h"|"v", ratio: f32 (0.05..0.95), a: LayoutNode, b: LayoutNode }

ChatMessage
  id: UUID
  workspace_id: UUID
  role: "user" | "assistant" | "tool"
  content: string
  tool_calls: ToolCall[]?  // when role=assistant
  tool_call_id: string?    // when role=tool
  created_at: ISO-8601
```

Workspaces are **logical groups** (chosen design B). They may reference filesystem paths but are not bound to a single folder. New agents inherit `workspace.paths[0]` as cwd, or `$HOME` if empty.

---

## 4. Persistence

SQLite at `~/.config/pigide/db.sqlite` (created on first run).

Tables:
```sql
CREATE TABLE workspaces (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  created_at TEXT NOT NULL,
  layout_json TEXT NOT NULL DEFAULT '{"type":"empty"}',
  paths_json TEXT NOT NULL DEFAULT '[]'
);

CREATE TABLE agents (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  type TEXT NOT NULL,
  cwd TEXT,
  status TEXT NOT NULL DEFAULT 'exited',
  created_at TEXT NOT NULL
);

CREATE TABLE chat_messages (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  role TEXT NOT NULL,
  content TEXT NOT NULL,
  tool_calls_json TEXT,
  tool_call_id TEXT,
  created_at TEXT NOT NULL
);

CREATE TABLE settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
```

`agents.status` is reset to `exited` on app start (PTYs die with the app process). No auto-restore of running PTYs across launches in MVP.

Settings keys (string→string):
- `omnirouter.base_url` (default `http://localhost:20128`)
- `omnirouter.model` (default `kr/claude-opus-4.7`)
- `omnirouter.api_key` (optional, sent as `Authorization: Bearer ...` if set)
- `whisper.model_path` (default `~/.cache/pigide/whisper-small.bin`)
- `whisper.language` (default `auto`)

---

## 5. IPC contract (Tauri commands & events)

### Commands (frontend → backend, async, JSON)

| Command | Args | Returns |
|---------|------|---------|
| `list_workspaces` | — | `Workspace[]` (without chat/agents) |
| `get_workspace` | `{id}` | `Workspace` (full) |
| `create_workspace` | `{name, paths?}` | `Workspace` |
| `rename_workspace` | `{id, name}` | `()` |
| `delete_workspace` | `{id}` | `()` |
| `update_layout` | `{workspace_id, layout}` | `()` |
| `spawn_agent` | `{workspace_id, agent_type, cwd?, count?}` | `Agent[]` |
| `kill_agent` | `{agent_id}` | `()` |
| `write_to_agent` | `{agent_id, data_b64}` | `()` |
| `resize_agent` | `{agent_id, cols, rows}` | `()` |
| `list_chat` | `{workspace_id}` | `ChatMessage[]` |
| `send_chat` | `{workspace_id, text}` | `()` (response streams via events) |
| `start_voice` | — | `()` |
| `stop_voice` | — | `()` (transcript arrives via event) |

### Events (backend → frontend)

| Event name | Payload | Meaning |
|------------|---------|---------|
| `agent://stdout` | `{agent_id, data_b64}` | PTY output chunk |
| `agent://exit` | `{agent_id, code?}` | PTY closed |
| `chat://message` | `ChatMessage` | new message appended (assistant streaming = repeated emits with same id) |
| `chat://status` | `{state: "idle"\|"thinking"\|"tool"}` | UI status indicator |
| `voice://state` | `{state: "idle"\|"recording"\|"transcribing"}` | mic state |
| `voice://transcript` | `{text}` | transcribed text (drops into draft input) |

---

## 6. Agent runtime (PTY)

`AgentManager` (singleton inside Tauri state):
- `HashMap<AgentId, AgentHandle>`
- `AgentHandle` holds: `PtyPair`, `Box<dyn MasterPty>`, `Box<dyn Child>`, writer half, `JoinHandle` for stdout reader, current `cols/rows`.

`spawn_agent(type, cwd)`:
1. Resolve binary: `kiro-cli` → `~/.local/bin/kiro-cli`, `claude` → `/usr/bin/claude`. Override via `settings` table keys `bin.kiro-cli` / `bin.claude`.
2. Open PTY at 80×24 default.
3. `CommandBuilder::new(bin)` with cwd + inherited env. No special flags initially.
4. Spawn child, store handle, update sqlite `agents.status=running`.
5. Background reader task: 4 KiB buffer; on each chunk, emit `agent://stdout` with base64 payload. On EOF/error, emit `agent://exit`, mark status `exited`.

`write_to_agent`: decodes base64 and writes to PTY master.

`resize_agent`: calls `master.resize(PtySize{cols, rows, ...})` and stores last size.

`kill_agent`: `child.kill()` then `child.wait()` async; remove from map.

On app shutdown (Tauri `RunEvent::Exit`): kill all children synchronously.

---

## 7. Tiling layout (frontend)

Pure-data tree in `zustand`:
```ts
type LayoutNode =
  | { type: "empty" }
  | { type: "leaf"; agentId: string }
  | { type: "split"; direction: "h"|"v"; ratio: number; a: LayoutNode; b: LayoutNode };
```

Renderer: recursive React component using `<Allotment>` (vertical) / `<Allotment vertical>` (horizontal) for splits, and `<AgentTile>` for leaves.

Operations (mutate immer-style, persist to backend after debounce 200 ms):
- `splitLeaf(leafId, direction, newAgentId)` → replace leaf with `{split, a: leaf, b: leaf(new), ratio: 0.5}`
- `closeLeaf(leafId)` → if parent is split, replace parent with the surviving sibling (collapsing). Root becomes `empty` if last leaf removed.
- `setRatio(splitPath, ratio)` from allotment onChange.
- `maximize(leafId)` → render only that leaf at full size; preserve previous tree in transient state.

Adding the **first** agent goes into root (`empty` → `leaf`). Adding more without specifying target splits the **focused** leaf vertically by default (12 agents → bnary tree producing roughly 4×3 grid through repeated alternating splits).

---

## 8. Orchestrator

Simple loop in Rust, one tokio task per `send_chat`:

```rust
loop {
  let resp = post_chat_completions(messages, tools).await?;
  let choice = resp.choices[0];
  if let Some(calls) = choice.message.tool_calls {
    for call in calls {
      let result = dispatch(call).await?;        // execute tool
      messages.push(assistant_with_tool_call);   // record
      messages.push(tool_result_message(result));
    }
    continue;
  }
  // plain assistant text
  emit("chat://message", choice.message);
  break;
}
```

System prompt (RU+EN mixed; user's prompts may be Russian):
> You are PigIDE Orchestrator. You manage workspaces and CLI agent tiles for the user. Use the provided tools to act. Be concise. After tool calls, briefly confirm what you did. Do not invent agent ids — always call list_agents/list_workspaces first if uncertain.

### Tool definitions (OpenAI tools schema)

```jsonc
[
  { "type": "function", "function": {
      "name": "list_workspaces",
      "description": "List all workspaces with id, name, agent_count.",
      "parameters": { "type": "object", "properties": {}, "required": [] }
  }},
  { "type": "function", "function": {
      "name": "create_workspace",
      "description": "Create a new workspace.",
      "parameters": { "type": "object", "properties": {
          "name": { "type": "string" },
          "paths": { "type": "array", "items": { "type": "string" }, "default": [] }
      }, "required": ["name"] }
  }},
  { "type": "function", "function": {
      "name": "switch_workspace",
      "parameters": { "type": "object", "properties": { "id": {"type": "string"} }, "required": ["id"] }
  }},
  { "type": "function", "function": {
      "name": "delete_workspace",
      "parameters": { "type": "object", "properties": { "id": {"type": "string"} }, "required": ["id"] }
  }},
  { "type": "function", "function": {
      "name": "list_agents",
      "description": "Agents in the current (or given) workspace.",
      "parameters": { "type": "object", "properties": { "workspace_id": {"type": "string"} } }
  }},
  { "type": "function", "function": {
      "name": "spawn_agent",
      "description": "Spawn one or more CLI agents in current workspace.",
      "parameters": { "type": "object", "properties": {
          "agent_type": { "type": "string", "enum": ["kiro-cli","claude"] },
          "count": { "type": "integer", "default": 1, "minimum": 1, "maximum": 32 },
          "cwd": { "type": "string" }
      }, "required": ["agent_type"] }
  }},
  { "type": "function", "function": {
      "name": "close_agent",
      "parameters": { "type": "object", "properties": { "agent_id": {"type": "string"} }, "required": ["agent_id"] }
  }},
  { "type": "function", "function": {
      "name": "focus_agent",
      "parameters": { "type": "object", "properties": { "agent_id": {"type": "string"} }, "required": ["agent_id"] }
  }},
  { "type": "function", "function": {
      "name": "send_to_agent",
      "description": "Inject text + Enter into an agent's stdin. Use 'active' for the focused agent.",
      "parameters": { "type": "object", "properties": {
          "agent_id": { "type": "string" },
          "text": { "type": "string" },
          "press_enter": { "type": "boolean", "default": true }
      }, "required": ["agent_id","text"] }
  }},
  { "type": "function", "function": {
      "name": "get_layout",
      "parameters": { "type": "object", "properties": {}, "required": [] }
  }}
]
```

Each tool call resolves through the same managers used by the frontend (no duplicated business logic). The orchestrator's "current workspace" is read from `settings.current_workspace_id` (also written by the frontend on switch).

---

## 9. Voice pipeline

### Capture
`cpal` builds an input stream at 16 kHz mono i16 (resample if device differs). Samples accumulate in a `Mutex<Vec<f32>>` while `recording=true`. Hard cap: 60 s of audio (drops oldest if exceeded).

### Push-to-talk flow
1. Frontend mousedown / Space hold → `start_voice` → backend opens stream, emits `voice://state {recording}`.
2. Frontend mouseup → `stop_voice` → backend stops stream, emits `voice://state {transcribing}`, runs Whisper inference on captured buffer in a blocking task, emits `voice://transcript {text}`, then `voice://state {idle}`.

### Whisper
- `whisper-rs` 0.13 (bindings to whisper.cpp).
- Model: `ggml-small.bin` (~466 MB), downloaded on first run from `huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin` to `~/.cache/pigide/`.
- Params: `Strategy::Greedy { best_of: 1 }`, `n_threads = num_cpus / 2`, language = setting (default auto).
- All inference happens on a `tokio::task::spawn_blocking` (pure CPU).

### Failure modes
- Mic missing / permission denied → emit `chat://message` system entry "Microphone unavailable: ...".
- Whisper model missing → at startup, kick off background download with progress events `voice://download {bytes,total}`. PTT button is disabled until done.

---

## 10. Module map

```
src-tauri/
  Cargo.toml
  src/
    main.rs                 // Tauri builder, .manage(state), .invoke_handler(...)
    state.rs                // AppState { db, ws_mgr, agent_mgr, orch, voice }
    db.rs                   // Pool, migrations, schema
    workspace.rs            // WorkspaceManager
    agent.rs                // AgentManager (portable-pty)
    layout.rs               // LayoutNode types, serde, validation
    chat.rs                 // ChatMessage CRUD
    orchestrator/
      mod.rs                // run_chat loop
      client.rs             // OmniRouter HTTP client (reqwest)
      tools.rs              // tool schema + dispatcher
    voice/
      mod.rs
      capture.rs            // cpal stream
      whisper.rs            // whisper-rs wrapper
      download.rs           // model download
    commands.rs             // #[tauri::command] thin wrappers
    events.rs               // event name constants
    error.rs                // unified Error / Result

frontend/
  package.json
  vite.config.ts
  index.html
  src/
    main.tsx
    App.tsx
    state/
      store.ts              // zustand: workspaces, current, layout, chat, voice
      ipc.ts                // wrappers around tauri invoke + listen
    components/
      WorkspaceSidebar.tsx
      TilingArea.tsx
      AgentTile.tsx
      OrchestratorPanel.tsx
      ChatList.tsx
      VoiceButton.tsx
    layout/
      tree.ts               // immutable ops on LayoutNode
      render.tsx            // recursive renderer using allotment
    styles.css
```

---

## 11. Error handling

- All Tauri commands return `Result<T, String>` where `String` is human-readable and logged via `tracing` server-side. UI shows toast on error.
- PTY spawn failures (binary not found) surface as toast + suggested settings entry.
- OmniRouter errors (network, 5xx, malformed tool args) → `chat://message` system entry with the error, then `chat://status idle`. The chat history persists this so user can retry.
- Whisper init failure → disable PTT, surface in right panel header.

---

## 12. Testing strategy

MVP-pragmatic, not exhaustive:
- **Rust unit tests** for: `layout::insert_split`, `layout::close_leaf` invariants; `tools::parse_args`; sqlite migrations idempotency.
- **Rust integration test** with `mockito`: orchestrator loop with a fake OmniRouter returning a tool call, verify dispatch + final message.
- **Smoke test** (manual scripted): start app, type "/echo hello" into a kiro-cli tile, observe stdout; type "create a workspace and spawn 4 kiro-cli" into orchestrator, observe 4 tiles.
- No frontend unit tests in MVP; rely on TS strict + smoke.

---

## 13. Out of scope (MVP)

- TTS (orchestrator speaks back) — text only for now.
- Re-attaching PTYs after restart.
- Drag-and-drop tile reorder (only header buttons + maximize).
- Multi-monitor or detached windows.
- Theming beyond a single dark theme.
- Telemetry / crash reports.
- Windows/macOS packaging (Linux first; cross-build later — code stays portable via `portable-pty` and `cpal`).

---

## 14. Key file locations on disk

- Binary: `target/release/pigide` after `cargo tauri build`
- Config DB: `~/.config/pigide/db.sqlite`
- Whisper models: `~/.cache/pigide/`
- Logs: stderr only in MVP (run with `RUST_LOG=pigide=debug,info` for verbosity)
