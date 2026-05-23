# PigIDE — Consolidated Audit & Roadmap

Сводный отчёт по трём read-only аудитам ветки `reset/main-20260517-181552` (HEAD `cd711e9`):

- `AUDIT_BACKEND.md` — 15 issues, фокус `src-tauri/src/`
- `AUDIT_FRONTEND.md` — 15 issues, фокус `frontend/src/`
- `AUDIT_TOOLING.md` — 15 issues, фокус build/CI/deps/configs

После дедупликации и rebalance: **3 P0**, **18 P1**, **15 P2**, плюс 5 архитектурных рекомендаций и 3 волны исполнения.

---

## 0. Что изменилось при сведении

### Дедупликация
- **D1.** "Fan-out агентского stdout" — Backend-P0#2 (двойной `EV_AGENT_EXIT` из `engine::kill`) и Frontend-P0#1 (`AgentTile` подписан индивидуально, шторм при N тайлов) — **сливаются в одну системную проблему** (FE-P0-01) с двумя точками фикса. Объединено как **P0**, owner = backend + frontend.
- **D2.** "Type contract drift" — Backend-P1#6 (spawn_agent UI vs MCP), Frontend-Reco#3 (внедрить `ts-rs`), Tooling-P2 (TS не strict) — собраны в один **архитектурный pillar** (R6). Underlying: типы `Workspace`/`Agent`/`Task` руками копируются в `frontend/src/state/types.ts`.
- **D3.** "Auto-cleanup живущих сущностей" — Backend-P1#12 (dead `kill_all`/`shutting_down`) и Frontend-P1 (тайл моментально исчезает на agent exit) — это **одна decided UX-policy** (терминал остаётся в `exited`), но фикс в двух местах. Слиты в **P1-08**.
- **D4.** "Dead code / врущий UI" — Backend-QW#3 (`stop_chat` no-op экспортирован во фронт), Backend-R5 (мёртвая инфра), Frontend-P1#10 (UX блокеры) — отдельное обязательство в Wave 1 cleanup.
- **D5.** "CI lint quality" — Tooling-P1 (lint non-blocking, cargo-audit non-blocking) и Backend-R5 (мёртвый код) — связаны: блокирующий lint поможет вычистить мёртвое и не дать новому накопиться.

### Перевзвешивание
- **Backend-P1#5 "dead Anthropic provider"** повышен до влияния на security аудита (читатель кода видит ANTHROPIC_API_KEY чтение и думает что оно работает). Остаётся P1, но в Wave 1 cleanup.
- **Frontend-P0#3 "IPC storm в NewWorkspaceModal"** при ближайшем взгляде — это не crash, а UX/perf деградация. Понижено до **P1**.
- **Frontend-P0#4 "leak в SkillsPanel"** — реальная утечка слушателей Tauri, но только в редком race-окне unmount. Остаётся **P0**, простой фикс.
- **Tooling P2 "ts-rs / strict TS / type-aware lint"** — поднято до **P1** в роли pillar для Wave 2 (R6).

---

## 1. Сведённая карта проблем (по приоритету)

### P0 — Hotfix wave (≤ 5 дней)

| ID | Источники | Файл:строка | Категория | Суть | Owner |
|---|---|---|---|---|---|
| **P0-01** | BE-P0#2 + FE-P0#1+#2 | `agentd/engine.rs:418-438,336-340`, `frontend/AgentTile.tsx:192-210` | runtime/race + perf | На kill агент шлёт двойной `EV_AGENT_EXIT` (EOF-cleanup делает второй `runtimes.remove`+`events.send`), а на UI каждый `AgentTile` индивидуально подписан на `agent://stdout` и парсит JSON для не-своего id ($O(N^2)$ при активном выводе). Кроме того, `term.write()` после `dispose` крашит React. | backend (engine/EOF guard) + frontend (single-subscription через store + `termDisposedRef`) |
| **P0-02** | BE-P0#1 | `mcp/server.rs:197-207,343-378` | security/auth | `tools/call` без API-key исполняет read-only tools (list_workspaces/list_agents/tail_agent/read_memory) на 127.0.0.1:20129 анонимно; audit пишет `key_id=NULL`. | backend |
| **P0-03** | BE-P0#3 + BE-P0#4 | `chat_queue_worker.rs:104-133` + `agent.rs:316-374` | crash/race consistency | (a) crash между `chat::insert(user_msg)` и завершением `tool_loop` оставляет user_msg в БД; на restart worker создаст дубликат. (b) `restore_session` без TX между `list_all` и `UPDATE NOT IN(...)`, параллельный broker-spawn → пропадает. | backend |
| **P0-04** | FE-P0#4 | `frontend/SkillsPanel.tsx:57-64` | memory leak | `onSkillsReloaded` промисы резолвят `unsub` асинхронно; быстрый unmount → `unsubs` пустой, а коллбеки навешены и утекают. | frontend |

### P1 — Architecture & UX wave (1-2 недели)

| ID | Источники | Файл:строка | Категория | Суть | Owner |
|---|---|---|---|---|---|
| **P1-01** | BE-P1#5 | `orchestrator/mod.rs:67-69`, `providers/mod.rs:113-119` | dead config | `build_provider` хардкодит OmniRouter; 823 строки `anthropic.rs` мёртвые, security-читатель введён в заблуждение. | backend |
| **P1-02** | BE-P1#6 | `commands.rs:140-163` + `orchestrator/tools.rs:354-393` | duplication/drift | spawn_agent в UI и MCP-tool разошлись (`auto_layout`, `effective_cwd`). | backend |
| **P1-03** | BE-P1#7 + BE-P2#14 | `swarm/ownership.rs:42-49`, `tasks.rs:202-258`, `swarm/review.rs:146` | atomicity | многошаговые ops на разных r2d2-conn без TX, гонки на review-gate. | backend |
| **P1-04** | BE-P1#8 | `commands.rs:1759` | god-file | 1759 строк, 75 commands, дублирует policy с MCP. | backend |
| **P1-05** | BE-P1#9 | `mcp/server.rs:39-77` | scope drift | `is_mutating`/`is_dangerous` matches!() легко мимо для нового tool. | backend |
| **P1-06** | BE-P1#10 | `lib.rs:175-362` | bootstrap god | 350 строк процедурного init, ошибки молча `.warn`. | backend |
| **P1-07** | BE-P1#11 | `orchestrator/mod.rs:341-503` | long method | `tool_loop` 162 строки, mutable phantom-state, placeholder DELETE+INSERT. | backend |
| **P1-08** | BE-P1#12 + FE-P1 (auto-close) | `lib.rs:493-517`, `agent.rs:533`, `engine.rs:122`, `frontend/App.tsx:127-133` | dead semantics + UX | broker outlives PigIDE, но `kill_all`/`shutting_down` живут как мусор; фронт моментально удаляет тайл при exit вместо overlay'я. | backend (cleanup) + frontend (keep-on-exit overlay) |
| **P1-09** | FE-P0#3 (rebalanced) | `frontend/NewWorkspaceModal.tsx:109-155` | perf/UX | onChange запускает `browseDir` на каждый символ → шторм IPC + ошибки + тосты. | frontend |
| **P1-10** | FE-P1 | `frontend/WorkspaceSidebar.tsx:39-51` + `App.tsx` | state/IPC | switchTo делает локальный set + IPC; ответный `workspace://changed` запускает `reloadAfterSwitch` второй раз. | frontend |
| **P1-11** | FE-P1 | `frontend/CodeEditor.tsx:161-176` | stale data | `useEffect` зависит только от `path`, не от `initial`; внешние правки агентом не подтягиваются. | frontend |
| **P1-12** | FE-P1 | `frontend/hooks/useHotkeys.ts:91-104` | hotkeys | shortcut'ы блокируются в xterm/editor; `ctrl+1..9`, `ctrl+k`, `escape` не работают. | frontend |
| **P1-13** | FE-P1 | `frontend/App.tsx:225-231` | UX | таймер тостов завязан на `toasts[0]` → лавина ошибок перекрывает UI на десятки секунд. | frontend |
| **P1-14** | FE-P1 | `frontend/OrchestratorPanel.tsx:714-731` | touch UX | `onTouchStart`+`onMouseDown` без `preventDefault` → двойные вызовы start/stop voice. | frontend |
| **P1-15** | FE-P1 | `frontend/TilingArea.tsx:96-102` | perf | `AgentTile` не мемоизирован; ресайз сплиттера ререндерит всю сетку. | frontend |
| **P1-16** | FE-P1 (×2) | `frontend/PromptsPanel.tsx:119-126`, `SshPresetsPanel.tsx:82-89` | UX | удаление промптов и SSH-пресетов без подтверждения. | frontend |
| **P1-17** | TO-P1 (×3) | `.github/workflows/ci.yml` | CI quality gates | frontend lint `continue-on-error: true`, `cargo audit` non-blocking, pnpm store не кешируется. | tooling |
| **P1-18** | TO-P1 + R6 | `frontend/package.json`, `tsconfig.app.json` (нет `strict`), `eslint.config.js` (нет type-aware) + рукописные TS-типы в `state/types.ts` | type contract drift | нет `packageManager`/`engines`, `strict: false`, рукописная синхронизация Rust↔TS — главный источник P1-02. | tooling + backend (ts-rs derive) |
| **P1-19** | TO-P1 + TO-P1 | `src-tauri/tauri.conf.json` (cwd `pnpm --dir frontend`), `release.yml` (`CHANGELOG.md` отсутствует) | build/release | hidden bug в build script + врущий release body. | tooling |

### P2 — Polish wave (ongoing)

| ID | Источники | Файл:строка | Категория | Суть | Owner |
|---|---|---|---|---|---|
| **P2-01** | BE-P2#13 | `swarm/mailbox.rs:191-213` | auth gap | `list_thread` без access-check. | backend |
| **P2-02** | BE-P2#15 | `path_suggest.rs:125-148`, `files.rs:172-200` | perf | `canonicalize_allowed_roots` syscalls в hot-path. | backend |
| **P2-03** | FE-P2 | `frontend/BrowserPanel.tsx:168-174` | UX | белый экран при X-Frame-Options без объяснения. | frontend |
| **P2-04** | FE-P2 | `frontend/themes/catalog.ts:375-381` | a11y | Solarized Light: контраст `--fg-muted` 3.72:1, ниже WCAG AA. | frontend |
| **P2-05** | TO-P2 | `frontend/tsconfig.{app,node}.json` | TS strictness | `strict: false`, `skipLibCheck: true` скрывают breakage. | tooling (часть P1-18) |
| **P2-06** | TO-P2 | repo-root | format/pre-commit | нет JS formatter'а, нет pre-commit. | tooling |
| **P2-07** | TO-P2 | `frontend/package.json`, `ci.yml` | tests | `test:helpers` есть, но не в CI; нет Vitest/Playwright. | tooling |
| **P2-08** | TO-P2 | `redesign.spec.js` | tooling drift | требует `@playwright/test`, не объявлен; CJS в ESM-репо. | tooling |
| **P2-09** | TO-P2 | `scripts/build.sh` | platform | Linux/macOS-only, не задокументировано. | tooling |
| **P2-10** | TO-P2 | `src-tauri/Cargo.toml` | rust deps bloat | duplicate `dirs` 5/6, `thiserror` 1/2, `rand` 0.8/0.9/0.10, `reqwest` 0.12/0.13; `tokio = full`. | tooling/backend |
| **P2-11** | TO-P2 | `Cargo.toml`, `.cargo/config.toml`, `scripts/build.sh` | docs/config drift | противоречивые комментарии про `strip`/`NO_STRIP`. | tooling |
| **P2-12** | FE-UX | модалки + nullsh | UX | нативные `window.prompt`/`confirm` блокируют WebView, не стилизованы. | frontend |
| **P2-13** | FE-UX | `FolderBrowser` | UX | при клике на папку фокус сбрасывается, нет визуального выделения. | frontend |
| **P2-14** | FE-UX | `AgentTile` (loading) | UX | нет индикатора при чтении 64 KiB лога — терминал висит пустой. | frontend |
| **P2-15** | FE-UX | Kanban + workspace switch | state sync | `clearWorkspaceState` обнуляет, но Kanban запрашивается только при mount → старые задачи как stale. | frontend |

---

## 2. Архитектурные рекомендации (отчёты сошлись)

### R1. Async-trait `AgentService` вместо sync-facade `block_on_safely`
**Источник**: BE-R1.  
Tauri commands помечены `async`, watcher/architect живут в tokio — нет необходимости в `block_in_place`. Убирает класс багов "Cannot start a runtime from within a runtime".  
**Owner**: backend.

### R2. Единый policy/capability слой
**Источник**: BE-R2 + закрывает P1-05, P1-04 частично.  
`policy.rs` с `Capability` enum; `function_tool(name, desc, params, capability)` единственная точка истины. Tauri-commands и MCP читают capability оттуда же.  
**Owner**: backend.

### R3. Обязательный `db::with_tx` helper
**Источник**: BE-R3 + закрывает P0-03(b), P1-03, P2 review-gate race.  
Запретить `pool.get()` вне data-access слоёв (lint/convention). Закроет окна гонок одной механикой.  
**Owner**: backend.

### R4. `WorldStateBuilder` с event-driven invalidation
**Источник**: BE-R4 + помогает P1-07.  
Snapshot prompt-блока инвалидируется на `workspace.changed`/`agent.spawned|exit`/`task.updated`. Снимет N+1 list_agents per turn.  
**Owner**: backend.

### R5. Удалить мёртвую инфраструктуру
**Источник**: BE-R5 + FE "врущий stop_chat".  
Список к удалению: `orchestrator/prompt.v1.rs`, `agent::DEFAULT_READINESS_TIMEOUT_MS`, `engine::shutting_down`, `agent::kill_all` no-op, `commands::stop_chat` (или реализовать), `anthropic.rs` (если хардкод OmniRouter остаётся).  
**Owner**: backend + frontend (вычистить вызовы `stop_chat`).

### R6. Type-contract auto-generation (NEW — добавлено при сведении)
**Источник**: FE-Reco#3 + BE-P1-02 + TO-P1-18.  
Внедрить `ts-rs` (или `specta`) на Rust-стороне: `#[derive(TS)]` на `Workspace`/`Agent`/`Task`/`ChatMessage`/`QueueItem`/`Attachment`/etc. Генерировать `.ts` при `cargo build`. Ручная синхронизация в `frontend/src/state/types.ts` и комментарий "Kept in sync manually" перестают существовать. Снимет drift между UI- и MCP-вариантами `spawn_agent`.  
**Owner**: backend (derive + build script) + tooling (CI step) + frontend (delete рукописные types).

---

## 3. Quick wins (≤ 1 час каждый)

Поднятые из всех трёх отчётов, дедуплицированы, отсортированы по value/effort:

1. **`engine.rs:336-340`** — Exit event только если `runtimes.remove` реально снял row. *(закрывает 1/2 P0-01, backend)*
2. **`mcp/server.rs:343-378`** — `read` scope check для non-mutating tools. *(закрывает P0-02, backend)*
3. **`frontend/SkillsPanel.tsx`** — `let active = true` + проверка перед `unsubs.push`. *(закрывает P0-04, frontend)*
4. **`commands.rs:370`** — реализовать `stop_chat` (CancelHandle) или удалить из command list + frontend usage. *(closes lying UI, R5, backend+frontend)*
5. **`db.rs:39`** — `Pool::builder().connection_timeout(5s).min_idle(2)`. *(надёжность под нагрузкой, backend)*
6. **`swarm/mailbox.rs:191`** — `list_thread_for_reader` с access-check. *(P2-01, backend)*
7. **`chat_queue.rs:74-95`** — убрать defensive `ALTER TABLE attachments_json` (живёт в migration v13). *(cleanup, backend)*
8. **`orchestrator/mod.rs:394-401`** — placeholder через `UPDATE`, не `DELETE+INSERT`. *(P1-07, backend)*
9. **`frontend/AgentTile.tsx`** — `termDisposedRef` + early return в обработчиках. *(P0-01, frontend)*
10. **`frontend/NewWorkspaceModal.tsx`** — debounce 250ms или trigger по Enter/onBlur. *(P1-09, frontend)*
11. **`frontend/PromptsPanel.tsx` + `SshPresetsPanel.tsx`** — `window.confirm` (или общий ConfirmDialog) перед delete. *(P1-16, frontend)*
12. **`frontend/App.tsx`** — `ToastItem` с локальным таймером. *(P1-13, frontend)*
13. **`.github/workflows/ci.yml`** — снять `continue-on-error: true` с frontend lint после baseline. *(P1-17, tooling)*
14. **`.github/workflows/ci.yml`** — pnpm cache через `actions/setup-node` или explicit `actions/cache`. *(P1-17, tooling)*
15. **`frontend/package.json`** — добавить `packageManager: pnpm@10.x` + `engines.node`; создать `.node-version`. *(P1-18, tooling)*
16. **`frontend/tsconfig.app.json`** — постепенно включить `strict: true`. *(P2-05, tooling)*
17. **`db.rs::migrate_one`** — разбить на `migrate_v1`...`v14`. *(maintainability, backend)*
18. **`CHANGELOG.md`** — создать или поменять `releaseBody` в release.yml. *(P1-19, tooling)*

---

## 4. Roadmap по волнам

### Wave 1 — Hotfix & Cleanup (≤ 5 рабочих дней)

**Цель**: закрыть все P0, удалить врущий UI, снять security-gap, базовая CI-гигиена.

- **Day 1** (backend): QW#1, QW#2, QW#3 (decision: implement или delete `stop_chat`), QW#5, QW#7. Ветки атомарные, test-coverage где есть.
- **Day 2** (backend): P0-03 — atomic restore_session (R3 helper кратко) + idempotency token на user_msg / chat_queue ↔ orchestrator_chat link.
- **Day 3** (frontend): QW#9, QW#10, QW#11, QW#12 + P0-04 (SkillsPanel unsubs).
- **Day 4** (joint): P0-01 — переносим подписку `agent://stdout` на уровень zustand-store (один subscriber, маршрутизация по `agent_id`); параллельно backend EOF guard.
- **Day 5** (tooling): QW#13, QW#14, QW#15, QW#18 + R5 cleanup pass (`prompt.v1.rs`, `kill_all`, `shutting_down`, `DEFAULT_READINESS_TIMEOUT_MS`).

**Exit-criteria Wave 1**: `cargo test` + `pnpm lint` зелёные и блокирующие; не осталось ни одного no-op exported command; MCP requires API key для всех tools/call; новый агент с активным выводом не блокирует CPU остальных тайлов.

### Wave 2 — Architecture (1-2 недели)

**Цель**: снять structural-debt R1-R6, закрыть P1.

- **Sprint A** (backend, 5 дн): R3 (`db::with_tx`) → проходом закрывает P1-03, перемигрирует ownership/tasks/restore_session.  
  Параллельно: R2 policy.rs → закрывает P1-05, дедупликация `is_mutating` matches!.  
  Параллельно: R5 cleanup финал.
- **Sprint B** (backend+tooling, 5 дн): R6 ts-rs/specta → закрывает P1-02 (общий `agent_spawn` сервис), P1-18, генерация `.ts` в CI step.  
  Параллельно: R1 async AgentService — большой рефактор, может уехать в Sprint C.  
- **Sprint C** (backend+frontend, 5 дн): R4 WorldStateBuilder → закрывает P1-07.  
  Параллельно: P1-04 разбиение `commands.rs` по доменам.  
  Параллельно frontend: P1-08 (terminal на `exited` state c overlay+respawn), P1-10 (single source of truth для switchTo), P1-11 (CodeEditor подтягивает внешние правки), P1-12 (whitelist навигационных hotkey'ев), P1-15 (memo `AgentTile`).
- **Buffer** (5 дн): P1-06 (Bootstrap фазы), P1-09 (debounce dir browser), P1-13 (ToastItem), P1-14 (touch preventDefault), P1-16 (ConfirmDialog), P1-17 (cargo audit blocking), P1-19 (CHANGELOG/cwd verify).

**Exit-criteria Wave 2**: типы Rust↔TS генерируются, ручная sync исчезла; нет `block_on_safely`-вызовов вне legacy edges; `db::with_tx` обязателен для multi-step ops; UI не врёт пользователю (no-op кнопок нет); `cargo audit` blocking.

### Wave 3 — Polish (ongoing)

**Цель**: P2 + non-critical UX.

- P2-01..P2-15 в порядке impact/effort.
- Pre-commit hooks (lefthook/husky), Prettier vs Biome decision (P2-06).
- Vitest или поддержать `node --test` + расширить coverage (P2-07).
- `redesign.spec.js` — удалить или формализовать в Playwright config (P2-08).
- a11y проход тем (P2-04).
- Rust deps consolidation (P2-10): `dirs` 5→6, `thiserror` 1→2, `rand` 0.8→совместимый major, narrow `tokio` features.

---

## 5. Открытые вопросы для архитектора

Сведены из всех трёх отчётов; помечены источниками.

**Бэкенд-критические**:
1. **Crash-recovery semantics для broker-outlives-PigIDE** (BE). Нужен ли idempotency token на user_msg? Что делать если человек работал с агентом через broker-сокет напрямую пока PigIDE мёртв? — *блокирует чистоту фикса P0-03.*
2. **`current_workspace_id` глобальный** (BE). MCP-клиенты делят контекст. Нужен per-key/per-session? — *блокирует merge крупного MCP-апдейта.*
3. **Cancellation orchestrator-turn** (BE). `stop_chat` no-op; нет CancelHandle в Orchestrator state. — *блокирует honest UI (QW#4).*
4. **Шкала dangerous scope** (BE). `delete_workspace` cascade-удаляет всё (agents+tasks+ownership+chat) — почему не dangerous?
5. **Voice PII at-rest** (BE). `voice_transcripts` хранит text+text_raw открыто, FTS5 индексирует. Single-user или нужен encrypt-at-rest?
6. **Sync std::fs::read в async dispatcher** (BE). `tail_agent` блокирует tokio worker; spawn_blocking для file-tools или явный async?
7. **MCP autostart порт** (BE). Bind fail = только tracing::warn; UI не знает. Нужен event + retry?

**Фронтенд-UX**:

8. **`window.prompt`/`window.confirm` блокируют WebView**. Перейти на стилизованный `<Modal>`?
9. **Voice-кнопка double-trigger** на тач-устройствах. preventDefault везде или общий gesture-handler?

**Tooling/процесс**:

10. **Поддерживаемые версии**: какие Node/pnpm/Rust officially supported? CI=20, README=20+, локально 26 — нужен один источник истины (`.node-version` + `rust-toolchain.toml`).
11. **Release workflow**: re-run всех quality gates на release или package only validated tags?
12. **Windows support для `scripts/build.sh`**: официально Linux/macOS only?
13. **`cargo audit` blocking immediately** или нужен triage advisories?
14. **TS strict + type-aware ESLint**: required gate сразу или warning/baseline?
15. **strip/NO_STRIP**: какая стратегия last-verified для AppImage packaging?
16. **`redesign.spec.js`**: нужен ли вообще? Если да — формализовать с playwright config.
17. **Frontend tests**: stay dependency-free `node --test` или Vitest?

---

## 6. Owner-распределение (сводка)

| Owner | P0 | P1 | P2 | R# | Total |
|---|---:|---:|---:|---|---:|
| **backend** | 3 (P0-01a, P0-02, P0-03) | 8 (P1-01..P1-07, P1-08a) | 2 (P2-01, P2-02) | R1, R2, R3, R4, R5 | 13 issues + 5 R |
| **frontend** | 2 (P0-01b, P0-04) | 9 (P1-08b..P1-16) | 6 (P2-03, P2-04, P2-12..P2-15) | (часть R5, R6 потребитель) | 17 issues |
| **tooling** | 0 | 3 (P1-17, P1-18, P1-19) | 7 (P2-05..P2-11) | R6 (lead) | 10 issues + 1 R |

P0-01, P0-03, P1-08, P1-18, R6 — joint (нужна координация двух команд).

---

*Сводка построена 2026-05-23 на основе AUDIT_BACKEND.md (24,098 bytes), AUDIT_FRONTEND.md (26,650 bytes), AUDIT_TOOLING.md (15,068 bytes).*
