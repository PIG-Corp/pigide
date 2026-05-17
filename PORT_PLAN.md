# PORT_PLAN — BridgeSpace 3 → PIG IDE

**Date:** 2026-05-15
**Source:** `~/gap-vs-pigide.md`, `~/research-bridgespace3.md`
**Scope:** Implement every feature from "Чего нет в PIG IDE" section.

---

## Survey summary

- **Stack:** Rust (Tauri 2.x) backend + React 19 + TypeScript + Vite + xterm.js frontend.
- **Layout:** `src-tauri/src/` (modules: agent, swarm, memory, voice, mcp, orchestrator, tasks, rooms, files, db, commands, lib). `frontend/src/components/` (panels), `state/` (zustand store + ipc). DB is SQLite with WAL + FTS5.
- **Conventions:** all backend errors → `crate::error::Error`; commands return `std::result::Result<T, String>`; events emitted via `tauri::AppHandle::emit`. Tests are `#[cfg(test)] mod tests` colocated with the module.
- **Test runner:** `cargo test` (Rust), `pnpm exec tsc -b`/`vite build`/`eslint .` (frontend).

---

## Backlog (priority order)

| # | Feature | Status | Commit |
|---|---------|--------|--------|
| 12 | Session snapshots | ✅ | 2535c85 |
| 6  | BridgeSwarm full (file ownership + review gates) | ✅ | 2535c85 |
| 18 | Prompts library | ✅ | a451537 / 4d335ba |
| 19 | Agent configuration UI | ✅ | a451537 / 4d335ba |
| 11 | 27 themes (CSS-vars + xterm + picker) | ✅ | 7249826 |
| 15 | WSL support | ✅ | 5c5fd42 |
| 14 | SSH support (presets + spawn) | ✅ | ca7c82c |
| 17 | Deep linking (pigide://) | ✅ | fafecf8 |
| 22 | Image preview (xterm sixel/iTerm2) | ✅ | f9d5660 |
| 25 | Mention textarea (@agent/@task) | ✅ | 32ebce2 |
| 24 | CLI launch (`pigide-cli .`) | ✅ | 95fcab5 |
| 20 | Command blocks (OSC 133) | ✅ | 6ecd601 |
| 2  | Integrated code editor (CodeMirror 6) | ✅ | 8409f16 |
| 26 | Transcript normalization (full) | ✋ | parallel agent |
| 8  | BridgeVoice / PigVoice | ✋ | parallel agent |

---

## Excluded (parallel agents own these)

- BridgeVoice / PigVoice (#8, #26 partial). All `voice/` module changes.
- Skills system (`src-tauri/src/skills/`, `frontend/src/components/SkillsPanel.tsx`).
- Architect model wiring (`orchestrator/providers/`, `ArchitectSettingsPanel`).

---

## Conventions for this port

- New files use the existing module layout.
- New DB tables go in single migration steps (v10, v11).
- Each feature ships with at least one test.
- Each feature commit message: `port(bridgespace3): <feature>`.

---

## Final state

All 13 owned features complete. Backend compiles (`cargo check --bins --lib`),
134 lib tests pass, frontend `tsc -b` and `vite build` succeed,
`eslint` passes for every file in this port.
