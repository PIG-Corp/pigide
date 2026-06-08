# Project Resolver

Open or switch a workspace from a fuzzy natural-language hint:

 > "open the widget plugin" → `~/code/widget-plugin/`
> "переключи на pigide" → `~/pigide`
> "wdgt plgn" / "виджет плагин" → same project as the first one.

The resolver is the bridge between **what the user says to the orchestrator**
and the **directory on disk** that should become the active workspace.

## Components

```
                 ┌────────────────┐
   roots ───────▶│   Indexer      │──▶ project-index.json
                 │  (fs walker +  │     (cached on disk,
                 │   parsers)     │      rebuilt on demand)
                 └────────────────┘
                          │
                          ▼
   query  ───────▶ ┌─────────────┐ ───▶ ResolveOutcome
                   │  Resolver   │     {found | ambiguous | not_found}
                   │ (fuzzy +    │
                   │  rerank)    │
                   └─────────────┘
                          ▲
                          │ aliases.json (user-defined)
```

The resolver is a single self-contained module
(`src-tauri/src/project_resolver/`). Nothing in it talks to the LLM directly —
the LLM rerank is a thin async hook the orchestrator wires in.

## Index schema

Stored at `~/.cache/pigide/project-index.json`:

```jsonc
{
  "version": 1,
  "built_at": "2026-05-17T09:00:00Z",
  "roots": ["~/code", "~/projects", "~"],
  "projects": [
    {
      "path": "~/code/widget-plugin",
      "dirname": "widget-plugin",
      "kind": ["git", "rust"],
      "names": ["WidgetPlugin"],
      "descriptions": ["Example widget plugin"],
      "headings": ["Widget Plugin"],
      "remote": "github.com/example/widget-plugin",
      "languages": ["rust"],
      "mtime": 1720000000,
      "alias_paths": [".pigmemory/aliases.json"]
    }
  ],
  "aliases": { "~/code/widget-plugin": ["widget", "widget plugin"] }
}
```

`names` / `descriptions` / `headings` / `dirname` / `remote` / `aliases`
are all collected as **signal tokens** during scoring. We never collapse
them into a single canonical "name" — the user might recall any of them.

### Project detection

A directory becomes a project iff it contains any of:
`.git/`, `package.json`, `Cargo.toml`, `pyproject.toml`, `go.mod`,
`pom.xml`, `build.gradle`, `build.gradle.kts`, `.pigide/`, `paper-plugin.yml`.

Once a directory is recognised as a project we **stop descending** into it.
This avoids indexing every sub-package of a monorepo while still allowing the
top-level repo to be picked.

### Excluded directories

By default we skip: `node_modules`, `target`, `dist`, `build`, `out`,
`.git/objects`, `.next`, `venv`, `.venv`, `__pycache__`, `.pnpm-store`,
`.cache`, `.gradle`, `.idea`, `.vscode`,
`.ssh`, `.gnupg`, `.config`, `.local`, `.cargo`,
`/proc`, `/sys`, `/tmp` (when they appear under a root).
Symlinks are not followed off the originating root.

### Roots

Default roots, deduplicated and only those that actually exist:

1. `~/code`, `~/projects`, `~/dev`, `~/src`, `~/work`
2. `~` (max depth 2, so we still pick `~/pigide` etc.)
3. Every path already known to the workspace store
   (`workspaces.paths_json` rows).

A user can extend roots via the `project_resolver.roots` setting (JSON array
of absolute paths).

### Cache invalidation

* `force` rebuild on demand (`pigide_resolver_rebuild`).
* TTL — 24 h: re-scan in the background if older.
* Filesystem watcher is **not** wired up in v1 — adding/removing a project is
  rare enough that an explicit rebuild trigger is fine.

## Resolver

```
query
  │
  ▼
1. normalise (lowercase, strip punctuation, split tokens, transliterate ru→en)
  │
  ▼
2. for every project, score = max over signals of fuzzy(query, signal)
   signals = [dirname, …names, …headings, repo-name, …aliases]
  │
  ▼
3. boosts:
     +0.30 alias exact match
     +0.20 repo-name match
     +0.15 recent workspace
     +0.10 path is current cwd / under it
     +0.05 query token is contained in dirname
  │
  ▼
4. top-K (K=5)
  │
  ▼
5. confidence gating:
     top1 >= 0.92 AND top1 - top2 >= 0.15  → found(top1)
     top1 < 0.60                            → not_found
     otherwise                              → ambiguous(top-K)
  │
  ▼
6. (optional) LLM rerank on the ambiguous bucket
     prompt: query + 5 candidates → {best_index, ambiguous, reason}
     LLM unreachable / timeout    → fall back to top1 if >= 0.85, else ambiguous
```

### Fuzzy

We avoid a heavy fuzzy crate. The module ships a vendored
**Jaro-Winkler** + **token-set ratio** in `fuzzy.rs` (~120 lines, no
dependencies). For a query `q` and a signal `s` we compute:

```
score(q, s) = max(
  jaro_winkler(q_norm, s_norm),
  token_set_ratio(q_norm, s_norm),
  prefix_bonus(q_norm, s_norm),    // q is a strict prefix of s
)
```

`token_set_ratio` is the rapidfuzz definition: split both sides on
non-alphanumeric, lowercased, sorted, then `jaro_winkler` of the
intersection plus differences. This is what makes
"wdgt plgn" match "widget-plugin" — the order of tokens
does not matter and a missing token just costs you ~0.15.

### Transliteration

Russian → Latin via a fixed table in `translit.rs`:
`а→a, б→b, …, ш→sh, щ→shch, ы→y, ё→e`. Same module also strips
common diacritics. Both query and signals are transliterated before
scoring, so "виджеты" produces tokens like `vidzhety`.

## Aliases

`<workspace>/.pigmemory/aliases.json`:

```json
{
  "~/code/widget-plugin": ["widget", "widget plugin"]
}
```

Aliases are **per-machine**, not per-workspace — they live on the project
directory itself, not in the workspace store, so they survive workspace
recreation. The orchestrator exposes `remember_project_alias` so users can
teach the resolver mid-conversation:

> user: "btw open that as 'widget plugin' in the future"
> orchestrator: `remember_project_alias { path, alias: "widget plugin" }`

## Orchestrator integration

Three new tools:

| tool | description |
|------|-------------|
| `resolve_project { query }` | returns `{ status, path?, candidates?, confidence }` without side effects |
| `open_project { query }` | resolves + creates/switches a workspace; returns the workspace |
| `remember_project_alias { path, alias }` | adds alias and re-indexes that path |

The Architect prompt (`SYSTEM_PROMPT_BASE`) gets a one-liner:

> When the user asks to open / switch / "поехали в" / "переключи" a project
> by name, call `open_project` with the raw natural-language hint.
> Don't try to guess directory paths yourself.

When `open_project` returns `ambiguous`, the orchestrator surfaces the
top-K to the user as a numbered list and waits for a pick.

## Performance budget

* Cold scan over `~/code` (≤200 projects, depth 5) finishes in **<5 s** on
  spinning-rust hardware. We never call `git`/network during indexing —
  only the few small files listed in *Project detection*.
* Warm `resolve_project` (index already loaded) returns in **<50 ms** for
  a 200-project corpus. Scoring is O(P · S) with P ≤ 200, S ≤ 10 — trivial.
* Index file is plain JSON, ~50 KB for 200 projects.

## Tauri surface

```
pigide_list_projects()           -> [ProjectEntry]   // current cached index
pigide_rebuild_project_index()    -> { count, took_ms }
pigide_resolve_project(query)     -> ResolveOutcome
pigide_open_project(query)        -> { workspace_id, path, status }
pigide_add_project_alias(path, alias)
pigide_remove_project_alias(path, alias)
```

The frontend uses these from the orchestrator/right-pane to expose a
"Projects" picker that shares the same scoring as the LLM.

## Out of scope (v1)

* fs watcher → live index
* multi-root projects (we surface only the discovered root for now)
* remote git introspection (latest commit author, etc.) — keep indexing
  offline so it works on a plane.
