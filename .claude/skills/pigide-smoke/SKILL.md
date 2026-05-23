---
name: pigide-smoke
description: Run the pigide spawn-and-mailbox smoke regression. Boots the agentd, spawns one builder agent via the pigide MCP, sends a no-op mail, asserts it lands in the mailbox, then tears the agent and daemon down. Use after edits in src-tauri/src/agentd, src-tauri/src/swarm, src-tauri/src/orchestrator, src-tauri/src/chat_queue*, or anything that touches the spawn / message-bus path.
disable-model-invocation: true
---

# pigide spawn-and-mailbox smoke

Quick regression that catches the class of bugs we hit most often: spawn returns success but mailbox never receives, or daemon crashes on shutdown.

## When to run

- After any change in `src-tauri/src/agentd/`, `swarm/`, `orchestrator/`, `chat_queue*.rs`, `mcp/`.
- Before pushing a branch that touches the agent lifecycle.
- When the user asks "does spawn still work" / "проверь агентов".

## Pre-flight

1. Confirm `cargo build -p pigide --bin pigide-agentd` succeeds. If not, fix the build first — smoke is meaningless on stale binaries.
2. Confirm port from `src-tauri/src/agentd/` config (or default in code) is free.
3. `.env` must be present with at least `ANTHROPIC_API_KEY` (Gemini watcher self-disables if missing — that's fine for smoke).

## Steps

### 1. Reuse the existing helper

`src-tauri/scripts/smoke_quit_restart.py` already does the daemon-up / daemon-down dance. Read it first; do NOT reinvent.

If the helper covers the scenario as-is, run it via Bash:
```
cd /home/camer/pigide/src-tauri && python3 scripts/smoke_quit_restart.py
```
Capture exit code and stderr. Non-zero = bug; report stderr verbatim, do not summarize.

### 2. Spawn-and-mail extension

If the helper does NOT cover spawn-and-mailbox (read it to confirm), extend it in-place rather than creating a parallel script. The pattern:

1. Boot the daemon.
2. Use the `pigide` MCP from this session: `mcp__pigide__create_workspace` with a temp name + path under `/tmp/pigide-smoke-<uuid>`.
3. `mcp__pigide__spawn_agent` with `agent_type: claude`, `count: 1` (or whatever's cheapest).
4. Wait for the agent in `mcp__pigide__list_agents` — poll up to 10s.
5. `mcp__pigide__send_mail` from a synthetic from_agent_id to the spawned agent's id, body `"smoke-ping"`.
6. `mcp__pigide__read_mailbox` for that agent — assert the message lands, body matches.
7. `mcp__pigide__close_agent`.
8. `mcp__pigide__delete_workspace`.
9. Stop the daemon. Verify no orphan processes (`pgrep -f pigide-agentd` returns nothing).

### 3. Pass / fail criteria

- Pass: every step returns success, mailbox receives `smoke-ping`, no orphan processes.
- Fail: any step errors, mailbox missing the message, or `pgrep` finds leftovers.

## On failure

- Print the failing step + tool response verbatim.
- Suggest the most likely culprit module from `git diff --stat` since `main`.
- Do NOT auto-revert. Smoke reports; the user decides.

## Output to user

One line: `smoke: pass` or `smoke: fail at step <n> — <one-sentence cause>`.
