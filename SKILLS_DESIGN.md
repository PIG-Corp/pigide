# PigIDE Skills System — Design

> Status: implemented. Lives under `src-tauri/src/skills/` (backend),
> `frontend/src/components/SkillsPanel.tsx` (UI),
> `src-tauri/resources/skills/` (built-in skills),
> `~/.pigide/skills/` (user-supplied), `<repo>/.pigide/skills/` (workspace).

## Why

The PigIDE Orchestrator ("Architect") is currently driven by a fixed
`SYSTEM_PROMPT_BASE`. That base is good but rigid: every prompt addition forces
a rebuild, every team has slightly different conventions, and the prompt has no
way to adapt to *what this turn is actually about*.

Skills are small, named, composable prompt-modules that the Architect
auto-discovers, indexes, hot-reloads, selects per turn, and composes into the
system prompt without blowing context.

A special meta-skill, `UserSkillPromptEngineer`, is invoked whenever the
Architect needs to **generate** a self-contained prompt for a sub-agent
(builder, reviewer, scout) — input role + goal + constraints + files-in-scope
+ exit-criteria + references → output a single ready-to-send prompt.

## Out of scope

These belong to other workstreams and are NOT touched here:

- Voice pipeline — owned by `04bbb060`.
- BridgeSpace 3 feature port — owned by `b2503886`.
- Architect model selection (Opus 4 / 4.5) — owned by `19fad603`. We just
  consume the model wired up by that workstream.

## File format

A skill is a single `.md` file with a YAML frontmatter header:

```markdown
---
id: builder-brief-writer
name: Builder Brief Writer
description: Writes a self-contained brief for a Builder agent
version: 1
priority: 50
tags: [dispatch, builder, brief]
triggers: [builder, "send_to_agent", "сборщик", "дать задачу"]
inputs:
  - name: goal
    required: true
  - name: files_in_scope
    required: false
outputs:
  - name: brief
    description: One ready-to-send prompt for an agent
model_hint: opus
enabled: true
---

You are helping the Architect compose a brief for a Builder agent.

GOAL: {{goal}}

{{#if files_in_scope}}
FILES IN SCOPE:
{{files_in_scope}}
{{/if}}

Exit criteria: {{exit_criteria}}
```

### Frontmatter schema

| field         | type            | required | notes                                                       |
|---------------|-----------------|----------|-------------------------------------------------------------|
| `id`          | kebab-case str  | yes      | unique within source; precedence resolves collisions        |
| `name`        | string          | yes      | shown in UI                                                 |
| `description` | string          | yes      | one-line summary; used by router (lexical + LLM fallback)   |
| `version`     | int             | no (1)   | bump on breaking template changes                           |
| `priority`    | int 0..100      | no (50)  | router tiebreaker (higher wins)                             |
| `tags`        | string[]        | no       | exact-match boost for the router                            |
| `triggers`    | string[]        | no       | substrings/regex-light hits in user message → instant pick  |
| `inputs`      | obj[]           | no       | declared template variables                                 |
| `outputs`     | obj[]           | no       | documentation only                                          |
| `model_hint`  | enum            | no       | `opus` \| `sonnet` \| `haiku` — informational               |
| `enabled`     | bool            | no (true)| disabled skills are loaded but skipped by the router        |

Body is a **handlebars-lite** template (see *Composer* below).

### Validation rules

- `id` must match `^[a-z0-9][a-z0-9-]{1,63}$`.
- `tags`/`triggers` are deduped, lower-cased, max 32 each.
- Body must be ≤ 32 KB and contain at least one non-whitespace character.
- Unknown frontmatter keys are kept (forward-compat) but warned in trace.

## Discovery & precedence

Skills are loaded from three roots, in this order:

1. **Built-in** — `<exe>/../resources/skills/` (shipped with the binary). At
   dev time we resolve from `CARGO_MANIFEST_DIR/resources/skills`.
2. **User** — `~/.pigide/skills/`.
3. **Workspace** — `<workspace.path[0]>/.pigide/skills/`.

When the same `id` shows up in more than one source, **workspace > user >
built-in**. The "shadowed" copies are kept in the index but marked
`shadowed_by: <source>` so the UI can show that.

Files are walked recursively; any `*.md` inside a root that has YAML
frontmatter is treated as a skill. Files without frontmatter are silently
skipped (so a workspace can drop random `.md` files into the dir without
breaking).

## Hot-reload

A `notify-debouncer-full` watcher is attached to each existing root. On a
filesystem event we:

1. Re-load the affected file.
2. Validate it; on failure, drop it from the index and emit
   `skills://error { id, source, error }`.
3. On success, swap in the new entry atomically; emit
   `skills://reloaded { id, source }`.

Manual `reload_skills()` does a full rescan and is the entry point for
"workspace changed" — when the user switches workspaces we re-attach the
watcher to the new path.

The current workspace's `paths[0]` is used as the workspace root; if there are
no paths, only built-in + user roots are watched.

## Router

Per turn, the router picks ≤ `max_skills` (default 4) skills to compose into
the system prompt. The pipeline is:

1. **Mention pass.** If the user's message contains `@skill:<id>` or
   `@<id>`, that skill is forced in (top of stack, ignores `enabled=false`
   only if explicit).
2. **Trigger pass.** Each skill's `triggers` are case-insensitively scanned
   in the user's message. Any hit gives a +5 score.
3. **Tag pass.** Tags are hashed against a small lexical signature of the
   message (lowercased word set ∩ tags). Each match is +2.
4. **Description lexical pass.** TF-style overlap between message tokens and
   description tokens, capped at +3.
5. **Priority pass.** `priority/100` is added as a continuous tiebreaker.
6. **Cutoff.** Skills with score < 1.0 and no explicit mention are dropped.
7. **LLM fallback** *(optional, gated by `skills.router.llm_fallback=true`)*.
   If the deterministic pass produced nothing and the message is ≥ 6 words,
   we ask OmniRouter (`temperature=0`, JSON mode) for a ranked list.
8. **Token budget.** Pack skills greedily by score until
   `skills.router.token_budget` (default 4000 tokens, ≈ 16 KB chars) is
   spent.
9. **Always-on.** `MemoryRecall` and `UserSkillPromptEngineer` are tagged
   `always_on: false` by default but are *promoted* whenever the Architect
   is about to dispatch (i.e. emits `send_to_agent`) — see *Integration*.

The deterministic part is pure (no I/O) and fully unit-tested with fixtures
under `tests/router_fixtures/`.

## Composer

The body is rendered with a tiny handlebars-lite engine — supports:

- `{{var}}` — string substitution; missing vars render as empty.
- `{{#if var}}…{{/if}}` — block; truthy iff var is non-empty/non-zero.
- `{{else}}` inside an `if` block.
- HTML-like escaping is *not* applied (prompts ≠ HTML).

The composer wraps each rendered skill in:

```
[SKILL: <name> (id=<id>, src=<built-in|user|workspace>)]
<body>
[/SKILL]
```

…then concatenates them in router order *after* the base
`SYSTEM_PROMPT_BASE` and *before* the `[WORLD STATE]` and
`[MEMORY CONTEXT]` blocks.

## Built-in skills

Five ship by default under `src-tauri/resources/skills/`:

| id                          | role                                                                |
|-----------------------------|---------------------------------------------------------------------|
| `user-skill-prompt-engineer`| Generates a self-contained sub-agent prompt (the meta-skill).      |
| `plan-decomposer`           | Breaks a non-trivial user intent into ordered phases + tools.       |
| `builder-brief-writer`      | Writes the brief that goes inside `send_to_agent { text }`.         |
| `reviewer-checklist`        | Hands a Reviewer a per-task PASS/FAIL checklist.                    |
| `memory-recall`             | Reminds the Architect to query memory before deciding.              |

`UserSkillPromptEngineer` is treated specially: when the Architect is about
to dispatch (i.e. its plan contains `send_to_agent`), the composer guarantees
`UserSkillPromptEngineer` is in the active set so the agent prompt comes out
production-quality.

## Telemetry & last-turn trace

Every turn that uses skills emits a structured trace to the DB
(`skills_trace` table):

```sql
CREATE TABLE skills_trace (
    id          TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL,
    turn_at     TEXT NOT NULL,
    selected    TEXT NOT NULL,  -- JSON: [{id, score, reasons}]
    rejected    TEXT NOT NULL,  -- JSON: [{id, score, reasons}]
    composed_chars INTEGER NOT NULL,
    fallback_used  INTEGER NOT NULL DEFAULT 0
);
```

The frontend's *Last Turn* tab in the Skills panel reads the latest row.

## Security

Skills are local files. There is **no remote skill fetching, no eval, no
shell execution**. The composer only renders text. The router never calls
out to anything other than OmniRouter for the optional LLM fallback (and
even then only the message + descriptions, never the bodies).

The watcher refuses to follow symlinks that point outside the configured
root (defence-in-depth — a malicious workspace cannot point its
`.pigide/skills/` at `/etc`).

## Settings keys

| key                              | default | meaning                                             |
|----------------------------------|---------|-----------------------------------------------------|
| `skills.dirs.user`               | `~/.pigide/skills` | absolute path                            |
| `skills.dirs.workspace`          | auto    | overrides the default `<ws>/.pigide/skills`         |
| `skills.router.mode`             | `auto`  | `off` \| `deterministic` \| `auto` (det + LLM)      |
| `skills.router.token_budget`     | `4000`  | tokens reserved for skills in the system prompt     |
| `skills.router.max_skills`       | `4`     | hard cap                                            |
| `skills.router.llm_fallback`     | `false` | gate for LLM tie-break                              |

## Public Tauri commands (frontend ↔ backend)

| command                   | purpose                                              |
|---------------------------|------------------------------------------------------|
| `list_skills`             | UI list (id, name, source, enabled, shadowed)        |
| `get_skill { id }`        | Full body + frontmatter                              |
| `set_skill_enabled`       | Toggle via DB-backed override                        |
| `reload_skills`           | Force rescan                                         |
| `last_skills_trace`       | Most recent turn's selected/rejected JSON            |
| `create_user_skill`       | Convenience: write a stub to `~/.pigide/skills/`     |

## Tests

- `tests/skills_validator.rs` — schema validator unit tests.
- `tests/skills_router.rs` — router fixtures (mention / trigger / tag /
  cutoff / LLM-fallback-disabled-path).
- `tests/skills_composer.rs` — handlebars-lite snapshots.
- `tests/skills_integration.rs` — fake turn → asserts skills selected and
  composed into the system prompt.
- `tests/skills_hotreload.rs` — write a new file, assert it's picked up.

## Failure modes & how we surface them

| failure                              | UX                                          |
|--------------------------------------|---------------------------------------------|
| invalid frontmatter                  | `skills://error` toast, skill unloaded      |
| watcher cannot bind                  | logged warn, periodic poll fallback         |
| LLM fallback request fails           | trace `fallback_used=0`, deterministic only |
| skill body > 32 KB                   | unloaded, error in trace                    |
| budget exceeded by mid-skill         | skill dropped wholesale (never truncated)   |

## Migration

The skills system is additive: with `skills.router.mode=off` the orchestrator
behaves exactly as before. The default is `auto` with the five built-ins
shipped, so users get useful behaviour out of the box.
