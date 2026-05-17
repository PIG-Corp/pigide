# PigMemory — implementation plan

**Date:** 2026-05-14
**Source:** subagent research
**Status:** plan only

Стек уже совместим: Tauri 2, rusqlite 0.31 с `bundled` (FTS5 включён), Allotment, zustand. Клон BridgeMemory: local-first markdown в `.pigmemory/`, wikilinks, backlinks, FTS5-поиск, граф, MCP-style tools.

## 1. Расположение хранилища

Корень: `workspace.paths[0]`. Fallback `~/.config/pigide/memory/`. Хранилище — `<root>/.pigmemory/`. Создание ленивое. Per-workspace кеш в `MemoryService`, перевычисляется на `workspace://changed`.

Файлы: `src-tauri/src/memory/storage.rs`, правка `src-tauri/src/lib.rs`.

## 2. Markdown layout & frontmatter

Layout: `.pigmemory/<slug>.md`, поддержка nested `.pigmemory/decisions/auth-pattern.md`. Slug — kebab-case через `slug = "0.1"`. YAML frontmatter:

```yaml
---
id: 0192...uuid7
title: Auth pattern
tags: [auth, security]
aliases: [authn]
created_at: 2026-05-14T20:00:00Z
updated_at: 2026-05-14T20:00:00Z
---
```

Парсинг: `gray_matter = "0.2"` (yaml feature).

## 3. Wikilinks

Regex: `\[\[([^\[\]\|\n]+?)(?:\|([^\[\]\n]+?))?\]\]`. Resolution:
1. exact slug
2. aliases match
3. case-insensitive title
4. ambiguous → ближайший по lex distance, флаг `ambiguous`
5. unresolved → dangling edge (`target_id=NULL`)

## 4. Backlinks index (migration user_version=3)

```sql
CREATE TABLE memory_notes (
  id TEXT PRIMARY KEY,
  workspace_root TEXT NOT NULL,
  slug TEXT NOT NULL,
  title TEXT NOT NULL,
  path TEXT NOT NULL,
  tags_json TEXT NOT NULL DEFAULT '[]',
  aliases_json TEXT NOT NULL DEFAULT '[]',
  body TEXT NOT NULL,
  mtime INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(workspace_root, slug)
);
CREATE INDEX idx_notes_root ON memory_notes(workspace_root);

CREATE TABLE memory_links (
  src_id TEXT NOT NULL REFERENCES memory_notes(id) ON DELETE CASCADE,
  dst_id TEXT,
  dst_text TEXT NOT NULL,
  display TEXT,
  ambiguous INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (src_id, dst_text)
);
CREATE INDEX idx_links_dst ON memory_links(dst_id);

CREATE VIRTUAL TABLE memory_fts USING fts5(
  title, body, tags, aliases,
  content='memory_notes', content_rowid='rowid',
  tokenize='unicode61 remove_diacritics 2'
);
-- triggers ai/ad/au для sync FTS
```

External edits — `notify = "6"` + `notify-debouncer-full = "0.3"`, debounce 500 мс.

## 5. FTS5 search

```sql
SELECT n.id, n.slug, n.title,
       snippet(memory_fts, 1, '<<', '>>', '…', 16)
FROM memory_fts f JOIN memory_notes n ON n.rowid = f.rowid
WHERE memory_fts MATCH ?1
ORDER BY bm25(memory_fts, 4.0, 1.0, 2.0, 1.5)
LIMIT ?2;
```
Веса bm25: title×4, body×1, tags×2, aliases×1.5.

## 6. suggest_connections

BM25 от FTS + `+0.3 * |tags(N) ∩ tags(M)|` бонус. Без эмбеддингов. Объяснимо.

## 7. Force-directed graph

`react-force-graph-2d@^1.25` (MIT). Backend tool `get_graph` → `{nodes, links}`. Dangling links — полупрозрачные к виртуальному `__unresolved__`.

## 8. Tools

8 штук с JSON schemas: `create_memory`, `read_memory`, `update_memory`, `delete_memory`, `list_memories`, `search_memories`, `find_backlinks`, `suggest_connections`.

## 9. Auto-injection

В `Orchestrator::run_chat` после `chat::insert(user_msg)`, перед `tool_loop`:
```
[MEMORY CONTEXT — top 3 relevant notes]
- [[auth-pattern]] (score 0.84): <snippet>
- [[stripe-webhook]] (score 0.71): <snippet>
Use [[wikilinks]] to reference these in your reply.
```
Threshold bm25 ≥ 0.1. Setting `memory.auto_inject` (default true).

## 10. UI

Tab-toggle в правой панели: `Chat | Memory`. Компоненты `MemoryPanel.tsx`, `MemoryGraph.tsx`. Редактор MVP — `<textarea>`. Monaco — future.

## Зависимости

```toml
notify = "6.1"
notify-debouncer-full = "0.3"
gray_matter = { version = "0.2", default-features = false, features = ["yaml"] }
regex = "1.10"
slug = "0.1"
pulldown-cmark = "0.10"
```
```json
"react-force-graph-2d": "^1.25"
```

## Roadmap

| # | Задача | Часы |
|---|---|---|
| 1 | Migration v3 (notes/links/fts5/triggers) | 2 |
| 2 | MemoryService + storage root + AppState | 3 |
| 3 | Note parse/serialize | 2 |
| 4 | Wikilinks regex + resolver | 3 |
| 5 | Index sync (write path) | 4 |
| 6 | FTS5 search + bm25 weights | 2 |
| 7 | suggest_connections | 2 |
| 8 | find_backlinks с context-snippet | 2 |
| 9 | 8 tool-обработчиков | 3 |
| 10 | notify watcher + debounced re-index | 4 |
| 11 | Auto-injection preamble | 2 |
| 12 | IPC commands + types | 2 |
| 13 | MemoryPanel | 6 |
| 14 | MemoryGraph | 4 |
| 15 | Tabs в правый pane | 2 |
| 16 | Тесты | 4 |

**Total ~47 h.** Критический путь: 1 → 2 → 3 → 4 → 5 → 6/7/8 → 9 → 11/13.

## Риски

- Watcher race с `update_memory` — сравнение mtime, idempotent upsert по id.
- Rename файла — id живёт в frontmatter.
- Multi-path workspace — пока `paths[0]`, future Vec.
- Русская морфология — `unicode61` без stemming, опц. snowball.
- Slug-коллизии — суффикс `-2`, `-3`.
