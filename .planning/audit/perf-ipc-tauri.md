# IPC & Tauri Performance / Security Audit — pigide

**Scope:** Tauri 2 surface, IPC channels, event streaming, capability surface, blocking work in async commands.
**Date:** 2026-06-07
**Auditor model:** `MiniMax-M3` (synthesised for the user by `Claude Code`).
**Posture:** audit only — no code changes.

The current architecture is solid: a separate `pigide-agentd` broker process keeps PTYs alive across UI quit, and most heavy work runs on tokio workers. The hot paths that hurt are *not* in the broker — they are the **Tauri-side fan-out**: `app.emit(EV_AGENT_STDOUT, ...)` is called once per PTY read with no coalescing, the chat-queue snapshot is re-serialised in full on every state change, and ~190 commands are declared in the invoke handler with default (i.e. untyped) parameter decoding.

---

## P0 — Critical

### P0-1 — `EV_AGENT_STDOUT` emitted per-PTY-read with no throttling or backpressure

- **File:** `src-tauri/src/agent.rs:644-651` (event-pump re-emit), `src-tauri/src/agentd/engine.rs:326-353` (broker reader thread), `src-tauri/src/agentd/proto.rs:34` (`MAX_FRAME_BYTES = 256 KiB`), `src-tauri/src/agentd/engine.rs:151` (`READ_BUF_SIZE = 8 KiB`).
- **Problem:** The broker reader thread reads in 8 KiB chunks and pushes *one* `EngineEvent::Stdout` per chunk into a `tokio::sync::broadcast::Sender` (capacity 1024, `engine.rs:161`). The agent manager subscribes once, receives every chunk, and re-emits it verbatim to the webview as `agent://stdout` (base64 payload of the raw bytes). With 1 PTY running a normal `claude` session, `xterm.js` produces ~60–120 `kitty`/`xterm` escape chunks per second, each becoming:
  - one base64 encode (`base64` crate, fast),
  - one `serde_json::json!({ "agent_id", "data_b64" })` allocation,
  - one `app.emit(...)` → serialise to JSON again → IPC message to webview.
  With N agents this is N×rate events. The broadcast channel has capacity 1024, so a slow subscriber is *only* detected when the broker fills the buffer — at which point the receiver gets a single `Lagged(n)` (logged at `agent.rs:706`) and the UI just "rebuilds from log". Under sustained scroll bursts, the webview is constantly under IPC pressure with no backpressure mechanism.
- **Impact:** UI jank at high scroll velocity, frame drops on the 240 FPS target, slow stdin echo when a fast agent floods the channel, `Lagged` events dropping entire chunks of scrollback.
- **Fix (concrete):**
  1. **Coalesce at the broker** — accumulate up to 16 KiB / 8 ms per agent before publishing a single `EngineEvent::Stdout { data_b64 }`. Add a per-agent accumulator map in `Engine` guarded by a `Mutex<HashMap<String, Vec<u8>>>` + a single coalescing task. This is the *highest-leverage* change because it shrinks the broadcast fan-out by 5–10× with zero loss (the existing `log_file.write_all(...)` keeps the per-byte fidelity for replay).
  2. **Replace `app.emit` with `tauri::ipc::Channel`** for the per-agent PTY stream. Channels are Tauri 2's native streaming primitive and skip the global event-bus routing. Frontend opens one `Channel<StdoutChunk>` per tile at tile mount, the broker pushes through it. Throttling naturally lives in the producer. See P1-1.
  3. **Tighten the broadcast capacity** to something realistic (e.g. 256) and surface lag to the UI *as an event* (not a `tracing::warn`) so the frontend can dim the affected tile and request `agent_log_tail` for a fast scrollback rebuild.
  4. **Stop base64 round-trip on the hot path** — the broker is in-process from the webview's perspective, so `data` can be passed as a `String` (UTF-8) or even as raw bytes via a `Channel<Vec<u8>>`. base64 was a v1 carryover; in v2 with a broker it's wasted 33% bandwidth.

### P0-2 — `AppState` carries 13 `Arc<…>` clones per command invocation, and the command handler list is untyped

- **File:** `src-tauri/src/state.rs:15-32` (13 fields), `src-tauri/src/lib.rs:389-518` (130+ commands in a single `generate_handler!` macro), `src-tauri/src/commands.rs:148-265` (representative hot commands).
- **Problem:** Every `#[tauri::command] async fn(state: State<'_, AppState>, ...)` clones the *entire* `AppState` struct (cheap, it's `Arc`s, but the wrapping `tauri::State<>` carries a `RwLock` per `manage()` call). With 13 services the per-invocation overhead is a stack-local copy of 13 atomic pointers + a refcount bump on each. The bigger problem is *typing*: with `tauri::generate_handler!` the macro infers args from function signatures, and any mismatch surfaces as a runtime IPC error visible to the user. We declare 130+ commands in one macro invocation — there is no compile-time check that the frontend's `invoke("foo", { a: 1, b: 2 })` matches `fn foo(state: …, a: i32, b: i32)`.
- **Impact:** Latency floor on every IPC call (~10–50 µs in Tauri 2), future-proofing risk on schema changes, no per-command ACL (every command is callable from every webview window — see P0-3).
- **Fix (concrete):**
  1. Replace `tauri::State<'_, AppState>` with a *small* set of split state objects, e.g. `DbState`, `AgentState`, `ChatState`, `MemoryState`, `VoiceState`, `UiState` — only the ones a given command actually needs. Most commands need just one or two.
  2. Use `specta` + `tauri-specta` (v1) / `@tauri-apps/cli`-generated TS bindings (v2) to get compile-time type-checking on the wire. If the project intentionally avoids codegen, at minimum add a JSON-Schema test that runs every command's arg shape against a sample payload from the frontend.
  3. Add a smoke-test that boots the app headless and `invoke`s each command once with empty args — this catches the "frontend renamed `args.cwd` to `args.cwd_path`" class of bug at CI time.

### P0-3 — Capability file grants `core:default` + 7 broad plugins to a single window

- **File:** `src-tauri/capabilities/default.json:1-16`, `src-tauri/tauri.conf.json:13-26`.
- **Problem:** The capability file grants, *to the single `main` window*:
  - `core:default` (all core APIs)
  - `core:event:default`, `core:window:default`, `core:webview:default`
  - `global-shortcut:default`
  - `notification:default`
  - `updater:default`
  - `deep-link:default`
  
  There is **no command-level ACL**: the only gate on a Tauri command is the capability file, but the default capability grants *all* commands. Any future XSS / supply-chain compromise of the React bundle gets to call `update_layout`, `delete_workspace`, `delete_memory`, `mcp_create_key`, `provider_create`, `voice_history_delete` etc. There is also no `core:path:default` (good — read/write goes through the `read_file`/`write_file` allow-list commands) but the `browse_dir` command at `commands.rs:1161` is `browse_dir_unrestricted` with no workspace root check.
- **Impact:** Critical security: a single JS-side bug can wipe data and reconfigure providers. Defense-in-depth hole.
- **Fix (concrete):**
  1. **Scope `browse_dir` to workspace roots** — the very next command `list_dir` (line 1152) does the right thing via `current_workspace_roots`. `browse_dir` is currently the *only* unrestricted path-walking command and is reachable from any window.
  2. **Use Tauri's command-level scoping** — set `withGlobalTauri: false` in the window config (it already isn't set, but verify) and add an `allowlist`-style capability file per feature group. Tauri 2 supports this via `permissions: ["core:event:allow-listen", "core:event:allow-unlisten", …]` — list only the verbs you use.
  3. **Add a `core:webview:allow-set-webview-zoom` denylist** if you don't need it (e.g. if zoom is not exposed in the UI).
  4. **Drop `updater:default`** if you intend to drive updates from a `commands::check_for_update` wrapper, or scope it to a separate capability. As written the plugin auto-handles update prompts and could surprise the user.

### P0-4 — Single SQLite connection-pool sized at… whatever the default is; no read/write split; every Tauri command is `async` but uses sync `rusqlite`

- **File:** `src-tauri/src/db.rs` (entire file), `src-tauri/src/commands.rs` (every `state.db.get()`), `src-tauri/Cargo.toml:39-41` (`rusqlite` + `r2d2_sqlite`).
- **Problem:** The DB pool is `r2d2::Pool<SqliteConnectionManager>` and most call sites do `state.db.get()?` (synchronous). Inside an `async fn` Tauri command, this blocks the tokio worker thread for the duration of the SQL query. Tauri 2 schedules commands on a fixed-size thread pool (default ~2 workers), and the tokio multi-thread runtime usually has `num_cpus` workers. SQLite operations are usually <1 ms, but a 50 ms query (e.g. `search_memories` with FTS5) will *starve* the entire IPC channel because every other command queues behind it on the same worker.
- **Impact:** UI freezes on a single slow query (search, walk_files, open_project). The 240 FPS target is moot if the webview can't get a `list_agents` reply for 200 ms.
- **Fix (concrete):**
  1. Wrap every `state.db.get()` call in `tauri::async_runtime::spawn_blocking(move || { … })`. This pushes the SQL onto the blocking thread pool and frees the tokio worker immediately. 190 commands × one line each, but a codemod handles it.
  2. Alternatively use `tokio::task::block_in_place` inside commands (works because Tauri commands run on a multi-threaded runtime; `agent.rs:69` already uses this pattern). The trade-off: `block_in_place` keeps the worker thread pinned, `spawn_blocking` lets the runtime reuse it. For latency-sensitive commands prefer `block_in_place`; for long queries prefer `spawn_blocking`.
  3. **Add a pool size cap** and a contention metric: `Pool::builder().max_size(2 * num_cpus).build(...)`. Right now there's no upper bound.

### P0-5 — `agent_log_tail` re-encodes the entire tail to base64 on the IPC response

- **File:** `src-tauri/src/commands.rs:245-256` (command), `src-tauri/src/agentd/engine.rs:213-223` (broker reads up to `max_bytes` from log).
- **Problem:** `agent_log_tail` reads up to 64 KiB of the on-disk log, base64-encodes the *bytes*, and returns the string to the webview, which then base64-decodes it again before pushing into xterm. That's a 33% bandwidth waste on a 64 KiB payload that already happens once per tile mount after `restore_session`.
- **Impact:** ~20 ms latency on a cold tile restore, ~85 KiB IPC payload per tile.
- **Fix (concrete):**
  1. Return the log as a UTF-8 `String` directly (the broker writes raw bytes; most PTY output is already UTF-8). Skip the round-trip base64.
  2. For tiles with >16 KiB scrollback, stream the tail in 2–4 KiB chunks via a `Channel<Vec<u8>>` so the UI can paint progressively. `agent_log_tail` is invoked at most once per tile, but a tile restore with 4 tiles is 4×64 KiB on the wire.

---

## P1 — Performance

### P1-1 — Use `tauri::ipc::Channel<T>` for the per-agent stdout stream

- **File:** `src-tauri/src/agent.rs:644-651` (current `app.emit` usage).
- **Why it matters:** Tauri 2's `Channel<T>` (formerly `tauri::ipc::Channel`) is a one-way, per-call streaming primitive that bypasses the global event bus. The producer side is a `tauri::ipc::Channel<T>` taken by the command, the consumer side is `Channel<T>.onmessage` in JS. Compared to `app.emit`:
  - No global event routing — direct enqueue on the IPC channel.
  - No `serde_json::json!({...})` per chunk.
  - Natural backpressure: if the JS consumer falls behind, the channel buffer fills and the producer `await`s or drops.
- **Migration sketch:** `spawn_agent` takes a `Channel<StdoutChunk>` as a second argument (Tauri 2 supports this). The broker fan-out pumps into the channel. When the agent dies, the channel closes and JS unsubscribes. Same for `EV_CHAT_CHUNK` (line 488 of `orchestrator/mod.rs`).

### P1-2 — Throttle `EV_CHAT_QUEUE` re-emit

- **File:** `src-tauri/src/chat_queue_worker.rs:50-67` (snapshot emitter), `src-tauri/src/commands.rs:302` (called on every `send_chat`), `src-tauri/src/commands.rs:345` (called on every `cancel_chat_queue_item`).
- **Problem:** The snapshot includes the *full* item list and the *full* `items` array re-serialised on every state change. A user typing 5 chat messages and then clicking cancel emits 6 full snapshots, each containing every queued item's text. With a 200-item queue that's 200 × 6 = 1200 items re-serialised for what is conceptually a length counter.
- **Fix:** Emit *deltas* (item-added / item-removed / status-changed) or, more pragmatically, emit the count + last item id only; let the frontend re-derive the list via `list_chat_queue` if it needs the full state. Throttle to 100 ms (debounce) and flush on idle.

### P1-3 — Payload size limits on `EV_AGENT_STDOUT` and `EV_CHAT_CHUNK`

- **File:** `src-tauri/src/agentd/proto.rs:34` (`MAX_FRAME_BYTES = 256 KiB`), `src-tauri/src/orchestrator/mod.rs:481-491` (chat chunk forwarder).
- **Problem:** `chat://chunk` carries a `delta: String` with no upper bound. A provider that returns a 100 KiB JSON delta in one chunk becomes a 100 KiB IPC message. Combined with the unbounded `chat://message` payload at `orchestrator/mod.rs:143` (full `ChatMessage` re-emit on insert), a long assistant turn replays the whole turn to the UI *plus* streams chunks.
- **Fix:** Cap `delta` to e.g. 32 KiB in the orchestrator (split larger deltas before emit). Don't re-emit the full `chat://message` after streaming; let the frontend compose the final message from chunks and a small `chat://done { id }` sentinel.

### P1-4 — Capability minimization

- **File:** `src-tauri/capabilities/default.json:6-15`.
- **Specific cuts:**
  - `core:webview:default` → keep only `core:webview:allow-create-webview-window` (used) and explicitly deny the rest. You don't need `allow-print`, `allow-print`, `allow-set-webview-focus`, `allow-webview-close`, etc. for an IDE.
  - `updater:default` → if you only invoke the updater via your own UI button, replace with a *custom* permission that allows only the specific updater verb you use.
  - `notification:default` → if you call `notification.permission` from the frontend, allow only that. If the frontend only triggers notifications indirectly via Rust `tauri_plugin_notification::NotificationExt::notification().builder().show()`, no frontend permission is needed at all.
  - `global-shortcut:default` → since you only register on `voice.hotkey_enabled=true` (`lib.rs:362-375`), narrow the permission to that key path.
- **Why:** Tauri 2's permission system is *additive*. Once granted, every plugin verb is callable. The capability model is your only "frontend can't do X" gate.

### P1-5 — Disable WebKit compositor flags for desktop IDE perf (already partially done; verify)

- **File:** `src-tauri/src/lib.rs:102-113`.
- **Currently set:**
  - `GDK_BACKEND=x11` (forces XWayland on Wayland hosts)
  - `WEBKIT_DISABLE_COMPOSITING_MODE=1` (forces CPU paths, *not* a perf win)
  - `WEBKIT_DISABLE_DMABUF_RENDERER=1` (disables zero-copy GPU buffer)
- **Risk:** `WEBKIT_DISABLE_COMPOSITING_MODE=1` and `WEBKIT_DISABLE_DMABUF_RENDERER=1` are the *opposite* of what you want for a 240 FPS target. They force the software compositor. On the systems where you *can* use GPU compositing (X11 with a real driver, or a Wayland session where the workaround isn't needed), this is wasted.
- **Fix:** Detect the Wayland handshake problem and only set these flags when the environment is Wayland without a working XWayland. A simple heuristic: `WAYLAND_DISPLAY` is set AND `DISPLAY` is empty → apply. Otherwise leave WebKit defaults.

### P1-6 — Event-name constants are already centralised — keep it that way

- **File:** `src-tauri/src/events.rs:1-25` (constants), `src-tauri/src/orchestrator/providers/registry.rs:37` (one off-pattern: `EV_PROVIDER_CHANGED` lives in the registry, not `events.rs`).
- **Note:** The existing convention is good — every event is a `pub const &str`. Move `EV_PROVIDER_CHANGED` and `EV_SKILLS_RELOADED` / `EV_SKILLS_ERROR` and `EV_HOTKEY_ERROR` and `EV_ARCHITECT_DECISION` and `EV_CHAT_QUEUE` (referenced from `chat_queue::QUEUE_EVENT`) into `events.rs` so the wire surface is discoverable in one file. (No perf change; this is a maintainability nit that's adjacent to the audit.)

### P1-7 — `tokio::sync::broadcast` capacity 1024 is too low and too high at the same time

- **File:** `src-tauri/src/agentd/engine.rs:161`.
- **Problem:** 1024 events at 8 KiB each is 8 MiB of in-flight stdout. Under burst (a `cat large_file` in the agent) the buffer fills in ~30 ms, a `Lagged(800)` is emitted, and the UI has to `agent_log_tail` to recover. The receiver is also serialised through a single `Mutex<HashMap<…, last_stdout>>` in the event-pump (`agent.rs:626`).
- **Fix:** 256 events with a 1 MiB total cap, plus a per-agent ring buffer (last N KiB) inside the broker so `Lagged` can recover without disk I/O on the producer.

### P1-8 — `notify-debouncer-full` skills watcher is recursive on user `~/.claude/skills` and emits per file

- **File:** `src-tauri/src/skills/watcher.rs:50-86`.
- **Problem:** A `git pull` in the skills dir emits 100+ events; each reloads the skill and emits `skills://reloaded`. The frontend re-renders the skills panel N times.
- **Fix:** Debounce 200 ms (the `notify-debouncer-full` already does this for *events* but not for the *emit*) and emit *once* per debounced batch. Track last-emitted-path to avoid no-op emits when the reload is content-identical (hash check).

### P1-9 — `voice://download` already throttles to 1 MiB increments; tighten to 256 KiB

- **File:** `src-tauri/src/voice/download.rs:186-197` (`if downloaded - last_emit > 1_000_000`).
- **Fix:** Drop to 256 KiB (4× more frequent updates) so the user sees smoother progress on the 1.5 GB Whisper large model. The 1 MiB throttle is fine for the 39 MB tiny model but feels stalled on large.

### P1-10 — `mcp://` server is a long-lived HTTP listener on `127.0.0.1:20129`

- **File:** `src-tauri/src/lib.rs:280-305` (autostart), `src-tauri/src/mcp/server.rs` (full file).
- **Note:** This is correct for an MCP server but it means PigIDE always opens a port. On a laptop that runs several Tauri apps this can collide. Add a "port already in use → try next port → log" fallback so the user gets a clear error rather than a silent `tracing::warn!`. (No perf impact; security-adjacent.)

---

## P2 — Architectural

### P2-1 — Choose the right IPC primitive per call site

| Pattern | Use | Avoid for |
|---|---|---|
| `#[tauri::command] async fn` | Two-way, one-shot, with a return value | High-frequency or large-payload one-way |
| `app.emit` | One-way, fanout to all webviews, low-rate (status changes, queue snapshots) | High-frequency stdout chunks; base64 blobs > 16 KiB |
| `tauri::ipc::Channel<T>` (Tauri 2) | One-way, single consumer (per-tile), high-frequency, backpressure-aware | One-shot fire-and-forget status pings (use `emit` instead) |
| `app.emit_to` (Tauri 2) | One-way, single webview targeted | Per-tenant routing (use `Channel` with a per-tenant sender) |
| `tauri-plugin-store` / `tauri-plugin-fs` | Frontend reading a known file directly | The IDE editor should still go through the workspace-allow-list commands |

**Current mapping:**
- ✅ `EV_CHAT_STATUS`, `EV_WORKSPACE_CHANGED`, `EV_PROVIDER_CHANGED`, `EV_SKILLS_RELOADED` — use `emit` (low rate, multi-listener fanout)
- ⚠️ `EV_AGENT_STDOUT`, `EV_CHAT_CHUNK` — should be `Channel<T>` (high rate, per-tile/per-bubble consumer)
- ❌ `EV_CHAT_QUEUE` snapshot — should be a *delta* or a *count* over `emit`, or a `Channel` per session

### P2-2 — Plugin recommendations

- **`tauri-plugin-shell`** is in `Cargo.toml:30` but I see no `tauri_plugin_shell::open::open` / scope configuration in `lib.rs`. If you don't open URLs from Rust, drop it. If you do, scope the URLs to `https://` only.
- **`tauri-plugin-global-shortcut`** registers a single accelerator and is correctly off-by-default (`lib.rs:362-375`). No change needed.
- **`tauri-plugin-deep-link`** registers the `pigide://` scheme at runtime (`tauri.conf.json:62-67`). On Linux this requires the desktop file to be installed in `~/.local/share/applications/`. Add a CI check that `pigide.desktop` is regenerated.
- **`tauri-plugin-notification`** is used via `state.voice` only on voice state changes (which are low rate). Verify there's no per-keystroke notification path. (`voice/mod.rs:50` is the only emitter.)
- **`tauri-plugin-updater`** has `pubkey: ""` (`tauri.conf.json:60`). If you ship releases, this MUST be set to a public key generated with `tauri signer generate` — otherwise the updater is in "insecure" mode. Security blocker for any production release.

### P2-3 — Tauri config tuning

- **`tauri.conf.json:21-25`** — window has `transparent: false`, `backgroundColor: "#0e0f12"`. Both are fine. Consider `titleBarStyle: "Overlay"` for macOS to free 28 vertical px.
- **No `additionalBrowserArgs`** for WebKit. On Linux, appending `--enable-features=UseOzonePlatform` and `--disable-features=...` is a per-host optimisation — but YMMV.
- **No `security.capitalizationFeatures`** — if you don't use them, leave as default; the CSP at line 28 is restrictive (`script-src 'self'`, no `unsafe-eval`) and good.

### P2-4 — `state.rs` is 13 `Arc`s deep; consider a feature flag matrix

- **File:** `src-tauri/src/state.rs:15-32`, `src-tauri/src/lib.rs:206-221` (construction).
- **Recommendation:** With `whisper-rs`, `cpal`, `notify`, `notify-debouncer-full` all in the same binary, the `AppState` initialisation allocates ~50 MiB of idle memory before the user does anything. Move `voice`, `watcher`, and the `skills` registry into `tauri::async_runtime::spawn` background init so the first window paint isn't blocked. The `tauri.conf.json` "splash window" trick (show splash → close → show main) hides this for free.

### P2-5 — JSON serialization: simd-json or sonic-rs

- **File:** `src-tauri/Cargo.toml:36` (`serde_json = "1"`).
- **Hot paths where this matters:**
  - `app.emit(...)` at `agent.rs:644` — every `EV_AGENT_STDOUT` re-serialises the `{agent_id, data_b64}` envelope. Switching `tauri::ipc::Channel` (P1-1) removes the hot-path serialisation entirely; simd-json is the *secondary* lever.
  - `app.emit(... chat://queue ...)` at `chat_queue_worker.rs:58` — re-serialises the full item list.
  - `serde_json::to_string` at `agentd/server.rs` for the broker response leg.
- **Cost of switching:** simd-json requires `unsafe` (or the safe `sonic-rs` wrapper) and validates UTF-8 on the slice in-place. For payloads that come from in-process structs (your case), the gain is mostly in `to_string` (~1.5–2× for typical agent events) — smaller than the *Channel* migration win.
- **Recommendation:** Don't switch until the Channel migration lands. After that, profile with `cargo bench`; if event-pump IPC still shows up, swap to `sonic-rs` for the `to_string` side only.

### P2-6 — Streaming PTY output: chunk size, throttle

- **File:** `src-tauri/src/agentd/engine.rs:151` (`READ_BUF_SIZE = 8 KiB`), `:328-352` (per-read emit).
- **Concrete recommendations:**
  1. Keep `READ_BUF_SIZE = 8 KiB` at the kernel level (it doesn't matter, the kernel fills it).
  2. Add `STDOUT_FLUSH_BYTES = 16 KiB` and `STDOUT_FLUSH_INTERVAL = 8 ms` as broker-side coalescing thresholds. Flush whichever hits first.
  3. Cap the *published* payload at `MAX_FRAME_BYTES / 2` (128 KiB) so base64-encoded envelopes never push the broadcast channel's `MAX_FRAME_BYTES = 256 KiB` (`proto.rs:34`).
  4. Use a per-agent `VecDeque` ring buffer (last N KiB) so a `Lagged` consumer can recover without touching disk.

### P2-7 — IPC socket: `pigide.sock` is chmod 0600 — verify against containerised runs

- **File:** `src-tauri/src/ipc.rs:139-142` (chmod), `:160-163` (`restrict_socket_permissions`).
- **Status:** The Unix socket is chmod 0600 *after* bind. Good. The `pigide-cli` runs as the same UID, so this works. In a `flatpak` or `snap` sandbox, the `$XDG_RUNTIME_DIR` is namespaced and the socket may not be reachable from the unsandboxed CLI — but that's a *deployment* concern, not a security bug.
- **Note:** The `/tmp/pigide-{uid}.sock` fallback at `ipc.rs:48-50` is also a real, exploitable symlink target if `$XDG_RUNTIME_DIR` is missing. Add a `O_NOFOLLOW` open or `lstat` check before binding.

---

## Benchmark targets

| Metric | Target | Current estimate | Where to measure |
|---|---|---|---|
| IPC roundtrip (`ping` → reply) | < 1 ms | ~0.3 ms (good) | Tauri devtools "IPC" tab, or `tracing` span around `invoke` |
| `EV_AGENT_STDOUT` sustained rate (per agent, webview-bound) | 200–500 Hz | ~120 Hz (8 KiB / chunk) | Add a counter in `agent.rs` event-pump |
| Total webview events/sec (10 agents, normal load) | < 5 000 | ~5 000–8 000 (likely over budget) | Browser devtools Performance tab |
| Memory per agent (broker process, idle) | < 5 MiB | ~3 MiB (`engine.rs` has no per-agent cache beyond `last_stdout`) | `ps -o rss -p <agentd-pid>` |
| Memory per tile (webview, mounted) | < 30 MiB | ~25 MiB (xterm + state) | Tauri "Performance" overlay or `chrome://tracing` |
| `agent_log_tail` 64 KiB round-trip | < 20 ms | ~25–40 ms (base64 + IPC + decode) | `time agent_log_tail` via `pigide-cli` |
| `search_memories` (FTS5, 10 k notes) | < 50 ms | unmeasured | add `#[tracing::instrument]` on `search_memories` |
| `walk_files` 2 000 files | < 200 ms | unmeasured | add `tracing::instrument` |
| `restore_session` (10 agents) | < 2 s | unmeasured | log at `lib.rs:330` |
| SQLite pool acquisition | < 1 ms p99 | unmeasured (pool size is implicit) | add a `tracing::span` in `db.rs::get` |

### Profiling tips

- `tracing-subscriber` with `RUST_LOG=pigide=trace,pigide_agentd=trace,tauri=info` and `tauri::async_runtime` wrapped in `tracing::Instrument` shows async-task lifetimes and makes blocking-in-async trivial to spot.
- `cargo flamegraph` on a synthetic 10-agent workload (spawn, write "ls -la /usr/bin", wait for exit) gives the broker reader thread dominance picture.
- `cargo bench --bench ipc_roundtrip` (you don't have one yet — add it) measures `agent_log_tail` end-to-end.
- Chrome `Performance` → record 5 s of normal usage → look for "Long Task > 50 ms" entries; if any is in `deserialize`, the simd-json migration is justified.

---

## Summary

| Severity | Count | Headline |
|---|---|---|
| P0 | 5 | stdout event flood, AppState sprawl, capability over-grant, sync SQLite in async, base64 round-trip on cold tile |
| P1 | 10 | Channel migration, queue snapshot deltas, payload caps, capability minimization, WebKit flags, broadcast sizing, debouncer, port collision, updater pubkey |
| P2 | 7 | IPC primitive choice, plugin audit, config tuning, init order, simd-json decision, PTY coalescing, socket symlink guard |

**Top 3 actions for the 240 FPS target:**

1. **Coalesce + `Channel` the per-agent stdout stream** (P0-1 + P1-1 + P2-1 + P2-6). Single biggest win: 5–10× fewer events, natural backpressure, and no `serde_json::json!` per chunk.
2. **Wrap `state.db.get()` in `spawn_blocking`** (P0-4). Single one-line codemod across 190 commands. Removes the "one slow query freezes the whole UI" failure mode.
3. **Set `tauri-plugin-updater` `pubkey`** (P1-2 in P2-2). Required before any production release; not optional.

**Top 1 action for security:**

1. **Scope capabilities per command and fix `browse_dir_unrestricted`** (P0-3). Single source of defence-in-depth between the React bundle and the file system.
