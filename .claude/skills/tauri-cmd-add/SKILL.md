---
name: tauri-cmd-add
description: Add a new Tauri 2 IPC command end-to-end. Wires the Rust handler in src-tauri/src/commands.rs, registers it in the tauri::Builder invoke_handler, adds the matching capability entry, and provides the TS invoke wrapper for the frontend. Use when the user asks to "add a command", "expose X to frontend", "new IPC", or anything that requires a new bridge between webview and Rust.
disable-model-invocation: true
---

# Add a new Tauri 2 IPC command

Use this skill when adding a fresh `#[tauri::command]` that the frontend invokes. Doing it manually four times in a row is what produced the inconsistent capability files we have today.

## Pre-flight

1. Read these to understand the project's current shape — do not skip:
   - `src-tauri/src/commands.rs` (existing command list, return types, error pattern)
   - `src-tauri/src/lib.rs` (`tauri::Builder::default().invoke_handler(generate_handler![...])`)
   - `src-tauri/tauri.conf.json` (`identifier`, plugin allowlists)
   - `src-tauri/capabilities/default.json` (existing `permissions` array)
   - `frontend/src/state/` and any existing `invoke('cmd_name', ...)` calls (style for the TS wrapper)
2. Confirm with the user:
   - command name (snake_case in Rust, mirrored in TS)
   - inputs (typed, not `serde_json::Value` unless truly dynamic)
   - return shape (Result<T, String> by default, matching the rest of the file)
   - whether it must be `async`

## Steps (in order)

### 1. Rust handler — `src-tauri/src/commands.rs`

- Place near related commands (group by domain: chat / files / agents / orchestrator).
- Signature template:
  ```rust
  #[tauri::command]
  pub async fn <cmd_name>(
      state: tauri::State<'_, AppState>,
      // typed args
  ) -> Result<<ReturnType>, String> {
      // delegate to manager / repo, do NOT inline business logic here
      manager.do_thing(...).await.map_err(|e| e.to_string())
  }
  ```
- If the command must touch sqlite or whisper, push the work into `tokio::task::spawn_blocking` — never block the runtime in an `async fn`.
- Validate user input. Reject empty strings, out-of-range numbers, paths outside workspace.

### 2. Register in invoke_handler — `src-tauri/src/lib.rs`

- Add the symbol to the `tauri::generate_handler![...]` list.
- Keep the list sorted alphabetically within its group; the diff stays readable.

### 3. Capability entry — `src-tauri/capabilities/default.json`

- Add the permission identifier: `"<plugin-or-app>:allow-<cmd-name>"` (custom commands use the app identifier from `tauri.conf.json`).
- For app-defined commands the entry is just the command name string in the `permissions` array.
- Re-run `cargo check` — Tauri build script will fail loudly on a missing capability.

### 4. TS wrapper — `frontend/src/state/` (or the closest domain module)

- Wrap `invoke` so the frontend never calls the magic string twice:
  ```ts
  import { invoke } from '@tauri-apps/api/core';
  export async function <cmdName>(args: <Args>): Promise<<Ret>> {
    return invoke('<cmd_name>', args);
  }
  ```
- Camel-case in TS, snake_case wire name. Tauri converts argument keys automatically — keep camelCase in the TS args object.
- Add the type to wherever frontend types live; do NOT inline `any`.

### 5. Verify

- `cd src-tauri && cargo fmt && cargo clippy --all-targets -- -D warnings` — must pass.
- `cd src-tauri && cargo test --lib` if you added a unit-testable helper.
- `cd frontend && pnpm exec tsc -b` — must pass.
- Smoke: launch the app, trigger the command from the UI or browser console with `await window.__TAURI__.core.invoke('cmd_name', { ... })`.

## Common mistakes to refuse

- Skipping the capability entry — the command compiles but throws "not allowed" at runtime, looks like a frontend bug.
- Returning `serde_json::Value` instead of a typed struct — kills TS inference.
- Using `tokio::sync::Mutex` for hot-path state where `parking_lot::Mutex` is the project default.
- Keeping `Player`-like references across awaits when only a `Uuid` is needed (mirror of the agent-id rule from java-paper rules).

## Output to user when done

Single sentence: which command was added, where the four edits landed, and that fmt+clippy+tsc passed. No multi-paragraph summary.
