# pigide — Backend & Architecture Audit

Read-only аудит на ветке `reset/main-20260517-181552` (HEAD `cd711e9`). Все цитаты `файл:строка` валидны на момент аудита. Frontend / build-tooling намеренно не покрывались — это другие отчёты.

---

## 1. Карта модулей

`src-tauri/src/` — три бинаря через `Cargo.toml`:

- `pigide` (`main.rs` → `lib.rs::run`) — Tauri-приложение (UI + state).
- `pigide-cli` (`bin/pigide-cli.rs`) — single-instance клиент через unix-socket в `ipc.rs`.
- `pigide-agentd` (`bin/pigide-agentd.rs`) — broker для PTY-агентов, выживающий между запусками PigIDE.

| Модуль | Назначение | Ключевые файлы | Зависит от |
|---|---|---|---|
| `db` | r2d2 + rusqlite пул, миграции v1..v14, KV-settings | `db.rs:514` | — |
| `error` | единый `Error` enum + `From<…>` | `error.rs:60` | — |
| `state` | `AppState`: все Arc-managers, передаётся в Tauri commands | `state.rs:32` | все managers |
| `commands` | 75 `#[tauri::command]` обёрток | `commands.rs:1759` | весь mid-tier |
| `ipc` | unix-socket для `pigide .` (single-instance) | `ipc.rs:265` | `workspace` |
| `agentd/` | broker-клиент + сам broker (engine, server, framing, supervisor, proto, resolve) | `engine.rs:646`, `server.rs:641`, `client.rs:609`, `resolve.rs:446` | `db`, `mcp`, `agent` (resolve) |
| `agent` | sync-facade v2 над broker'ом (`block_on_safely`) | `agent.rs:809` | `agentd::*`, `db` |
| `workspace` | CRUD workspace, layout, paths, `prune_stale_layout` | `workspace.rs:181` | `db`, `layout` |
| `layout` | дерево панелей (split/leaf/empty), сериализация | `layout.rs` | — |
| `tasks` | CRUD задач, `update` инфорсит review-gates + release_all_for_task | `tasks.rs:401` | `db`, `swarm::review`, `swarm::ownership` |
| `chat` | `orchestrator_chat` row-API, `to_api_message`, `delete_after` | `chat.rs` | `db` |
| `chat_sessions` | sessions + `ensure_current` (lazy-create "Main") | `chat_sessions.rs:133` | `db` |
| `chat_queue` | очередь юзер-сообщений, `claim_next` атомарен в TX | `chat_queue.rs:593` | `db`, `path_suggest` |
| `chat_queue_worker` | single-consumer worker, `Notify`-park, `recover_inflight` на старте | `chat_queue_worker.rs:135` | `chat_queue`, `orchestrator` |
| `orchestrator/` | LLM-цикл: prompt + tools + phantom-detect + skills/memory inject | `mod.rs:630`, `tools.rs:736`, `prompt.rs:482`, `providers/anthropic.rs:823`, `providers/omni.rs:97` | `agent`, `workspace`, `tasks`, `memory`, `skills`, `project_resolver` |
| `architect/` | tokio-loop supervisor агентов: classifier+policy → AutoConfirm/AutoChoose/AssignNext/Escalate | `supervisor.rs:467`, `policy.rs:324`, `classifier.rs:276` | `agent`, `tasks`, `workspace` |
| `watcher/` (cfg `watcher`) | Gemini-classifier поверх stdout-агентов с rate-limit | `supervisor.rs:339`, `classifier.rs:423` | `agent`, `db` |
| `mcp/` | HTTP/JSON-RPC сервер + auth (sha256 keys) + audit | `server.rs:428`, `auth.rs:172`, `launcher.rs` | `orchestrator::tools` (dispatch) |
| `memory/` | заметки на диске + FTS5 + wikilink-граф | `service.rs:668`, `note.rs`, `links.rs`, `storage.rs`, `watcher.rs` | `workspace`, `files` |
| `swarm/` | mailbox + ownership + review_gates + rollcall + role | `mailbox.rs:377`, `ownership.rs:367`, `review.rs:241` | `db`, `files`, `workspace` |
| `skills/` | реестр markdown-скиллов, router (kw/llm), composer, hot-reload | `registry.rs:517`, `router.rs:324`, `composer.rs`, `claude_import.rs:793` | `db`, `workspace` |
| `project_resolver/` | fuzzy-резолв "открой <name>", fs-индексер, translit, aliases | `service.rs`, `fuzzy.rs`, `indexer.rs:440`, `parsers.rs:462` | `workspace`, `db` |
| `voice/` | whisper-rs + cpal capture + dictionary + history | `mod.rs`, `capture.rs`, `whisper.rs`, `dictionary.rs`, `history.rs` | `db` |
| `path_suggest` | `@`-mention резолвер + validate в allow-roots | `path_suggest.rs:847` | `workspace`, `files` |
| `files` | sandboxed read/write/list/walk с canonical-prefix-check | `files.rs:427` | — |
| `prompts`, `rooms`, `ssh`, `deeplink`, `events`, `sanitize` | вспомогательные | — | — |

### Поток данных — типичный chat turn

```
UI → commands::send_chat → path_suggest::validate_all
  → chat_queue::enqueue_with_attachments + worker.poke
  → ChatQueueWorker::drain_once → claim_next(TX, status=processing)
  → Orchestrator::run_chat
    → build_messages (history + system + memory + skills inject)
    → LlmProvider::chat_stream (streaming SSE deltas → EV_CHAT_CHUNK)
    → tools::dispatch (per tool_call) → chat::insert(tool_msg)
    → loop до has_tools=false или MAX_ITERATIONS=6
  → mark_done | mark_failed → emit_snapshot
```

### Поток данных — агент (PTY-стрим)

```
Tauri command → AgentManager (sync) → block_on_safely
  → AgentClient (async, NDJSON over unix-socket)
  → pigide-agentd::server::serve_connection
  → Engine::spawn|write|kill → portable-pty + std::thread reader
  → broadcast::Sender<EngineEvent>
  → server-side subscribe-task → Tauri AppHandle::emit(EV_AGENT_STDOUT)
```

---

## 2. ТОП-15 проблем

| # | Sev | Файл:строка | Категория | Описание | Фикс |
|---|---|---|---|---|---|
| 1 | **P0** | `mcp/server.rs:197-207,343-378` | Безопасность / auth | `tools/call` с `key=None` обрабатывается; `is_mutating`/`is_dangerous` срабатывают и режут запись, но read-only tools (`list_workspaces`, `list_agents`, `tail_agent`, `read_memory`, `list_file_owners`...) исполняются **анонимно** на 127.0.0.1:20129. Audit log пишет `key_id=NULL`. Любой локальный процесс читает workspace, агентский stdout, memory без следов в audit. | Запретить anonymous вообще: блок `if req.method != "initialize" && key.is_none() { UNAUTHORIZED }` уже есть, но надо добавить отдельный `read` scope и проверять для не-mutating tools. |
| 2 | **P0** | `agentd/engine.rs:418-438,292-341` | Утечка / FD | `Engine::kill` снимает row из `runtimes`, делает `kill+wait`, дропает writer/master. Reader-thread живёт на склонированном `try_clone_reader` fd — после reap child'а EOF прилетает не моментально. EOF-cleanup делает второй `runtimes.remove` (already-gone) и второй `events.send(Exit)` → фронт может увидеть **двойной EV_AGENT_EXIT**. | В `kill()` дропать reader явно (или `JoinHandle::abort` если было async). В EOF-cleanup слать Exit только если remove реально снял row (`if removed.is_some() { send Exit }`). |
| 3 | **P0** | `chat_queue_worker.rs:104-133` + `orchestrator/mod.rs:290-312` | Crash consistency | `recover_inflight` (`chat_queue.rs:102`) на старте флипает `processing→queued`. Если процесс упал между `chat::insert(user_msg)` и завершением `tool_loop`, `delete_after(user_created_at)` НЕ вызвался, user_msg остался; на restart worker заберёт ту же row → второй `chat::insert(user_msg)` → дубликат у модели. | Связать `chat_queue.id` ↔ `orchestrator_chat.id` (FK или idempotency key). На recover делать `delete_after(queue_item.created_at)` для соответствующей сессии до повторного запуска. |
| 4 | **P0** | `agent.rs:316-374` | Race condition | `restore_session`: `client.list_all()` (snapshot) → `UPDATE agents SET status='exited' WHERE status='running' AND id NOT IN (...)` + UPSERT live. Два разных `pool.get()`, без транзакции. Параллельный broker-spawn в окне → его row помечается exited. UPSERT приходит через event позже, но окно реально. | Обернуть весь блок (`SELECT live` → `UPDATE NOT IN` → `UPSERT`) в `conn.transaction()`. Альтернатива: помечать exited только агентов с `created_at < snapshot_time`. |
| 5 | **P1** | `orchestrator/mod.rs:67-69` + `providers/mod.rs:113-119` | Dead config | `build_provider` хардкодит `OmniProvider("kr/claude-opus-4.7")`; тест `provider_ignores_settings` фиксирует, что `KEY_PROVIDER`/`KEY_ARCH_MODEL`/`KEY_ANTHROPIC_API_KEY` игнорируются. 823 строки `anthropic.rs` (с retries, fallback, prompt-caching) — мёртвый код в runtime, вводит в заблуждение security-аудит. | Либо вернуть выбор по `KEY_PROVIDER`, либо удалить `anthropic.rs` + соответствующие `KEY_ANTHROPIC_*`/`KEY_ARCH_*` константы и комментарии. |
| 6 | **P1** | `commands.rs:140-163` + `orchestrator/tools.rs:354-393` | Дублирование / drift | `spawn_agent` (Tauri command) и `spawn_agent` (orch tool) разошлись: `auto_layout: Option<bool>` есть только в command; `effective_cwd = args.cwd.or(ws.paths.first())` есть только в command — tool не подставляет workspace path. Поведение через UI vs MCP отличается. | Выделить `agent_spawn_with_layout(ws_id, type, count, cwd, auto_layout)` в `agent.rs` или `spawn_service.rs`, вызывать из обоих. |
| 7 | **P1** | `swarm/ownership.rs:42-49` + `tasks.rs:202-258` + повсеместно | Atomicity / транзакции | `acquire`: `INSERT ON CONFLICT DO NOTHING` → отдельный `SELECT task_id` на разных r2d2-conn. `tasks::update`: `SELECT current` → `UPDATE` на разных conn. Каждый `db.get()` — новый conn, между ними другой агент может flip-нуть state. `chat_queue::claim_next` корректно использует TX — это исключение, а не правило. | Внутри одной операции брать **один** conn и оборачивать в `transaction()`. Или `RETURNING` (SQLite ≥3.35). |
| 8 | **P1** | `commands.rs:1759` | God-файл / cohesion | 1759 строк, 75 commands, 25+ `*Args` структур, зависит от 15 модулей. Любой новый endpoint правит этот файл. Тестируется плохо (commands напрямую вызывают managers). Политика дублируется с `mcp/server.rs:39-77`. | Разбить по доменам: `commands/{agents,workspace,memory,voice,tasks,...}.rs`. Использовать sub-`generate_handler!`. Permissions вынести в общий `policy.rs`. |
| 9 | **P1** | `mcp/server.rs:39-77` | Scope drift | `is_mutating` / `is_dangerous` — захардкоженный `matches!()` по именам tool'ов. При добавлении нового tool'а в `tool_definitions` легко забыть — он молча станет доступен с read-only ключом. Тест `mutating_scope_includes_security_sensitive_tools` (`server.rs:411`) ловит лишь подмножество. | Хранить scope/capability в самом `function_tool(...)`. `dispatch_tool` читает оттуда. Тест-генератор: для каждого имени из `tool_definitions()` обязательно есть capability. |
| 10 | **P1** | `lib.rs:175-362` | Bootstrap god | `run()` 350+ строк процедурного init: .env, Wayland env hacks, pool, 8 Arc-managers, skills bootstrap, MCP autostart, deep-link wiring, restore_session в spawned thread, memory watcher, skills watcher, voice hotkey. Порядковые зависимости (`set_app_handle` → restore → emit). Любая ошибка молча `unwrap_or` / `.warn`. | `Bootstrap::new(app).initialize()` с явными фазами и surface-failure в UI (event), а не только tracing. |
| 11 | **P1** | `orchestrator/mod.rs:341-503` | Long method / state | `tool_loop` 162 строки, 5 фаз; mutable `phantom_nag`/`phantom_pending`; placeholder обновляется через `DELETE WHERE id=?` + новый `chat::insert` — теряется ordering при конкурентных insert'ах. Ошибка в одной фазе → `delete_after` всех messages, не атомарно. | Разбить на `prepare_request` / `stream_into_placeholder` / `handle_phantom` / `dispatch_tool_calls`. Phantom-state — отдельный struct. Placeholder через `UPDATE … SET content=?, tool_calls_json=?`. |
| 12 | **P1** | `lib.rs:493-517` + `agent.rs:533` + `engine.rs:122` | Dead semantics | В v2 broker outlives PigIDE, master/reader живут в broker-процессе. `kill_all` no-op + `shutting_down` AtomicBool никогда не выставляется + длинный комментарий про reader-EOF — артефакты v1. На quit достаточно Detach, но `ExitRequested` не делает explicit Detach. | Удалить `shutting_down` в `engine.rs:122` и `kill_all`-no-op в `agent.rs:533`. На `RunEvent::ExitRequested` — explicit `Detach` op к broker'у. |
| 13 | **P2** | `swarm/mailbox.rs:191-213` | Auth gap | `list_thread(thread_id)` возвращает все mail в треде без проверки, кто читает. `list_for_reader` делает access-check для inbox, но `list_thread` — нет. | Добавить `list_thread_for_reader(reader_agent_id, thread_id)` с проверкой участия (`from_agent_id=reader OR to_addr=reader OR to_addr='role:'+role`). |
| 14 | **P2** | `tasks.rs:202-258` + `swarm/review.rs:146` | Race на review-gate | `task_completable` читает `list_for_task` на одной conn → `UPDATE status='complete'` на другой. Между ними другой агент открыл pending-gate → задача всё равно станет complete. | Enforcement в самом UPDATE: `UPDATE tasks SET status='complete' WHERE id=? AND NOT EXISTS (SELECT 1 FROM review_gates WHERE task_id=? AND verdict!='pass')`, всё в одной TX. |
| 15 | **P2** | `path_suggest.rs:125-148` + `files.rs:172-200` | Производительность / hot-path syscalls | `validate_workspace_write_path` дёргает `canonicalize_allowed_roots` на каждом read/write/list/walk/`@`-suggest validate/ownership::acquire — `metadata` syscall на каждый workspace path. На воркспейсе с 5+ путями + частый suggest = десятки syscall'ов на одном поиске. | Кешировать canonicalized roots в `WorkspaceManager` (или отдельный `RootsCache`). Инвалидировать при `set_paths`/`delete`/`rename`. |

---

## 3. Архитектурные рекомендации

### R1. Async-trait `AgentService` вместо sync-facade

`agent.rs:809` — гибрид sync API + async client + DB-mirror + AppHandle emit. Sync обёртка через `block_on_safely` (`agent.rs:67-72`) нужна потому, что Tauri commands исторически sync. На самом деле commands помечены `async` и могут `.await` без `block_in_place`. Watcher и architect живут в tokio (`tauri::async_runtime::spawn`) — тоже могут.

**Действие**: ввести `AgentService: Send + Sync` async-trait. Перевести вызовы из orchestrator/architect/watcher на `.await`. Удалить `block_on_safely`. Это убирает целый класс багов "Cannot start a runtime from within a runtime" и делает блокирующее IO видимым для tokio scheduler.

### R2. Единый policy / capability слой

Сейчас три набора политики: Tauri commands (нет — всё открыто фронту), MCP server (`is_mutating`/`is_dangerous` matches!), internal callers (orchestrator-tools — без проверок).

**Действие**: `policy.rs` с `Capability` enum. `function_tool(name, desc, params, capability)`. Dispatcher читает capability и применяет независимо от поверхности. Тестом покрыть «каждый tool в `tool_definitions()` имеет capability».

### R3. Обязательный `db::with_tx(pool, |tx| {...})` helper

Сейчас 90% операций — `let conn = pool.get()?; conn.execute(...)`. Многошаговые (workspace.create + initial layout, task update + ownership release, restore_session UPDATE+UPSERT, ownership::acquire + verify-owner) разбросаны на множественные `pool.get()` и не атомарны под FK.

**Действие**: ввести `db::with_tx`. Запретить `pool.get()` вне data-access слоёв (lint / convention). SQLite WAL держит read-consistent snapshot — это закроет P0#4, P1#7, P2#14 одной механикой.

### R4. `WorldStateBuilder` с event-driven invalidation

`Orchestrator::build_system_prompt` (`mod.rs:113-174`) каждый turn собирает workspace+agents+tasks через 3 manager'а. Это N+1 для `agents` (один list per workspace). С учётом skills + memory inject — 4-7 SQL-запросов на каждый turn.

**Действие**: `WorldStateBuilder` подписан на `workspace.changed` / `agent.spawned` / `agent.exit` / `task.updated` и держит готовый prompt-блок. Кеш инвалидируется только на event'ах. Снимет также P1#11 (длинный `tool_loop`) — preparation станет дешёвой.

### R5. Удалить мёртвую инфраструктуру

- `orchestrator/prompt.v1.rs` (306 строк) — `prompt.rs` уже актуальный.
- `agent::DEFAULT_READINESS_TIMEOUT_MS` с `#[allow(dead_code)]` (`agent.rs:78-79`).
- `engine::shutting_down` AtomicBool — никогда не выставляется (P1#12).
- `commands::stop_chat` (`commands.rs:370`) — комментарий "no-op until re-implemented", команда экспортируется во фронт, всегда возвращает `false` (UI кнопка stop врёт пользователю).
- `anthropic.rs` provider, если оставляем хардкод OmniRouter (P1#5).

**Действие**: серия чистящих PR. Аудит безопасности и onboarding не должны упираться в призраков.

---

## 4. Quick wins (≤1 час каждый)

1. **`engine.rs:336-340`** — в EOF-cleanup слать `Exit` event только если `runtimes.remove` реально что-то снял (`if let Some(_) = ...`); снимет двойной `EV_AGENT_EXIT`.
2. **`mcp/server.rs:343-378`** — `read` scope-check для non-mutating tools, ~5 строк.
3. **`commands.rs:370`** — реализовать `stop_chat` (CancelHandle в Orchestrator) или убрать и удалить из фронт-вызовов.
4. **`db.rs:39`** — `Pool::builder().connection_timeout(5s).min_idle(2)`. Сейчас под нагрузкой (chat_queue + watcher + architect + memory_watcher одновременно) можно ждать вечно.
5. **`swarm/mailbox.rs:191`** — `list_thread_for_reader` с access-check (P2#13).
6. **`chat_queue.rs:74-95`** — убрать defensive `ALTER TABLE … ADD COLUMN attachments_json`; колонка живёт в migration v13 (`db.rs:448-452`).
7. **`orchestrator/mod.rs:394-401`** — placeholder через `UPDATE`, а не `DELETE+INSERT` (теряется ordering при конкурентных вставках).
8. **`tasks.rs:155-200`** — заменить `format!("AND status=?{}", params.len()+1)` на `rusqlite::named_params!`. Уйдёт класс potential SQL-injection (статус валидируется enum'ом — тонкий лёд).
9. **`db.rs::migrate_one`** — разбить на `migrate_v1`...`migrate_v14` (450 строк → читаемые diff'ы, упростит ревью новых миграций).
10. **`lib.rs:222`** — `VoicePipeline::new_with_handle(app)` вместо postfix `set_app_handle` (нет тестов, легко забыть порядок).

---

## 5. Открытые вопросы (требуют решения архитектора)

1. **Crash-recovery semantics для broker-outlives-PigIDE.** PigIDE упал во время `chat_queue.processing`, broker жив, агенты живы. На restart `recover_inflight` возвращает row в queue → второй turn → второй `chat::insert(user_msg)`. Нужен ли idempotency token? Что делать, если человек вручную работал с агентом через broker-сокет, пока PigIDE мёртв? (P0#3 — но решение архитектурное.)
2. **`current_workspace_id` — глобальная KV-настройка**, но MCP принимает команды от нескольких клиентов одновременно. Если два MCP-клиента одновременно делают `switch_workspace`, последний выигрывает, другой видит чужой контекст. Нужен ли per-key/per-session current_workspace? Особенно опасно с `delete_workspace`.
3. **Cancellation orchestrator-turn.** `commands::stop_chat` no-op, в Orchestrator нет CancelHandle. UX-блокер: пользователь не может отменить дорогой turn. Когда планируется — и каков design (cancel signal через mpsc, abort на стрим, marker в БД)?
4. **Шкала опасности в scopes.** `delete_task` в `is_dangerous` (`mcp/server.rs:74`). Но `delete_workspace` cascading-удаляет всё (agents + tasks + ownership + chat_messages). Иерархия неочевидна — нужна явная.
5. **Voice — PII storage.** `voice_transcripts` хранит `text` + `text_raw` без шифрования; FTS5 индексирует. На multi-user системе серьёзный риск. Targeting single-user или нужен encrypt-at-rest?
6. **Sync IO в async dispatcher.** `orchestrator::tools::dispatch` async, но `tail_agent` (`tools.rs:497-510`) и аналоги делают sync `std::fs::read`. На большом log'е блокирует tokio worker. Поднимать `spawn_blocking` для file-reading tools? Где граница?
7. **`ipc.rs::handle_stream` — каждый OpenPath полный `WorkspaceManager::list()`.** При 50+ workspace'ах + частом `pigide .` — линейный поиск по `paths`. Кешировать? Не критично сейчас, но станет.
8. **MCP autostart порт 20129 захардкожен (`lib.rs:272`)** с override через `mcp.port`, без проверки конфликта (другой процесс держит). Bind fail = только `tracing::warn`. UI не показывает статус — пользователь не знает, что MCP мёртв. Нужно ли surface'ить через event и дать retry-кнопку?

---

*Аудитор: agy (workspace pigiderefactor). Полный текст также отправлен mail-ом на `role:coordinator`, thread `audit-backend`, message id `039a4d8e-5119-4e99-8a3c-aa72c7305a28`.*
