# PigIDE — Полный аудит проекта

**Дата:** 2026-05-31
**Аудитор:** claude-code agent (`bebc4037`), task `b4f68e60`, thread `pigide-audit`
**Объект:** `/home/camer/pigide` (Rust/Tauri 2 + React 19/TS) — desktop IDE с тайловыми CLI-агентами, голосовым оркестратором и multi-agent swarm
**Ревизия:** ветка `main`, HEAD `399a625`, при наличии незакоммиченных изменений (`agentd/*`, часть frontend)
**Методология:** промт «KingPrompt» не был доступен (нет ни как skill, ни на диске; запрос отправлен координатору, ответа за время аудита не поступило). Применена строгая структурная методология аудита, организованная по KingPrompt-секциям: карта системы → tool surface → runtime → сборка/качество → безопасность → приоритизированные находки → рекомендации. **Каждое существенное утверждение подтверждено чтением кода с указанием `файл:строка`; топ-находки перепроверены лично, а не приняты на веру от субагентов.**

> ⚠️ Это аудит-отчёт. **Исходный код не изменялся.** Создан только этот файл.

---

## 0. TL;DR — executive summary

PigIDE — это амбициозный, на удивление **чистый** по меркам кодовой базы такого размера проект (~32 000 строк Rust в 10+ подсистемах + ~10 000 строк React). Архитектура продуманная: брокер-процесс `pigide-agentd` владеет PTY и переживает перезапуск UI, оркестратор с tool-loop, swarm-координация через SQLite, двухстадийный memory-ingest, голосовой пайплайн на whisper-rs. Код почти не содержит `TODO/FIXME/HACK`, `unwrap()` в продакшен-путях единичны и защищены, тестов много (унит-тесты почти в каждом модуле + 5 интеграционных файлов).

**Но** ключевая проблема безопасности — **модель доверия webview**: `csp: null`, отсутствие per-command capability-гейтинга, и несколько команд, принимающих пути/бинарники/SSH-аргументы без валидации. Любой XSS во webview = полный доступ к бэкенду и, через цепочку настроек, **произвольное исполнение кода** на машине пользователя. Плюс — **подтверждённая XSS** в рендере markdown ответов LLM (`javascript:` URL).

| Сводка | Значение |
|---|---|
| Подсистем Rust | 13 (+watcher feature-gated) |
| Tauri-команд | 120 (все зарегистрированы, **0 capability-гейтов**) |
| MCP-инструментов | ~40 (workspace/agent/task/memory/swarm + watcher_status) |
| Критичных находок | **4** |
| High находок | **8** |
| Medium находок | 11 |
| Прод-`unwrap`/`expect` (не тесты) | ~6, все защищены/инфаллибл |
| `TODO/FIXME/HACK` в коде | ~0 |
| Орфанные (некомпилируемые) модули | 2: `orchestrator/budget.rs`, `orchestrator/prompt.v1.rs` |

**3 вещи, которые надо сделать в первую очередь:** (1) починить XSS в `Markdown.tsx:29`; (2) валидировать `workspace.paths` при создании — это корень sandbox-escape; (3) подключить и применить `budget.rs` (token-budgeting написан, но **не скомпилирован** — `mod budget;` нигде не объявлен).

---

## 1. Карта системы и архитектура

### 1.1. Топология процессов

```
┌──────────────────────────────────────────────────────────────┐
│  PigIDE (Tauri app)                                            │
│  ┌────────────────────┐         ┌──────────────────────────┐  │
│  │ Webview (React 19) │◄──IPC──►│ Rust core (pigide_lib)   │  │
│  │  zustand store      │ invoke  │  AppState: 13 менеджеров │  │
│  │  xterm tiles        │ events  │                          │  │
│  └────────────────────┘         └───────────┬──────────────┘  │
│                                              │                 │
│   ┌──────────────┐   axum HTTP :20129    ┌───┴────┐            │
│   │ Claude tiles │◄──── /mcp (Bearer) ───┤ PigMCP │            │
│   └──────┬───────┘                       └────────┘            │
└──────────┼─────────────────────────────────────────────────────┘
           │ unix socket (NDJSON, 0600)
   ┌───────┴────────────────┐
   │ pigide-agentd (брокер) │  владеет всеми PTY, переживает Cmd+Q
   │  Engine: HashMap<id,RT>│  fork-exec CLI-агентов
   └────────────────────────┘
```

- **Три бинарника** (`src-tauri/Cargo.toml`): `pigide` (GUI), `pigide-cli` (`pigide-cli .` передаёт workspace через unix-socket в запущенный инстанс — `ipc.rs`), `pigide-agentd` (брокер PTY).
- **Wiring всего** — `lib.rs:88-534`. `AppState` (`state.rs`) держит 13 `Arc`-менеджеров. Фоновые задачи стартуют в `setup` (`lib.rs:229-375`): architect loop, chat-queue worker, smart-ingest worker, watcher (feature), memory watcher, skills watcher, MCP autostart, session restore.

### 1.2. Подсистемы Rust (по строкам)

| Подсистема | Назначение | Ключевые файлы | LOC | Тесты |
|---|---|---|---|---|
| `commands` | 120 Tauri-хендлеров (фасад) | commands.rs | 1996 | через нижележащие модули |
| `orchestrator` | LLM tool-loop, провайдеры | mod, prompt, tools, providers/anthropic, providers/omni | ~3000 | phantom, anthropic, providers |
| `agentd` | брокер PTY (v2 архитектура) | engine, server, client, proto, framing, resolve, supervisor | ~3400 | сильное покрытие, все 8 файлов |
| `memory` | PigMemory: заметки + 2-стадийный ingest | service, note, storage, ingest/* | ~3500 | сильное |
| `agent` | AgentManager (клиент брокера) | agent.rs | 866 | 6 |
| `skills` | реестр/роутер навыков | registry, router, composer, claude_import | ~2200 | сильное |
| `db` | r2d2+rusqlite, 17 миграций | db.rs | 659 | **нет тестов миграций** |
| `voice` | дикт-пайплайн whisper-rs | capture, whisper, cloud, inject, download | ~2000 | средне |
| `project_resolver` | fuzzy-поиск проектов на диске | indexer, resolver, parsers, fuzzy, translit | ~1800 | хорошо |
| `swarm` | mailbox/ownership/review/rollcall | mailbox, ownership, review, tools | ~1700 | сильное |
| `architect` | regex-supervisor авто-действий | policy, classifier, supervisor | ~1100 | classifier+policy (supervisor — **нет**) |
| `watcher` | Gemini-классификатор stdout | classifier, supervisor, rate_limiter | ~1000 | сильное + интеграционный |
| `chat_*` | очередь/сессии/история чата | chat_queue, chat_sessions, chat_queue_worker | ~1700 | сильное |
| `mcp` | HTTP JSON-RPC сервер | server, auth, launcher | ~900 | средне (нет e2e) |

### 1.3. Frontend (React 19 / TS ~6 / Vite 8)

- Один глобальный zustand-store (`state/store.ts:95-229`, ~30 полей) + отдельный `useArchitectStore` (`state/architect.ts`).
- Весь IPC централизован в `state/ipc.ts` (единственная обёртка над `invoke`), типы зеркалят Rust вручную (`state/types.ts`).
- Топ-5 компонентов по размеру: `PigMemoryWorkbench.tsx` (1154), `OrchestratorPanel.tsx` (835), `AgentTile.tsx` (621), `NewWorkspaceModal.tsx` (549), `SkillsPanel.tsx` (415).
- xterm.js привязан к `agent://stdout` (base64 → Uint8Array), очистка слушателей корректная (`AgentTile.tsx:212-225`).

### 1.4. Поток данных «сообщение пользователя»

```
send_chat → chat_queue (SQLite FIFO) → ChatQueueWorker (1 consumer)
  → Orchestrator::run_chat → build_messages (+WORLD STATE +memory hot-cache +skills)
  → provider (OmniRouter :20128, model kr/claude-opus-4.8) → tool_loop (MAX_ITER=6)
  → tools::dispatch (workspace/agent/task/memory/swarm) → результат назад в историю
```

---

## 2. MCP tool surface

### 2.1. Что экспонируется

**Транспорт:** HTTP, `POST /mcp` + `GET /healthz`, по умолчанию loopback `127.0.0.1:20129` (`mcp/server.rs:128-132`, autostart `lib.rs:269-304`). JSON-RPC методы: `initialize` (без auth), `tools/list`, `tools/call`, `prompts/list`, `prompts/get`, `ping` (`mcp/server.rs:209-249`).

**~40 инструментов** делегируются в `orchestrator::tools::dispatch` (`mcp/server.rs:343-354`): workspace CRUD, `spawn_agent`/`close_agent`/`send_to_agent`/`wait_for_agent_idle`/`tail_agent`, task CRUD/assign, project-resolver, + `memory::tools` + `swarm::tools` (mailbox/ownership/review).

**Опасные инструменты** (`mcp/server.rs:71-76`): `spawn_agent`, `send_to_agent`, `delete_workspace`, `delete_memory`, `delete_task`.

### 2.2. Аутентификация / авторизация

- Bearer-токен `Authorization: Bearer pk_<43 base64url>`, в БД только SHA-256 (`mcp/auth.rs:30-71`). Плейнтекст показывается один раз. **Это хорошо.**
- Скоупы `read,mutate,dangerous`. Проверка в `mcp/server.rs:313-322`.
- **Аудит-лог** каждого `tools/call` со статусом `ok|denied:scope|err` (`mcp/server.rs:381-400`). **Хорошо.**
- `is_mutating`/`is_dangerous` — **ручные `matches!`-списки** (`mcp/server.rs:39-76`), легко рассинхронятся при добавлении инструмента в `orchestrator/tools.rs`. (Прошлый аудит уже ловил пропуск 7 инструментов — см. `audit/FIXES_APPLIED.md` #4.)
- Ошибки auth/dispatch **утекают внутренние строки** клиенту (`mcp/server.rs:192,367-378`) — могут содержать пути ФС.

### 2.3. Tool surface координируется со swarm

Swarm-инструменты (`send_mail`, `broadcast`, `read_mailbox`, `claim_files`, review-gates) пишут/читают SQLite. `read_mailbox` ограничивает доступ через `validate_mailbox_access` (`swarm/mailbox.rs:215-230`). Но (унаследовано из прошлого аудита, D7/D8): `send_mail` не привязывает identity отправителя к MCP-вызову — любой обладатель ключа может слать почту от имени любого зарегистрированного агента.

---

## 3. Agent runtime / терминалы / UI

### 3.1. Брокер `pigide-agentd` (v2)

Сильная сторона архитектуры: PigIDE больше не владеет PTY — брокер живёт отдельно, поэтому `kill_all` на Cmd+Q **намеренно no-op** (`agent.rs:550-557`), агенты переживают перезапуск UI. `restore_session` (`agent.rs:332-395`) пересинхронизирует живых агентов в SQLite-зеркало.

- **Auth на сокете отсутствует** — защита только ФС: `chmod 0600` на сокет (`bin/pigide-agentd.rs:80-86`), single-instance через `flock`. Модель угроз: одна машина, один пользователь. Приемлемо **при условии** жёстких прав на сокет.
- **`bin_path` принимается от клиента без allowlist** (`agentd/engine.rs:217-256`) — брокер фактически fork-exec gateway. Любой, кто может говорить с сокетом, получает RCE как пользователь.
- **`reuse_id` не санитизируется** и идёт в `format!("{}.log", agent_id)` (`agentd/engine.rs:194-196,289-302`) → path traversal записи/чтения лог-файла.
- Логи агентов **растут без ротации** (`agentd/engine.rs:298-310`); `LogTail` аллоцирует `Vec` пропорционально запрошенному размеру.
- Незакоммиченные изменения в `agentd/*` — проверено: это **чистый rustfmt-реформат** (перенос строк), без полуготовой логики. `ListPersistedRunning`/`RespawnPersisted` — задокументированные заглушки протокола миграции (`agentd/server.rs:218-263`).

### 3.2. UI терминалов

- xterm: scrollback 5000, реплей последних 64 KiB лога при mount (`AgentTile.tsx:172-184`), очистка слушателей полная.
- **Неэффективность O(N²):** каждый `AgentTile` И каждый `useAgentSummary` открывают свою подписку на `agent://stdout` и фильтруют по id в колбэке (`AgentTile.tsx:192-202`, `hooks/useAgentSummary.ts:92-103`). При N тайлах burst от одного агента = N×M колбэков.
- **Нет React error boundaries нигде** — ошибка рендера в любом потомке роняет весь UI (особенно опасно для `Markdown`, графов на canvas, монтирования xterm).

---

## 4. Сборка, зависимости, конфиг, тесты

### 4.1. Сборка и зависимости

- Workspace Cargo, `resolver=2`, release-профиль `opt-level="s"`, LTO, `strip=false` (с комментарием — linuxdeploy падает на стрипнутом бинаре).
- **Whisper GPU-бэкенды** через features (`gpu-cuda` дефолт по комментарию, но `default = ["custom-protocol","watcher"]` — то есть GPU надо включать явно). Аккуратно.
- Зависимости свежие и адекватные: tauri 2, axum 0.7, rusqlite 0.31 (bundled), whisper-rs 0.16, reqwest 0.12. Dev: wiremock, tempfile, tokio-test.
- Frontend: React 19.2, Vite 8, TS ~6, минификатор `oxc` (очень новый). `@codemirror/*` (~13 пакетов) + `@xterm/*` — самые тяжёлые.

### 4.2. Конфиг — находки

| Что | Где | Оценка |
|---|---|---|
| `"csp": null` | `tauri.conf.json:28` | **риск** — нет Content-Security-Policy, любой загруженный контент исполняется |
| `updater.pubkey: ""` | `tauri.conf.json` | **риск** — пустой ключ подписи апдейтера; endpoint `github.com/pigide/pigide` (репо может не существовать) |
| capabilities | `capabilities/default.json` | только core/event/window/updater/deep-link; **нет per-command гейтинга** |
| `.env` | gitignored ✅, не в git ✅ | живые ключи лежат локально — корректно |
| `.mcp.json` | только context7 | ок |

### 4.3. Тесты

- **Rust:** унит-тесты почти в каждом модуле (sanitize 13, chat_queue 16, path_suggest 14, architect policy/classifier ~24, и т.д.) + 5 интеграционных (`sanitize`, `project_resolver_e2e`, `skills`, `watcher`, `bench_whisper`). `watcher_integration.rs` использует wiremock для мока Gemini — единственный кросс-модульный e2e.
- **Пробелы:** нет тестов миграций БД (`db.rs`), `architect/supervisor.rs` (tokio-loop, execute, assign_next), orchestrator `tool_loop`/`build_messages` (e2e), `commands.rs` (только через нижние модули).
- **Frontend:** ровно **один** тест-файл (`scripts/pathMentionHelpers.test.ts`, ~22 кейса). Не покрыты: store, layout tree, OSC 133 parser, markdown-санитизация (где XSS!), wikilink.

---

## 5. Безопасность — приоритизированные находки

> Severity: **CRITICAL** (RCE/sandbox-escape/XSS с реальным путём эксплуатации) · **HIGH** (утечки/обходы при умеренных условиях) · **MEDIUM** · **LOW**.
> Все строки перепроверены лично, кроме помеченных «(via subagent)».

### CRITICAL

**C1 — Модель доверия webview: нет CSP + нет capability-гейтинга → XSS = полный бэкенд.**
`tauri.conf.json:28` (`csp: null`) + `capabilities/default.json` (нет per-command allowlist) + все 120 команд в `generate_handler!` (`lib.rs:388-507`). Любой XSS во webview (а он есть — см. C2) вызывает любую Tauri-команду. Через настройки (C3) это эскалируется до RCE. **Это корневая проблема, от которой зависят C2/C3/C4.**
→ Включить строгую CSP, рассмотреть per-command capabilities, трактовать webview как недоверенную границу.

**C2 — XSS в рендере markdown ответов LLM (`javascript:` URL).** *(перепроверено лично)*
`frontend/src/components/Markdown.tsx:29`:
```js
html = html.replace(/\[([^\]]+)\]\(([^)]+)\)/g, '<a href="$2" ...>$1</a>');
```
URL берётся из `$2` дословно, без проверки схемы. `escapeHtml` запускается раньше и не трогает `javascript:` (там нет `&<>"'`). Полезная нагрузка `[click](javascript:alert(1))` → кликабельная XSS. Рендерится для **каждого ответа ассистента/агента** в `OrchestratorPanel`. Рядом, в `pigmemory/MarkdownPreview.tsx:96-102`, схема уже ограничена `https?:` — то есть фикс известен команде, просто не применён в этом рендере.
→ Ограничить схему `http(s):` (как `BrowserPanel.tsx:6-13 isSafeUrl`), плюс добавить CSP как defense-in-depth.

**C3 — Sandbox-escape: `workspace.paths` не валидируется при создании.** *(перепроверено лично)*
`workspace.rs:94-114` — `create()` пишет `paths` в `paths_json` без единой проверки (не абсолютный, существует ли, владелец). `current_workspace_roots` (`commands.rs:914-930`) — единственный гейт для read/write — берёт именно эти `paths`. Webview-вызов `create_workspace`/`open_project`/`set_paths` с `paths=["/"]` или `["/etc"]` превращает `read_file`/`write_file`/`walk_files` в чтение/запись по всей ФС. `files::validate_*` (`files.rs:126-170`) защищает от traversal **внутри** root, но не валидирует сам root.
→ Валидировать пути при создании workspace: абсолютные, существуют, в пределах `$HOME` либо явный allowlist.

**C4 — Эскалация webview→RCE через настройки `bin.<type>` / `args.<type>` и SSH `args`.** *(перепроверено через resolve.rs + ssh.rs)*
`bin.<type>` и `args.<type>` пишутся webview через `set_setting` и идут прямо в spawn брокера (`agentd/resolve.rs:58-210`). `set_setting(bin.claude, /tmp/evil)` → следующий спавн claude запускает `/tmp/evil`. Аналогично SSH-пресеты: `args` Vec идёт дословно в `ssh` (`ssh.rs:142-163`), что допускает `-o ProxyCommand=...` (документированный RCE-примитив). Шелла нет (execvp), но флаговая инъекция = исполнение команд.
→ Allowlist бинарников/флагов, либо подтверждение пользователя на изменение `bin.*`/`args.*`.

### HIGH

**H1 — Token-budgeting написан, но не скомпилирован (орфан-модуль).** *(перепроверено лично)*
`orchestrator/budget.rs` (342 строки, `compact`, `should_compact`, тесты) **никогда не объявлен** через `pub mod budget;` — в `orchestrator/mod.rs:1-5` его нет. Файл не входит в крейт; его тесты не запускаются. `build_messages` (`mod.rs:227-327`) не вызывает компакцию. Единственные ограничители контекста — `HISTORY_LIMIT=60` и `MAX_ITERATIONS=6`. Длинная сессия растёт, пока провайдер сам не вернёт 4xx. (В прошлом аудите это «отложенный D1» — но инфраструктура уже написана, её просто забыли подключить.)
→ Добавить `pub mod budget;`, вызвать `compact` в `build_messages`.

**H2 — Нет глобального лимита расходов LLM.** Anthropic-провайдер с ретраями существует, но в рантайме **хардкод на OmniRouter** (`providers/mod.rs:108-115`). У OmniRouter-клиента нет ретраев (`omni.rs:40-63`). chat-queue worker дренит очередь без пауз; tool-loop = до 6 LLM-вызовов на сообщение. Watcher Gemini — только per-agent RPM, без глобального потолка. Runaway-цикл сжигает кредиты.

**H3 — Prompt-injection: широкая неэкранированная цепочка untrusted→system prompt.** *(via subagent, выборочно перепроверено)*
Имя workspace, title/instructions задач, id агентов, тела memory-заметок, тела почты — всё попадает в system-prompt оркестратора или в `[Tool result of …]` без экранирования (`orchestrator/mod.rs:140-180,189-225,262-268`). Цепочка усиливается: (а) PTY-stdout агента → smart-ingest LLM (`memory/ingest/chat_chunk.rs` → `smart.rs`); (б) Gemini-классификация произвольного stdout → `prompt_text` в mailbox координатора (`watcher/supervisor.rs:228-244`). Любой агент, печатающий в stdout, влияет на будущие промпты.

**H4 — OmniRouter error-path логирует полное тело запроса.** `orchestrator/client.rs:85-90,144-149` (`body sent={}`) — на каждом не-2xx в `tracing::error!` уходит весь промпт: чат, world-state, memory hot-cache, каталог инструментов. Ключа там нет, но PII/контент проекта — да. Аналогично `client.rs:67` на `debug`.

**H5 — Auto-mint tile-токена со скоупом `dangerous` в плейнтексте.** *(перепроверено лично)*
`mcp/launcher.rs:54-61` — токен `tile-claude` безусловно получает `read,mutate,dangerous` и пишется в `settings.mcp.tile_token` **плейнтекстом**, плюс в `<cwd>/.mcp.json` через `std::fs::write` без явного `chmod` (умолчательный umask, обычно 0644) (`launcher.rs:107-156`). Кто прочитал settings-строку или `.mcp.json` — получил full-power MCP (а значит `spawn_agent` с произвольным bin → RCE, см. C4).

**H6 — `agent_log_tail` path traversal через `agent_id`.** *(перепроверено лично)*
`agent.rs:251-265`: `log_dir().join(format!("{}.log", agent_id))`, `agent_id` от webview без UUID-проверки. `agent_id="../../etc/passwd"` → чтение `<log_dir>/../../etc/passwd.log`. Суффикс `.log` сужает импакт, но на многопользовательской машине значим.

**H7 — `mcp_start { bind_all: true }` снимает loopback и выставляет JSON-RPC в LAN.** *(перепроверено лично)*
`commands.rs:796-799` — `[0,0,0,0]`. В сочетании с дефолтным `read,mutate` (H8) и tile-токеном (H5) — сетевой RCE-вектор.

**H8 — Дефолтный скоуп ключа при пустом списке = `read,mutate`.** *(перепроверено лично)*
`mcp/auth.rs:49-53` — `mcp_create_key` без скоупов создаёт мутирующий ключ. Безопаснее дефолт `read`.

### MEDIUM

- **M1 — Whisper-модель скачивается без проверки подписи/SHA** (`voice/download.rs:75,141-186`), хардкод HF URL, потом `mmap` в whisper.cpp. MITM/компромет HF-зеркала → произвольный GGML → потенциальный RCE через парсер. *(via subagent)*
- **M2 — Voice inject авто-печатает stdout Whisper в фокусное окно** (`voice/inject.rs` + `mod.rs:204-213`), без фильтра контента; при фокусе на sudo-prompt текст уходит туда. Защита — opt-in + проверка фокуса.
- **M3 — Cloud STT API-ключ в SQLite плейнтекстом** (`voice/cloud.rs:70`); модуль — скелет, WS-движок не реализован.
- **M4 — Нет транзакций вокруг multi-statement записей**: `agent.rs:333-394`, `tasks.rs:228-258` (релиз локов до UPDATE — при сбое UPDATE локи уже сняты), `chat_queue_worker.rs:114-128`. Окна частичного применения.
- **M5 — `chat_queue` неограничен** (`chat_queue.rs:117-186`) — runaway frontend заполняет SQLite. Есть только дедуп подряд идущих дублей.
- **M6 — Утечка внутренних ошибок клиентам MCP** (`mcp/server.rs:192,367-378`) — пути ФС/детали БД.
- **M7 — `migration::walk` следует симлинкам** (`memory/migration.rs:82-95`) — симлинк в `.pigmemory/` ведёт к чтению/перезаписи внешнего `.md`. *(via subagent)*
- **M8 — `add_project_alias`/`mcp_register_cwd` пишут файлы в любую writable-директорию** (`commands.rs:866-880,1976-1985`). Низкий импакт (фиксированные имена), но это неаутентифицированный file-create примитив.
- **M9 — `tsconfig.app.json` без `strict`** (нет `strictNullChecks`/`noImplicitAny`) на 10k строк фронта.
- **M10 — Single-slot abort-handle оркестратора** (`orchestrator/mod.rs:43,419`) — второй параллельный `run_chat` молча затирает cancel-хендл первого.
- **M11 — `prompt.v1.rs` — орфан-файл** (306 строк, ссылается на удалённые инструменты), не объявлен модулем, дрейф мёртвого кода.

### LOW

- **L1** — `updater.pubkey` пустой + endpoint на возможно несуществующий репо (`tauri.conf.json`).
- **L2** — drag-drop в PTY использует POSIX-only квотинг (`AgentTile.tsx:32-38`), ломается на Windows.
- **L3** — лог-файлы агентов без ротации (`agentd/engine.rs:298-310`).
- **L4** — ~40 silent `.catch(() => undefined)` на фронте скрывают операционные ошибки от пользователя.
- **L5** — fallback-модель Anthropic `claude-opus-4`/`claude-opus-4-5` (`providers/mod.rs:89-91`) могут не существовать в каталоге (актуально, только если переключиться обратно на Anthropic).
- **L6** — watcher плодит по треду на источник без join (`skills/watcher.rs`, `memory/watcher.rs`).

---

## 6. Качество кода

**Сильные стороны (заслуживают упоминания):**
- Прод-`unwrap()/expect()/panic!` почти отсутствуют: ~6 на весь бэкенд, все либо на статических regex (`Lazy::new`), либо защищены проверкой строкой выше, либо инфаллибл (`reqwest` build). Это редкая дисциплина.
- `TODO/FIXME/HACK` ≈ 0 в коде. Комментарии — нарративные, объясняют «почему» (напр. развёрнутое объяснение Cmd+Q/`kill_all` в `lib.rs:511-527`).
- Path-валидация (`files.rs`, `memory/storage.rs`) — слоистая, с каноникализацией и тестами на traversal/симлинки. `project_resolver` корректно пропускает симлинки и ограничивает глубину.
- Защита от деструктива в `architect` (`policy.rs`/`classifier.rs`) — `rm -rf`, force-push, prod-deploy эскалируются, а не авто-подтверждаются; есть тесты, включая русские варианты.
- Прошлый аудит реально применён: 15 фиксов в `audit/FIXES_APPLIED.md` (5 CRITICAL), и я подтвердил их наличие в коде (scope-enforcement, slug-traversal, body-limit и т.д.).

**Слабые стороны:**
- **Орфан-модули** (`budget.rs`, `prompt.v1.rs`) — написанный код вне компиляции. `budget.rs` особенно болезнен: критичная для расходов фича существует, но выключена невидимо.
- Ручные `matches!`-списки опасных/мутирующих инструментов — источник дрейфа.
- `commands.rs` (1996 строк) и `PigMemoryWorkbench.tsx` (1154) — кандидаты на декомпозицию.
- Дублирование narrow-типа `agent_type` в 5+ местах фронта (`state/types.ts:22` хранит `string`, а сужают на месте вызова).
- Архитектурное именование путает: `architect` (regex-supervisor, без LLM) vs `watcher` (Gemini-supervisor) vs `orchestrator` (основной LLM) — три «надзирателя» с пересекающейся семантикой.

---

## 7. Рекомендации (приоритизированные, конкретные)

### Сделать сейчас (CRITICAL/HIGH, малый-средний объём)

1. **`Markdown.tsx:29`** — ограничить схему ссылок `^https?:` (скопировать `isSafeUrl` из `BrowserPanel.tsx:6-13`). Добавить unit-тест на `javascript:`/`data:`. *(C2, ~10 строк)*
2. **Валидировать `workspace.paths`** в `workspace.rs:94` (`create`) и в `set_paths`/`open_project`: требовать абсолютные существующие пути, ограничить `$HOME` или явным allowlist. *(C3, корень sandbox-escape)*
3. **Включить CSP** в `tauri.conf.json` (не `null`) — defense-in-depth под C2 и весь webview-trust. *(C1)*
4. **Подключить `budget.rs`**: добавить `pub mod budget;` в `orchestrator/mod.rs`, вызвать `compact` в `build_messages` перед отправкой провайдеру. *(H1)*
5. **Allowlist/подтверждение для `bin.<type>`/`args.<type>` и SSH-флагов** (`agentd/resolve.rs`, `ssh.rs`). Минимум — запретить `-o ProxyCommand`/`RemoteCommand` без явного opt-in. *(C4)*
6. **Tile-токен:** не писать плейнтекстом в settings; `.mcp.json` создавать с `chmod 0600`; рассмотреть сужение скоупа. *(H5)*
7. **`agent_id` валидировать как UUID** перед `format!("{}.log", …)` в `agent.rs:251` (и `reuse_id` в `agentd/engine.rs:194`). *(H6, ~5 строк)*
8. **MCP дефолтный скоуп → `read`** (`auth.rs:49`); `bind_all` — за явный флаг + предупреждение. *(H7, H8)*

### Сделать дальше (HIGH/MEDIUM)

9. Глобальный потолок расходов LLM + ретраи для OmniRouter (зеркало Anthropic-паттерна). *(H2)*
10. Перестать логировать полное тело запроса на error-path (`client.rs:85,145,67`) — редактировать/обрезать. *(H4)*
11. Санитизация untrusted-данных перед попаданием в system-prompt (экранирование/фенсинг для task-title, memory-body, mail-body). *(H3)*
12. SHA/подпись для Whisper-модели (`voice/download.rs`). *(M1)*
13. Обернуть multi-statement записи в транзакции (`agent.rs`, `tasks.rs`). *(M4)*
14. Лимит размера `chat_queue` + backpressure. *(M5)*
15. Заменить ручные `is_mutating`/`is_dangerous` на единый источник правды (атрибут у tool-definition). *(дрейф)*

### Гигиена / качество

16. Включить `"strict": true` в `tsconfig.app.json`. *(M9)*
17. Добавить React error boundaries на уровне панелей и `Markdown`. *(раздел 3.2)*
18. Удалить орфан-файлы `prompt.v1.rs` (и определиться с `budget.rs` — подключить или удалить). *(M11)*
19. Тесты: миграции БД, `architect/supervisor`, orchestrator e2e, frontend markdown-санитизация + OSC-parser.
20. Декомпозиция `commands.rs` и `PigMemoryWorkbench.tsx`.

---

## 8. Что НЕ проверено / ограничения аудита

- **Промт «KingPrompt» недоступен** — методология реконструирована; если у него есть специфические чек-листы (напр. конкретные CWE/threat-model рамки), их применение нужно повторить после получения текста.
- **Сборка/тесты не запускались** (`cargo build`/`cargo test`/`pnpm build`) — задача только на создание MD-файла. Утверждения о компиляции (орфан-модули) сделаны статически по объявлениям `mod`; рекомендуется подтвердить `cargo build` + `cargo test`.
- **Незакоммиченные изменения `agentd/*`** проверены как rustfmt-реформат, но полный `git diff` построчно не приложен.
- Часть MEDIUM/LOW-находок помечена *(via subagent)* — получены параллельными explore-агентами и выборочно, но не на 100%, перепроверены лично.
- Runtime-поведение (реальный prompt-injection PoC, реальный XSS-trigger в живом UI) не воспроизводилось — находки статические, но с подтверждённым путём кода.

---

*Конец отчёта. Исходный код не изменялся; создан только `/home/camer/pigide/AUDIT.md`.*
