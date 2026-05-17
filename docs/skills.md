# PigIDE Skills — author guide

> The Architect's prompt is extensible. Skills are small composable
> prompt-modules — `.md` files with a YAML frontmatter — that the Architect
> auto-discovers, indexes, hot-reloads, and selects per turn.

This guide tells you how to write your own skills. For the design, see
`SKILLS_DESIGN.md` at the repo root.

## Where do skills live?

PigIDE looks in three places, **workspace > user > built-in**:

| Source     | Path                                  | Notes                                |
|------------|---------------------------------------|--------------------------------------|
| Built-in   | `<bundle>/resources/skills/`          | Ships with the binary; do not edit.  |
| User       | `~/.pigide/skills/`                   | Your personal library.               |
| Workspace  | `<workspace.path[0]>/.pigide/skills/` | Project-specific overrides.          |

Higher-precedence sources **shadow** the same `id` lower down. The Skills
panel shows shadowed copies under a "shadowed by …" disclosure.

## File format

Every skill is one `.md` file:

```markdown
---
id: my-thing
name: My Thing
description: One line — used by the router to decide relevance
priority: 60                # 0..100, default 50
tags: [dispatch, brief]      # exact-match boost
triggers: [foo, "bar baz"]   # substring hit → instant pick
inputs:
  - name: goal
    required: true
outputs:
  - name: brief
    description: A ready-to-send prompt
model_hint: opus             # informational
enabled: true
---
You are doing X for {{goal}}.

{{#if extra_context}}
EXTRA: {{extra_context}}
{{else}}
(no extra context)
{{/if}}
```

### Frontmatter fields

- `id` *(required)* — kebab-case, `^[a-z0-9][a-z0-9-]{1,63}$`. Stable: don't
  rename casually — the Architect routes on it.
- `name` *(required)* — display name in the Skills panel.
- `description` — one line. The router scores this against the user's message.
  Be specific about *when* the skill should fire, not what it does.
- `priority` — continuous tiebreaker; higher wins.
- `tags` — boost when the user's message contains a tag word.
- `triggers` — substrings to match anywhere in the user's message
  (case-insensitive). One hit = +5 score.
- `inputs` / `outputs` — documentation only today; future versions will check
  required inputs.
- `model_hint` — informational hint to the Architect about target model
  family. Doesn't switch models on its own.
- `enabled` — disable in-file. The Skills panel can also override this via
  the `skills.disabled.<id>` setting.

### Body

The body is a [handlebars-lite](#templating) template. It gets composed into
the system prompt wrapped as:

```
[SKILL: My Thing (id=my-thing, src=user)]
<rendered body>
[/SKILL]
```

Keep bodies under 32 KB. The router will refuse oversized files.

## Templating

The renderer supports three things, deliberately:

- `{{var}}` — substitutes a value. Missing → empty.
- `{{#if var}}…{{/if}}` — block; truthy iff the value is non-empty/non-zero.
- `{{else}}` — optional branch inside an `if`.

That's it. No partials, no helpers, no escaping (prompts ≠ HTML). The
context map is populated by the Architect at compose time. Today the only
auto-populated key is `user_message`; future built-in skills may take more.

## What gets selected each turn?

The router scores every enabled, non-shadowed skill:

1. Explicit mention `@skill:<id>` or `@<id>` → +100 (instant pick).
2. Each `triggers` substring hit → +5.
3. Each `tags` ∩ message-words match → +2.
4. `description` token overlap → up to +3.
5. `priority` / 100 → continuous tiebreaker.
6. Dispatching turn → `user-skill-prompt-engineer` gets +50.
7. Skills below score 1.0 are dropped.
8. The top `skills.router.max_skills` (default 4) survive.
9. The composer packs them into `skills.router.token_budget` (default 4000
   tokens ≈ 16 KB chars). Skills that don't fit whole are dropped, never
   truncated.

You can see the full scoring per turn in **Skills → Last turn**.

## Hot reload

The watcher reloads any `.md` file you save under one of the configured
roots. Errors are surfaced as `skills://error` toasts; the broken skill is
silently dropped from the index until you fix it.

## Settings

| key                              | default | meaning                                             |
|----------------------------------|---------|-----------------------------------------------------|
| `skills.router.mode`             | `auto`  | `off` \| `deterministic` \| `auto` (det + LLM)      |
| `skills.router.token_budget`     | `4000`  | tokens reserved for skills in the system prompt     |
| `skills.router.max_skills`       | `4`     | hard cap                                            |
| `skills.router.llm_fallback`     | `false` | gate for LLM tie-break                              |
| `skills.disabled.<id>`           | unset   | `"true"` to disable a skill regardless of file flag |

## Authoring tips

- **Triggers earn their keep.** A `description` overlap is fuzzy; a trigger
  is precise. If your skill should fire on the word *"deploy"*, list it.
- **Don't dump everything.** A focused 30-line skill outperforms a 300-line
  monolith. Split by intent.
- **Use the meta-skill for prompts.** When your skill produces a sub-agent
  brief, *delegate to* `[[user-skill-prompt-engineer]]` rather than
  re-implementing it. Mention it via `@skill:user-skill-prompt-engineer` if
  you want to force it into the active set.
- **Test with the trace.** The "Last turn" tab shows exactly what fired and
  why. If your new skill never gets picked, it's almost always a
  description/trigger problem.
- **Match user language.** Skills get prepended to the system prompt — the
  Architect still replies in the user's language.

## Built-in skills

Five ship by default:

- `user-skill-prompt-engineer` — the meta-skill. Generates a self-contained
  prompt for a sub-agent. Auto-promoted whenever the Architect is about to
  call `send_to_agent`.
- `plan-decomposer` — splits a non-trivial intent into the canonical
  5-phase plan.
- `builder-brief-writer` — composes the `text` argument for a Builder.
- `reviewer-checklist` — produces the PASS/FAIL brief for a Reviewer.
- `memory-recall` — reminds the Architect to query memory before acting.

You can shadow any of them by dropping a file with the same `id` into your
user or workspace dir.

## Security

Skills are local files. There is no remote fetching, no `eval`, no shell
execution. The composer renders text. The watcher refuses to follow
symlinks that escape their root.
