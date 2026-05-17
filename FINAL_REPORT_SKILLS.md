# PigIDE Skills System — Final Report

## Scope

Made the Architect (Orchestrator) prompt extensible via user-supplied
"Skills": small named composable prompt-modules in `~/.pigide/skills/` and
`<repo>/.pigide/skills/`. The Architect auto-discovers, indexes,
hot-reloads, selects per turn (by tags / triggers / explicit `@skill:<id>`
mention), and composes them into the system prompt without blowing context.

Special meta-skill `user-skill-prompt-engineer` is auto-promoted whenever
the Architect is about to dispatch to a sub-agent (Builder, Reviewer,
Scout) so the resulting prompt is production-quality.

## What landed

### Backend (Rust, `src-tauri/src/skills/`)

- `skill.rs` — frontmatter parser (gray_matter + handrolled fallback),
  validator, sha256 digest, `SkillSourceTag` (Builtin / User / Workspace).
- `composer.rs` — handlebars-lite renderer (`{{var}}`, `{{#if v}}…{{else}}
  …{{/if}}`) + `compose_system_prompt` with a hard byte budget that drops
  whole skills rather than truncating.
- `router.rs` — deterministic per-turn router. Mention pass (+100), trigger
  substring (+5 each), tag ∩ words (+2 each), description token overlap
  (capped at +3), priority/100 tiebreaker, dispatching promotion (+50 for
  the meta-skill), score ≥ 1.0 cutoff, `max_skills` cap. `RouterMode::Off`
  short-circuits the whole subsystem.
- `registry.rs` — in-memory index with workspace > user > built-in
  precedence, shadowed entries kept and surfaced. Symlink-escape defence
  on the walker. Per-id manual `enabled` overrides via `settings`.
- `watcher.rs` — `notify-debouncer-full` watcher per source root. Emits
  `skills://reloaded` / `skills://error` on every save.
- `trace.rs` — `skills_trace` SQLite table; `record(routed, composed_chars)`
  and `latest(session?)` for the UI's *Last turn* tab.
- `tools.rs` — Tauri command surface: `list_skills`, `get_skill`,
  `set_skill_enabled`, `reload_skills`, `last_skills_trace`,
  `create_user_skill`.

### Orchestrator integration

`Orchestrator::inject_skills` runs in `build_messages`:

1. Reads `RouterConfig` from settings (`skills.router.*`).
2. Detects "dispatching turn" by lexical scan of the user's message
   (`send_to_agent`, `builder`, `сборщик`, `выдай задание`, …).
3. Routes the active skills with `dispatching` flag set.
4. Composes selected bodies into the system prompt with a
   `token_budget × 4`-char budget.
5. Persists the trace row.

Skills are injected **before** the memory preamble so they can shape which
memories the model decides to fetch.

### Built-in skills (5)

Shipped under `src-tauri/resources/skills/`:

| id                          | priority | role                                                                |
|-----------------------------|---------:|---------------------------------------------------------------------|
| `user-skill-prompt-engineer`| 95       | Generates a self-contained sub-agent prompt (the meta-skill).       |
| `memory-recall`             | 80       | Reminds the Architect to query memory before acting.                |
| `plan-decomposer`           | 70       | Breaks a non-trivial intent into the canonical 5-phase plan.        |
| `builder-brief-writer`      | 60       | Composes the `text` for `send_to_agent { agent_id, text }`.         |
| `reviewer-checklist`        | 60       | Hands a Reviewer a per-task PASS/FAIL checklist.                    |

### Frontend (`frontend/src/components/SkillsPanel.tsx`)

- New right-pane tab "Skills" with two views:
  - **Skills list** — winners + shadowed disclosure, source tag (built-in /
    user / workspace), priority, on/off toggle, tags + triggers chips,
    inline body inspector, plus a "Create user skill" stub form.
  - **Last turn** — the latest router trace, selected vs. rejected with
    score and reasons, fallback-used flag, composed-chars total.
- Listens to `skills://reloaded` and `skills://error` for live updates.
- IPC client extended in `frontend/src/state/ipc.ts` (`SkillView`,
  `SkillFull`, `SkillsTraceRow`, `onSkillsReloaded`, `onSkillsError`).

### Tests

- `cargo test --lib skills::` — 20 unit tests across parser, composer,
  router, registry (validator, frontmatter rejection, mention/trigger/tag
  scoring, max-skills cap, off mode, dispatching promotion, workspace
  shadows built-in, override disables, char-budget drop).
- `cargo test --test skills_integration` — 2 integration tests:
  end-to-end compose with routing, hot-reload (create + edit + delete).
- Full lib suite (existing + new): **124 passed, 0 failed**.

### Docs

- `SKILLS_DESIGN.md` at repo root (file format, precedence, router math,
  composer, security, settings keys, public command surface, failure
  modes).
- `docs/skills.md` — author guide.
- `README.md` — "Skills" section + how-to-test snippet.

## Boundaries respected

- **Voice (04bbb060)** — not touched.
- **BridgeSpace 3 port (b2503886)** — not touched.
- **Architect model selection (19fad603)** — not touched. Orchestrator
  consumes whatever provider `build_provider(&db)` returns. While doing so
  I had to fix three pre-existing parse errors in
  `orchestrator/providers/{mod,anthropic,omni}.rs` (`&mut dyn Trait + Send`
  precedence) that were blocking my lib build; the upstream workstream
  later replaced the closure-based delta sink with an `UnboundedSender<String>`
  channel and my orchestrator integration is unchanged because skill
  injection happens in `build_messages`, before streaming.

## Build & test status

```
cargo check                     ✔
cargo build                     ✔ (1 unrelated voice/dead-code warning)
cargo test --lib                ✔ 124 / 124
cargo test --test skills_integration   ✔ 2 / 2
pnpm build (frontend)           ✔ 2804 modules, no TS errors
```

STATUS: skills_complete
