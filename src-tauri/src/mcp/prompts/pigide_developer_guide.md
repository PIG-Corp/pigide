# PigIDE Developer Guide

PigIDE is a desktop IDE that hosts multiple interactive CLI agents (Kiro, Claude Code, OpenCode, Devin) as tiled terminal panes. An LLM orchestrator drives them through the same tool surface you are reading now.

## Concepts

- **Workspace** — a logical project. Has a `layout` (binary split tree), zero or more agent tiles, and a `paths[]` list pointing at the project root(s) on disk.
- **Agent** — a CLI process running inside a PTY tile. Created via `spawn_agent`. Receives input via `send_to_agent` (writes literal text + Enter to stdin).
- **Task** — a first-class unit of work. Has a status (todo / in_progress / in_review / complete / cancelled) and an optional `agent_id` linking it to a tile.
- **Memory** — markdown notes in the workspace's `.pigmemory/` folder, with `[[wikilinks]]` between them. Searchable via `search_memories` (FTS5 + BM25).
- **Mailbox** — inter-agent message bus. Each entry has `from_agent_id`, `to_addr` (agent UUID or `role:builder`), and an optional `thread_id` for 1-on-1 conversations.
- **Roll-call** — broadcast a prompt to a role and collect responses asynchronously (`start_rollcall` then `collect_rollcall`).

## Phases of a typical request

1. **Setup.** `create_workspace { name, paths }` → workspace becomes current. `spawn_agent { agent_type, count }` to populate it. Capture the returned ids; you will need them.
2. **Knowledge.** `search_memories { query }` to find prior decisions. `read_memory { id }` for the full body of any hit. If you decide something new and important during the task, `create_memory` to capture it for next time.
3. **Plan.** `create_task` per unit of work. `assign_task_to_agent` to bind a task to a specific tile. Include relevant `[[memory-slug]]` references in the task `knowledge` field so the agent has context.
4. **Dispatch.** `send_to_agent { agent_id, text }` with a self-contained prompt. The receiving agent does not see your chat with the human — write the prompt as if introducing the work for the first time. Multiple parallel `send_to_agent` calls in one round are allowed.
5. **Coordinate.** Use `read_mailbox { to: "role:coordinator" }` to see who reported back. `update_task { id, status }` as work progresses.
6. **Conclude.** Plain-text reply to the user with the final summary. Don't echo state — the user already sees it.

## Tool catalog (shape only — see `inputSchema` for fields)

### Workspaces
`list_workspaces`, `create_workspace`, `switch_workspace`, `delete_workspace`.

### Agents
`list_agents`, `spawn_agent`, `close_agent`, `send_to_agent`, `get_layout`.

### Tasks
`create_task`, `list_tasks`, `get_task`, `update_task`, `assign_task_to_agent`.

### Memory
`create_memory`, `read_memory`, `update_memory`, `delete_memory`, `list_memories`, `search_memories`, `find_backlinks`, `suggest_connections`.

### Swarm
`send_mail`, `broadcast`, `read_mailbox`, `mark_mail_read`, `start_rollcall`, `collect_rollcall`.

## Scope policy

- `read` — every list/get/search/read tool.
- `mutate` — anything that writes (create/update/spawn/send/close non-agent state).
- `dangerous` — `spawn_agent`, `send_to_agent`, `delete_workspace`, `delete_memory`, `delete_task`. These have the largest blast radius and require the `dangerous` scope explicitly.

## Conventions

- Reply in the user's language. The orchestrator's chat is bilingual (RU/EN).
- Use `[[wikilinks]]` to reference memory notes — UI renders them as links.
- Keep `send_to_agent` prompts self-contained. The agent does not have access to memory or other tools by default; pass it the snippets it needs.
- `agent_id="active"` means "the most recent agent in the current workspace" — useful for "send X to the active builder" intents.
- Do not poll `list_agents` mid-loop. The host-side `[WORLD STATE]` block in your system prompt is refreshed every iteration.
