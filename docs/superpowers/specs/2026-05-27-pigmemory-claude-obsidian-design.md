# PigMemory — claude-obsidian parity for the everyday PigIDE user

**Status:** spec  
**Date:** 2026-05-27  
**Owner:** kroch228  
**Inspiration:** [AgriciDaniel/claude-obsidian](https://github.com/AgriciDaniel/claude-obsidian)

---

## 1. Goal

Make PigMemory work like an Obsidian vault that maintains itself — for the everyday PigIDE user who runs claude/codex agents in chat tiles and tracks work as tasks. Knowledge accumulates automatically from the user's normal work, the next session starts pre-loaded with relevant context, and the workbench visualizes the live ingest in a way that feels alive without being noisy.

Non-goals:

- A literal port of the claude-obsidian Claude Code plugin (different runtime, different UX surface).
- Multi-vault / cross-project bridging via REST API or filesystem MCP. PigMemory remains workspace-local.
- Six wiki "modes" (Website / GitHub / Business / Personal / Research / Book). The user's workspace already encodes intent.

## 2. Current state (as of cb21c18)

Already in place — do not rebuild:

- `Note` model + storage + FTS5 search + `[[wikilinks]]` + tags (`src-tauri/src/memory/`)
- Force-directed graph + backlinks + suggest_connections (`PigMemoryGraph`, `MemoryService::search`)
- `build_memory_preamble` auto-injects top-3 relevant notes into the orchestrator system prompt (`orchestrator/mod.rs:185`)
- Tasks lifecycle `todo → in_progress → in_review → complete` with review gates (`tasks.rs`, `swarm/review.rs`)
- Agentd PTY supervisor + classifier that already labels output as `working / done / error / ask` (`architect/classifier.rs`)
- LLM client w/ provider abstraction; default OmniRouter Opus 4.7 (`orchestrator/providers/`)

Gaps that this spec fills:

- Notes are flat (`validate_slug` rejects `/`); no folders by `kind`.
- No `kind` field in frontmatter; nothing distinguishes a `concept` from an `entity` from a raw `chat` dump.
- Nothing automatically writes to `.pigmemory/` from chats or tasks.
- No "hot cache" surfaced to the next session.
- Graph treats every node uniformly — no color-by-kind, no live ingest pulse, no activity timeline.

## 3. Approach

**Hybrid ingest pipeline.** Two cooperating lanes.

### Fast lane (synchronous, deterministic, zero-LLM)

Triggers on internal events and writes minimal note stubs immediately. No model calls, predictable latency, free.

| Trigger | Result |
|---|---|
| `task.status.changed → complete` | upsert `tasks/<task-id>.md` from title / instructions / knowledge / files-touched |
| `chat-rotation` (every N PTY lines OR tile close) | append `chats/<agent-name>/<yyyy-mm-dd>.md` chunk |
| user `/save` (future) | upsert `sources/<slug>.md` with current chat selection |

Stubs land in the graph immediately so the user sees feedback as they work.

### Smart lane (async, batched, LLM)

A tokio interval worker (default 5 min) drains `ingest_queue`, sends a batch to Haiku 4.5, and applies the returned upserts/edits idempotently. Extracts concepts and entities, lays down `[[wikilinks]]`, attaches `#tags`, appends decision blocks, marks `smart_pass_at` so the same item isn't re-processed.

Both lanes share storage. The fast lane gives liveness, the smart lane gives the obsidian-feel ("notes are organized for me").

### Hot cache

A small `meta/hot.md` is rebuilt on every smart pass. It's pulled into `build_memory_preamble` ahead of FTS hits so a fresh session opens with the right working set already in context.

## 4. Storage layout

```
<workspace>/.pigmemory/
  concepts/        — abstract ideas, patterns (smart-lane created)
  entities/        — concrete things: people, projects, files, libs (smart-lane)
  sources/         — drop-in materials, /save outputs (user-created)
  tasks/           — task stubs (fast-lane on complete)
  chats/<agent>/   — raw chat chunks per day (fast-lane)
  meta/
    hot.md         — refreshed every smart pass
    pins.md        — user-pinned slugs (manual)
```

### Note frontmatter (extended)

```yaml
---
id: <uuid>
title: ...
slug: tasks/abc-123          # / now allowed
kind: task                   # concept | entity | source | task | chat | meta
tags: [auth, refactor]
aliases: [...]
created_at: ...
updated_at: ...
ingest:
  source_kind: task          # task | chat_chunk | raw | manual
  source_ref: <task_id|chat_msg_id|file_path|null>
  ingested_at: 2026-05-27T15:00:00Z
  smart_pass_at: 2026-05-27T15:05:00Z   # null until smart-lane processed
---
<body>
```

### Storage changes required

- `storage::validate_slug` — allow `/` (single-level forward slashes), reject `..`, `\`, NUL, leading/trailing `/`, double `//`.
- `storage::slug_to_path` — already canonicalizes against root; canonicalize check stays.
- `note::serialize` / `parse` — extend frontmatter writer/reader for `kind` and `ingest` block.
- One-shot migration: existing flat notes get `kind: source`, `ingest.source_kind: manual`. Idempotent: skipped if `kind` already set.

## 5. Backend modules

```
src-tauri/src/memory/
  ingest/
    mod.rs            — pub use
    events.rs         — listens to task.status.changed / chat.chunk
    fast.rs           — deterministic stub writer
    queue.rs          — sqlite ingest_queue table + dequeue
    smart.rs          — tokio worker, batch → LLM → upsert
    prompts.rs        — system prompt for ingest LLM
    hot.rs            — rebuilds meta/hot.md
  folders.rs          — kind ↔ folder mapping
```

### `ingest_queue` table

```sql
CREATE TABLE ingest_queue (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  workspace_id TEXT    NOT NULL,
  kind         TEXT    NOT NULL,    -- 'task_complete' | 'chat_chunk'
  payload_json TEXT    NOT NULL,    -- {task_id?, chat_path?, agent_id?, ...}
  created_at   TEXT    NOT NULL,
  processed_at TEXT,
  error        TEXT,
  smart_attempts INT NOT NULL DEFAULT 0
);
CREATE INDEX ingest_queue_pending ON ingest_queue(workspace_id, processed_at);
```

### Event integration

- `tasks.rs::update` — on transition to `complete`, write a fast-lane stub AND enqueue a `task_complete` row. (Reuse existing event bus if present; otherwise direct call into `ingest::events::on_task_complete`.)
- `agentd/supervisor.rs` — buffer PTY output per agent; flush on (`>= memory.chat_rotation.lines`) or on tile close. Each flush → `fast::append_chat_chunk` + enqueue `chat_chunk`.

### Smart-lane LLM contract

Model: `kr/claude-haiku-4-5-20251001` (configurable). Provider: existing OmniRouter client.

System prompt (sketch):

```
You are PigMemory ingest for workspace <name>. Input: a JSON batch of new
items (task stubs and chat chunks). Output strict JSON with two arrays:

  upsert: [{
    kind: "concept" | "entity" | "source",
    title: string,
    body: string,           // markdown, may include [[wikilinks]] and #tags
    tags: string[],
    links_to_slugs: string[]
  }]
  edits: [{
    slug: string,
    append_section: string,  // h2 heading
    body: string             // markdown to append
  }]

Rules:
- Prefer linking to existing slugs (provided in context) over creating new.
- Max 5 new notes per batch.
- concept = abstract idea/pattern. entity = concrete thing (person, project,
  file path, library). source is reserved for user-saved material.
- Quote 1–2 lines from the source as evidence in the body.
- If nothing useful is found, return {upsert:[], edits:[]}.
```

Existing slugs in context: top 50 by FTS-score against the batch text, capped at 4KB. Token budget per batch: 8K input, 2K output.

Failure handling: parse error or schema mismatch → log, leave `processed_at` null, increment `smart_attempts`. Drop after 3 attempts; row stays for forensics.

### Idempotency

- Fast lane uses deterministic slugs (`tasks/<task-id>`, `chats/<agent>/<date>`). Re-running is a no-op overwrite.
- Smart lane: each input item carries `source_ref`. Worker queries `ingest_queue WHERE processed_at IS NULL` so processed items never re-enter. Concepts/entities deduped by exact slug match; collisions are upgraded to `edits`.

## 6. Settings (per-workspace)

| Key | Default | Notes |
|---|---|---|
| `memory.smart_ingest.enabled` | `true` | master toggle for the smart lane |
| `memory.smart_ingest.interval_seconds` | `300` | tokio interval |
| `memory.smart_ingest.model` | `kr/claude-haiku-4-5-20251001` | configurable |
| `memory.smart_ingest.max_notes_per_batch` | `5` | cap to prevent runaway |
| `memory.smart_ingest.batch_window_minutes` | `30` | how far back to drain |
| `memory.chat_rotation.lines` | `120` | flush threshold |
| `memory.hot.enabled` | `true` | rebuild hot.md after each smart pass |
| `memory.hot.max_pinned` | `8` | cap for the pins panel |
| `memory.auto_inject` | `true` (existing) | already used by `build_memory_preamble` |

## 7. Visualization

### 7.1 Color-by-kind

`PigMemoryGraph::nodeCanvasObject` reads `kind` from the node payload (extend `GraphNode` type) and picks fill from CSS variables:

| Kind | CSS var | Rationale |
|---|---|---|
| `concept` | `--accent` | the "idea" — most semantic value |
| `entity` | `--info` | concrete, queryable |
| `source` | `--ok` | user-curated input |
| `task` | `--warn-soft` | in-flight work |
| `chat` | `--fg-muted` | low-signal raw stream |
| `meta` | `--accent-soft` | hot.md, pins |

Node radius keeps the existing `4 + sqrt(degree) * 1.6` formula.

### 7.2 Ingest pulse

On `memory:note.created` and `memory:note.updated` events:

1. Frontend pushes id into `recentNodeIds: Set<string>` with a 3s timeout.
2. `nodeCanvasObject` paints a radial-gradient halo (alpha 0.4 → 0) on recent nodes.
3. New links to a recent node get `linkDirectionalParticles=2` for 1.5s using `--accent` color.
4. The new node is briefly scale-animated `0 → 1` over 300ms via canvas state (no DOM tricks).

Wired through the existing Tauri event channel; reuse `events.rs` to emit `memory://note.created` with `{id, kind, slug, links_to: [...]}`.

### 7.3 Activity timeline (new bottom strip in Graph-fullscreen)

Horizontal strip ~80px tall, full width, under the graph. Shows the last 4 hours of ingest events as colored dots on a time axis (zoomable with scroll-wheel).

- Dot color = kind color
- Hover → tooltip with title + 1-line snippet
- Click → focuses graph node + opens in editor
- Bottom-right corner: `12 today · 47 this week · 312 total`

Data source: `ingest_queue` joined with notes for the active workspace, polled every 30s. No new table.

### 7.4 Hot panel (left rail in Graph-fullscreen, collapsible)

```
🔥 Hot
■ pinned slug 1
■ pinned slug 2
□ recent slug 1
□ recent slug 2
…

🌱 New today (5)
Concept: idempotent-upsert
Entity:  haiku-4-5
Task:    chat-rotation-rfc

⚡ Smart-lane
next pass in 2:14
3 events queued
```

Backed by `meta/hot.md` + `meta/pins.md` + a small IPC `memory_smart_status(workspace_id)` that returns `{next_pass_at, queue_len}`.

### 7.5 Density modes

Toolbar segmented control in Graph-fullscreen: `[All] [Concepts] [Recent]`.

- **All:** every node (current behavior)
- **Concepts:** filters to `kind ∈ {concept, entity}` — the "knowledge graph"
- **Recent:** only nodes with `updated_at > now - 24h`

Filtering is done client-side over the existing `GraphData` payload.

### 7.6 Workbench changes (sidebar / inspector)

- **Sidebar:** kind icon prepended to each row in `NoteList`. New segmented filter above the list: `[All] [Concepts] [Entities] [Tasks] [Chats]`.
- **Inspector:** new collapsible section "Ingested from" showing `ingest.source_kind / source_ref` with deep-link buttons (`Open task`, `Open chat`). Footer button `Re-run smart pass on this note` → enqueues a one-off ingest.
- **Header:** status pill `🟢 Smart on · next 2:14` (clickable → settings popover w/ toggle, interval, model selector).
- **Empty state:** when `.pigmemory/` is empty, replace the existing blank card with a green CTA "Запусти агента или заверши задачу — память начнёт собираться сама" plus a 5-node animated mock-graph preview.

## 8. IPC surface (additions)

```ts
ipc.memorySmartStatus(workspace_id): Promise<{
  enabled: boolean;
  next_pass_at: string | null;
  queue_len: number;
  last_pass_at: string | null;
  last_error: string | null;
}>

ipc.memorySmartTrigger(workspace_id): Promise<void>     // manual run
ipc.memoryReingestNote(note_id): Promise<void>          // re-run on one
ipc.memoryActivity(workspace_id, since_iso): Promise<{
  events: Array<{ id, kind, slug, title, kind_color, at }>
}>
ipc.memorySettings(workspace_id): Promise<Settings>
ipc.memorySetSetting(workspace_id, key, value): Promise<void>
ipc.memoryPinToggle(workspace_id, slug): Promise<void>
```

Events (Tauri → frontend):

- `memory://note.created` `{id, kind, slug, links_to}`
- `memory://note.updated` `{id, kind, slug}`
- `memory://smart.tick` `{queue_len, next_pass_at}`
- `memory://smart.pass.done` `{created, updated, errors}`

## 9. Phasing

| Phase | Deliverable | Approx scope | Ships independently? |
|---|---|---|---|
| **0** | Storage rework: allow `/` in slugs, `kind` in frontmatter, migration | small | yes |
| **1** | Fast lane: task→complete stubs, chat-rotation stubs, `memory://note.*` events | medium | yes (visible in current UI) |
| **2** | Smart lane: ingest_queue, tokio worker, Haiku prompt, idempotent upsert | large | yes |
| **3** | Hot worker + meta/hot.md + auto-inject upgrade | small | yes |
| **4** | Graph: kind colors + ingest pulse + sidebar kind filter | medium | yes |
| **5** | Activity timeline + Hot panel + density modes + onboarding empty state | medium | yes |
| **6** | Settings panel + per-note re-ingest action + counters | small | yes |

Each phase is its own implementation plan + PR. After Phase 1+3 the system already works for the everyday user; Phases 4–5 make it look the part.

## 10. Open questions / explicit deferrals

- **`/save` for chat tiles:** deferred — fast lane's chat-rotation already captures everything; explicit `/save` is a polish item.
- **Manual drop-zone (`.raw/`):** deferred — most PigIDE users won't open the workspace folder; we'll add a "Drop file" target in the workbench when needed.
- **Canvas (`Wiki Map.canvas`):** deferred — the force-directed graph already covers the visual-hub use case for this audience.
- **Cross-workspace memory:** explicitly out of scope. PigMemory is and stays workspace-local.
- **Watcher for external `.pigmemory/` edits:** existing `memory/watcher.rs` already covers this — verify it picks up smart-lane writes correctly.
- **Token cost control:** Phase 2 ships with a hard cap (`max_notes_per_batch=5`, batch_window 30 min). Add a daily token-budget setting if real usage shows runaway.

## 11. Risks

- **Smart-lane hallucination.** Mitigated by: existing-slug-first rule, max-5-new-per-batch, idempotency by `source_ref`, manual `Re-run smart pass` to fix bad outputs.
- **Graph clutter from chat dumps.** Mitigated by Density modes (`[Concepts]`) and kind-based dimming.
- **Storage growth.** Chat chunks rotate daily; old chats can be archived (Phase 6+) but for now bounded by user's own chat volume.
- **PTY buffering bugs.** Reuse the existing `agentd/supervisor.rs` buffer; only add a flush hook. Won't introduce a parallel PTY reader.
- **Migration on first run.** Frontmatter rewrite is idempotent (skip if `kind` present); failures log & continue rather than abort load.

## 12. Acceptance

The everyday user should be able to:

1. Run a chat agent, finish a task — see a yellow `task` node appear in the graph within 1s, no manual save.
2. Within 5 min of finishing the task, see one or more orange `concept` nodes link to it, with extracted decisions in their body.
3. Open a fresh chat tile in the same workspace next day — agent's first reply already references the right notes by `[[wikilink]]`.
4. Open Graph-fullscreen, see colored nodes by kind, watch the activity timeline, click `[Concepts]` to declutter.
5. Toggle the smart lane off in settings — fast-lane stubs keep flowing, no LLM calls.
