# Аудит backend оркестратора PigIDE — баги ориентации, phantom tool-calls, таскборд

> Read-only аудит. Backend: Rust, `src-tauri/src/`. Дата: 2026-05-31.
> Task: `8cbc8219-d31b-4e59-b5eb-7f85f3511a0f`. Смежный таск `0fcaf40d-…` пишет в `docs/orchestrator-bugs.md` — этот файл (`docs/audit/orchestrator-bugs.md`) самостоятельный, без слепого дублирования.
>
> Скан-карта (через pigmemory): [[pig-ide-backend-map]], [[pig-ide-data-flows]], [[pig-ide-subsystems]].
> Прочитанные модули: `orchestrator/{mod,phantom,budget,tools}.rs`, `orchestrator/providers/anthropic.rs`, `agent.rs`, `agentd/{engine,client}.rs`, `tasks.rs`, `chat.rs`, `chat_queue.rs`, `chat_queue_worker.rs`, `chat_sessions.rs`, `commands.rs`, `swarm/{tools,mailbox,rollcall,ownership}.rs`, `architect/{supervisor,classifier}.rs`, `skills/composer.rs`, `db.rs`.
>
> **Раунд 2 (2026-05-31, дополнение):** класс 4 (chat-queue / pipeline) и класс 5 (broker / провайдер / lifecycle) — 10 новых багов ниже основного блока.
>
> **Раунд 3 (2026-05-31, дополнение):** класс 6 (taskboard wiring) и класс 7 (swarm-auth / стриминг / spawn-cwd) — ещё 10 багов. Дочитаны `mcp/server.rs`, `memory/ingest/task_complete.rs`, `agentd/resolve.rs`, `agentd/engine.rs` (spawn), `swarm/review.rs`, `anthropic.rs` (stream retry), `commands.rs:update_task`.

## Сводка

| Класс | critical | high | med | low | всего |
|---|---|---|---|---|---|
| 1. Phantom tool-calls | 1 | 3 | 2 | 0 | 6 |
| 2. Путаница в агентах / потеря ориентации | 3 | 3 | 2 | 0 | 8 |
| 3. Таскборд / done-сигнал | 2 | 3 | 1 | 0 | 6 |
| 4. Chat-queue / pipeline / контекст | 1 | 3 | 1 | 0 | 5 |
| 5. Broker / провайдер / lifecycle | 1 | 2 | 2 | 0 | 5 |
| 6. Taskboard wiring (хуки/события) | 1 | 2 | 1 | 0 | 4 |
| 7. Swarm-auth / стриминг / spawn-cwd | 2 | 3 | 1 | 0 | 6 |
| **Итого** | **11** | **19** | **10** | **0** | **40** |

**Корневая первопричина №1 (связывает классы 2 и 3):** оркестратор **не имеет собственной agent-идентичности** и **`spawn_agent` всегда создаёт агента с `role='builder'`** (default из миграции v7, `db.rs:262`; ни один `INSERT INTO agents` в `agent.rs` не пишет `role`). Значит в системе **никогда не существует агента с ролью `coordinator`/`reviewer`/`scout`**. Из-за этого:
- билдеру некуда отправить `send_mail(role:coordinator, …)` — `validate_to_addr` пройдёт (роль валидна как строка), но **читателя `role:coordinator` не существует**, и оркестратор физически не вызывает `read_mailbox` в своём tool-loop → done-сигнал теряется (Баги 3.1, 3.2);
- `open_review_gate(reviewer_id=…)` ссылается на несуществующего ревьюера; gate висит Pending вечно (Баг 3.5).

**Корневая первопричина №2 (класс 2):** в backend **нет понятия "focused/active tile"** — `grep focused/active_agent` по `*.rs` находит только оконный фокус в `voice/` и текст промптов. Поэтому `send_to_agent("active")` берёт `list.last()` из **`HashMap`-итерации брокера** (`engine.rs:187`, без сортировки) → недетерминированный получатель (Баг 2.1).

---

# Класс 1. PHANTOM TOOL-CALLS

### Bug 1.1 — `is_phantom` слепнет, как только эмитнут ЛЮБОЙ tool_call (частичный фантом не ловится)
- **Severity:** critical
- **Файл:** `orchestrator/phantom.rs:97-103`, точка вызова `orchestrator/mod.rs:473`
- **Что ломается:** `is_phantom(content, has_tool_calls)` в первой же строке `if has_tool_calls { return false; }`. Детектор бинарный на уровне всего ассистент-сообщения. Реальный сценарий потери ориентации: модель в одном turn эмитит `spawn_agent` (реальный tool_call) **и тут же текстом пишет** "…и отправил ему промпт с заданием" — без второго tool_call `send_to_agent`. `has_tools=true` → фантом не зафиксирован, промпт **не ушёл**, но модель уверена, что ушёл. Это ровно симптом "спавнит агента, но не шлёт промпт".
- **Воспроизвести:**
  1. Дать оркестратору задачу "подними aider и выдай ему задание".
  2. Модель возвращает `tool_calls=[spawn_agent]`, `content="Поднял aider и отправил промпт билдеру."`
  3. `has_tools=true` → `is_phantom` = false → loop выполняет только `spawn_agent`, фантомный `send_to_agent` теряется без следа в `phantom_log.jsonl`.
- **Фикс (step-by-step):**
  1. Изменить сигнатуру на учёт того, *какие именно* tool_calls эмитнуты: `is_phantom(content: &str, emitted: &[&str]) -> bool`.
  2. Завести маппинг trigger-фраза → ожидаемый инструмент (например `"отправил промт"/"sending to agent"` ⇒ `send_to_agent`; `"закинул"/"i'll send"` ⇒ `send_to_agent`; `"вызвал search"` ⇒ `search_memories`).
  3. Считать фантомом, если фраза найдена И ожидаемого инструмента нет в `emitted`, **даже если** другие tool_calls присутствуют.
  4. В `mod.rs:473` передавать имена реально эмитнутых вызовов из `assembled.tool_calls`.
  5. Тест: `is_phantom("Поднял aider и отправил промпт", &["spawn_agent"])` ⇒ `true`.

### Bug 1.2 — после исчерпания retry оркестратор завершает turn (тихий truncate действия)
- **Severity:** high
- **Файл:** `orchestrator/mod.rs:515-530`
- **Что ломается:** при `phantom_retries == PHANTOM_MAX_RETRIES` (2) пишется system-warning и `return Ok(())`. Задача пользователя **молча не выполнена** — действие потеряно, никакого fallback (например, прямого выполнения предполагаемого вызова или эскалации). Модель «сказала, что сделала», ретраи не помогли — и turn закрыт как будто всё нормально (`Ok`). Для координатора это означает «билдер запущен, задание выдано», хотя ничего не произошло.
- **Воспроизвести:** замокать провайдер, чтобы он 3 раза подряд возвращал narrative без tool_calls на одну и ту же фразу → в чате один warning, действие не выполнено, ошибки нет.
- **Фикс:**
  1. Сделать warning жёстче: пометить turn как **failed** (вернуть `Err(Error::Orchestrator("phantom tool_call unresolved"))`), чтобы `run_chat` (mod.rs:352) откатил частичную историю и показал явную ошибку, а не «успех».
  2. Опционально: в warning добавить, *какой* инструмент ожидался (из маппинга Bug 1.1), чтобы пользователь понимал, что именно не сработало.
  3. Логировать финальный неуспех с `retried=true, resolved=false` (уже делается на :516 — оставить), но убедиться, что turn не выглядит успешным.

### Bug 1.3 — "resolved" засчитывается, если на nag модель ответила пустым/нейтральным текстом
- **Severity:** high
- **Файл:** `orchestrator/mod.rs:477-491`
- **Что ломается:** на retry-turn `resolved = !is_phantom(final_text, has_tools)`. Если модель в ответ на RETRY_NAG напишет нейтральный текст без триггер-фраз и без tool_calls (например "Хорошо."), `is_phantom` вернёт false → `resolved=true` → `phantom_nag=false` и `continue`. Поскольку `has_tools=false`, следующая итерация попадёт в `if !has_tools { return Ok(()) }` (:532). Фантом записан в лог как **успешно разрешённый**, хотя инструмент так и не вызван. Метрика resolved в `phantom_log.jsonl` врёт.
- **Воспроизвести:** retry-turn возвращает `content="Понял, продолжаю", tool_calls=[]` → лог пишет `resolved=true`, действие не выполнено.
- **Фикс:**
  1. Считать `resolved=true` **только** если на retry эмитнут реальный tool_call (`has_tools==true`), а не «отсутствие триггер-фразы».
  2. Если retry дал снова пустой текст без вызова — это не resolution, а повторный фантом: продолжать retry до бюджета, затем — Bug 1.2 фикс.

### Bug 1.4 — список TRIGGER_PHRASES не покрывает прошедшее/«сделал» без местоимения и частые формы
- **Severity:** med
- **Файл:** `orchestrator/phantom.rs:18-60`
- **Что ломается:** список фраз ручной и узкий. Не ловятся типичные формулировки: "Задание выдано", "Промпт ушёл билдеру", "Агент получил инструкции", "Готово, передал в работу", англ. "the prompt has been sent", "dispatched the task", "handed off to the builder", "kicked off the agent". Любая из них = фантом, который проходит как обычный финальный ответ (`!has_tools` → `return Ok`).
- **Воспроизвести:** `is_phantom("Задание выдано билдеру", false)` ⇒ сейчас `false` (нет в списке).
- **Фикс:**
  1. Расширить `TRIGGER_PHRASES` пассивными/безличными формами (рус+англ): "задание выдано", "промпт ушёл", "передал в работу", "агент получил", "dispatched", "handed off", "kicked off", "has been sent".
  2. Долгосрочно — заменить keyword-список на лёгкий regex-set (как в `architect/classifier.rs`) с группами «глагол отправки/вызова + объект (агент/промпт/задание)», чтобы покрыть склонения без ручного перечисления.
  3. Синхронизировать с «forbidden phrases» в `prompt.rs` (комментарий на phantom.rs:16 это уже требует).

### Bug 1.5 — фантом-детект не учитывает план («сначала сделаю X, потом Y»), где X — реальный вызов, Y — фантом
- **Severity:** med
- **Файл:** `orchestrator/mod.rs:467-535`
- **Что ломается:** детектор смотрит на один turn целиком. Многошаговые планы, где модель в тексте обещает будущие вызовы ("сейчас заклеймлю файлы, потом отправлю промпт"), но эмитит только первый — частный случай Bug 1.1, но проявляется и при честном multi-tool turn: будущие обещанные вызовы не отслеживаются между итерациями.
- **Воспроизвести:** turn1 эмитит `claim_files`, текст "потом отправлю билдеру"; turn2 модель считает, что уже отправила, и пишет финальный ответ.
- **Фикс:**
  1. После фикса Bug 1.1 ввести лёгкий «pending-intent» трекер: если в turn найдена фраза-обещание инструмента, которого нет в текущем turn, пометить этот инструмент как ожидаемый и проверить его появление в следующей итерации; если не появился до конца loop — warning/Err.

### Bug 1.6 — `strip_calling_tools_preamble` режет по первому маркеру, теряя осмысленный текст после него
- **Severity:** med
- **Файл:** `orchestrator/phantom.rs:71-91`, вызов `orchestrator/mod.rs:286`
- **Что ломается:** функция обрезает всё начиная с первого `Calling tools:`/`Вызываю тулзы:`. Если модель написала "Calling tools: …\n\nПосле этого проверю результат и отпишусь" — хвост (намерение на след. шаг) теряется при реконструкции истории, модель в следующей итерации не видит собственный план → теряет ориентацию по многошаговой задаче.
- **Воспроизвести:** assistant content = "text A\nCalling tools:\n...\nReal plan B" → в истории остаётся только "text A".
- **Фикс:**
  1. Вырезать только сам блок перечисления вызовов (строки, начинающиеся с буллета/имени-функции непосредственно после маркера), а не «всё до конца строки».
  2. Либо: распознать конец преамбулы (первая пустая строка после блока) и сохранить последующий осмысленный текст.

---

# Класс 2. ПУТАНИЦА В АГЕНТАХ / ПОТЕРЯ ОРИЕНТАЦИИ

### Bug 2.1 — `send_to_agent("active")` = `list.last()` поверх HashMap-итерации → недетерминированный получатель
- **Severity:** critical
- **Файл:** `orchestrator/tools.rs:455-465`; источник списка — `agent.rs:515` → `client.list` → `agentd/engine.rs:195-202`
- **Что ломается:** при `agent_id=="active"` берётся `agent_mgr.list(ws).last()`. Но `Engine::list_workspace` (`engine.rs:195`) собирает агентов из `self.runtimes.lock().values()` — это `HashMap<String, Runtime>` (`engine.rs:130`), **порядок итерации не определён** и меняется между вызовами. То есть «active» — это **случайный** агент воркспейса, а не «focused tile» (которого в backend вообще нет, см. первопричину №2). Симптом «спавнит агента, шлёт промпт другому» — прямое следствие.
- **Воспроизвести:**
  1. Спавнить 2+ агентов в одном воркспейсе.
  2. `send_to_agent(agent_id="active", text=…)` несколько раз — `list.last()` будет указывать на разных агентов между вызовами (HashMap не гарантирует порядок).
- **Фикс:**
  1. Ввести явное понятие focused agent в backend: настройка `current_agent_id` (per-workspace) в `settings`, апдейтится фронтом через Tauri-команду при смене фокуса тайла и эмитится событием.
  2. `send_to_agent("active")` резолвит её, а не `list.last()`. Если не выставлена — **ошибка** "no focused agent; pass explicit agent_id", а не молчаливый случайный выбор.
  3. Детерминизировать `list`: в `Engine::list_workspace`/`list_all` сортировать результат по `created_at` (затем по `id`) перед возвратом, чтобы «последний спавненный» был стабильным понятием для любых fallback.
  4. Обновить описание тула (`tools.rs:86`): "active" = focused tile из настройки, иначе ошибка.

### Bug 2.2 — `send_to_agent` не проверяет, что агент жив и принадлежит текущему воркспейсу
- **Severity:** critical
- **Файл:** `orchestrator/tools.rs:442-471`
- **Что ломается:** при явном `agent_id` нет проверки статуса (`running`/`exited`) и воркспейса. `agent_mgr.write` дойдёт до брокера; если агент `exited`, broker вернёт `Gone`/`NotFound` (`engine.rs:402-407`) — но оркестратор получит generic-ошибку без подсказки «агент закрыт, переспавни». Хуже: можно отправить промпт агенту **другого** воркспейса (id валиден в брокере), что напрямую «уводит промпт не туда».
- **Воспроизвести:** закрыть агента (`close_agent`), затем `send_to_agent(agent_id=<его id>, …)` → write упадёт generic-ошибкой; или взять id агента из чужого ws → промпт уйдёт ему.
- **Фикс:**
  1. Перед `write` сделать `agent_mgr.list(current_ws)` и проверить, что `resolved_id` присутствует и `status=="running"`.
  2. Если агент не в текущем ws — `Err(Invalid("agent <id> belongs to another workspace"))`.
  3. Если `exited`/отсутствует — вернуть структурированную ошибку `{"error":"agent_gone","agent_id":…,"hint":"respawn and re-send with full context"}`, чтобы модель знала, что делать (промпт `prompt.rs:267` уже описывает этот recovery — но backend сейчас не даёт сигнала).

### Bug 2.3 — гонка при `spawn_agent(count>1)`: layout-grid и недетерминированный «последний»
- **Severity:** high
- **Файл:** `orchestrator/tools.rs:393-411`
- **Что ломается:** цикл спавнит `count` агентов последовательно, каждый `insert_grid(&a.id, 0)`. Все ids возвращаются в `spawned`, но (а) оркестратор не получает явного «вот id для задачи N» маппинга — он сам должен сопоставить; (б) сразу после batch-спавна `send_to_agent("active")` укажет на HashMap-`last()` (Bug 2.1), который не обязан быть последним из `spawned`. При параллельной обработке tool_calls в одном turn (mod.rs:541 цикл по вызовам — последовательный, но event-pump и broker асинхронны) свежий агент может ещё не появиться в `client.list` к моменту резолва «active».
- **Воспроизвести:** `spawn_agent(count=3)` затем в том же плане `send_to_agent("active", brief1)` — нет гарантии, что brief1 уйдёт одному из только что созданных троих, и тем более «правильному».
- **Фикс:**
  1. Возвращать из `spawn_agent` явный упорядоченный список `agents[].id` (уже есть) и **запретить** `"active"` сразу после batch-спавна на уровне промпта; требовать явный id из ответа `spawn_agent`.
  2. Для multi-agent dispatch — связывать каждый спавн с task через `assign_task_to_agent` сразу, и слать промпт по `task.agent_id`, а не по «active».
  3. Гарантировать, что `spawn` синхронно отражён в `client.list` до возврата (он UPSERT-ит mirror в `agent.rs:451`, но «истина» — broker; добавить читать-после-записи через broker `list` в spawn-результат, либо использовать возвращённый `info.id` напрямую — что и делается, главное не уходить в "active").

### Bug 2.4 — [WORLD STATE] строится на каждый turn, но может отставать от брокера / показывать чужие воркспейсы
- **Severity:** high
- **Файл:** `orchestrator/mod.rs:120-185`
- **Что ломается:**
  - Список агентов берётся `self.agent_mgr.list(&w.id)` по **всем** воркспейсам (mod.rs:149-150). Это бьёт брокер N раз; при недоступном брокере `list` падает в SQLite-mirror (`agent.rs:529`), который мог устареть (статусы сбрасываются в `exited` только на restore). Итог: модель может видеть `running`-агентов, которых уже нет, или наоборот.
  - `agent_count` в строке воркспейса (`mod.rs:144`) — из `ws_mgr.list()` (отдельный источник, вероятно SQLite), может расходиться с реальным числом из брокера, показанным ниже. Модель видит противоречивые числа.
  - WORLD STATE формируется **до** стрима; если за время длинного turn (до 6 итераций, tool-loop) агент закрылся/спавнился, модель оперирует устаревшим снимком до конца turn.
- **Воспроизвести:** убить агента в середине многошагового turn → модель в следующей итерации всё ещё видит его в WORLD STATE (снимок не перестраивается между итерациями? — перестраивается в `build_messages` на каждой итерации, но `agent_mgr.list` может вернуть stale mirror при flaky-брокере).
- **Фикс:**
  1. Единый источник истины: и `agent_count`, и список агентов брать из одного вызова `agent_mgr.list` (broker), убрать рассинхрон с `ws_mgr.list().agent_count`.
  2. При недоступном брокере явно помечать в WORLD STATE: "agents: (broker unavailable — list may be stale)", чтобы модель не доверяла снимку слепо.
  3. Показывать `status` агента в WORLD STATE (сейчас фильтр `a.status != "running"` скрывает не-running, но не показывает «недавно exited» — модель не понимает, что агент умер).

### Bug 2.5 — оркестратор не знает своего agent_id; `send_mail` от его имени невозможен корректно
- **Severity:** high
- **Файл:** первопричина №1; `swarm/mailbox.rs:31` (`validate_agent` требует существующую строку в `agents`), вызов из `swarm/tools.rs:173`
- **Что ломается:** `send_mail` требует `from_agent_id`, который должен существовать в таблице `agents` (`validate_agent`→`agent_role`). Оркестратор **не зарегистрирован** как агент (нет `INSERT` для него). Значит любой `send_mail(from_agent_id=<orchestrator>)` упадёт `NotFound`. Модель вынуждена подставлять `from_agent_id` одного из билдеров (как видно в истории memory-чата — "use one of the registered claude tile ids as the sender") — это подделка отправителя и путаница в трекинге переписки.
- **Воспроизвести:** оркестратор зовёт `send_mail(from_agent_id="orchestrator", to="role:builder", …)` → `Error::NotFound("agent orchestrator")`.
- **Фикс:**
  1. На старте (`lib.rs::run` / при создании воркспейса) регистрировать синтетического агента-координатора: строка в `agents` с фиксированным id (например `coordinator:<ws_id>`), `role='coordinator'`, `status='running'`, без PTY (broker про него не знает — это чисто swarm-идентичность).
  2. Прокидывать этот id в оркестратор и подставлять как `from_agent_id` по умолчанию в swarm-туллах, если модель не указала.
  3. Тогда `role:coordinator` mailbox имеет реального читателя (см. класс 3).

### Bug 2.6 — `close_agent` чистит layout и SQLite, но `current_agent_id`/задачи остаются висеть на мёртвом id
- **Severity:** med
- **Файл:** `orchestrator/tools.rs:413-441`; `tasks.rs` (agent_id ON DELETE SET NULL только при удалении строки agents, а `kill` лишь ставит `status='exited'`)
- **Что ломается:** `close_agent` зовёт `agent_mgr.kill` (ставит `status='exited'`, строка остаётся) и убирает leaf из layout. Но задачи с `task.agent_id = <закрытый>` остаются привязанными к мёртвому агенту (FK `ON DELETE SET NULL` не срабатывает — строка не удаляется). Таскборд продолжает показывать таск «у агента X», которого уже нет. Если был «focused», ничто не сбрасывает фокус (Bug 2.1).
- **Воспроизвести:** assign таск агенту, `close_agent`, `list_tasks` → таск всё ещё `agent_id=<exited>`, `status` не изменён.
- **Фикс:**
  1. В `close_agent` после kill: найти задачи с этим `agent_id` в статусе `in_progress` и либо вернуть в `todo` с `agent_id=NULL`, либо пометить и сообщить координатору.
  2. Сбросить `current_agent_id`, если он указывал на закрытого (после фикса 2.1).

### Bug 2.7 — `assign_task_to_agent` не сверяет воркспейс таска и агента
- **Severity:** med
- **Файл:** `tasks.rs:293-312`, вызов `orchestrator/tools.rs:627-637`
- **Что ломается:** `assign` проверяет только существование агента (`COUNT(*) FROM agents`), не то, что агент в том же воркспейсе, что и таск. Можно привязать таск к агенту чужого ws → координатор будет слать бриф «через task.agent_id» не туда (после фикса 2.3.2).
- **Воспроизвести:** `assign_task_to_agent(task_in_ws1, agent_in_ws2)` → успех, скрытый рассинхрон.
- **Фикс:** в `TaskManager::assign` дополнительно проверять `agents.workspace_id == tasks.workspace_id`, иначе `Err(Invalid)`.

### Bug 2.8 — `wait_for_agent_idle`/`tail_agent` не валидируют agent_id и читают лог напрямую мимо брокера
- **Severity:** med
- **Файл:** `orchestrator/tools.rs:473-534`
- **Что ломается:**
  - `tail_agent` сам строит путь `…/agents/{agent_id}.log` (tools.rs:516-522) **без** `is_safe_agent_id` (в отличие от `agent.rs:251`, где проверка есть). Хотя id обычно UUID, в MCP-режиме args приходят от внешнего клиента → возможен path-traversal через `agent_id="../../…"`.
  - `wait_for_agent_idle` опирается на `last_stdout_age`, кэш которого живёт в `ConnectedState.last_stdout` и обновляется только при наличии event-pump; для агента, по которому ещё не было stdout, `last_stdout_age` = `None` → цикл крутится до timeout, даже если агент давно «idle». Это ломает done-детекцию по таску (класс 3).
- **Воспроизвести:** `tail_agent(agent_id="../../etc/passwd")` в MCP-режиме; или `wait_for_agent_idle` сразу после спавна до первого stdout → всегда timeout.
- **Фикс:**
  1. В `tail_agent` вызвать `crate::agent::is_safe_agent_id(agent_id)` перед построением пути (или проксировать через `agent_mgr.read_log_tail`, где проверка уже есть).
  2. `wait_for_agent_idle`: трактовать `None` (нет записей stdout) как «ещё не начинал» и опираться на наличие агента в `list` + время с момента write, а не только на `last_stdout_age`.

---

# Класс 3. ТАСКБОРД / DONE-СИГНАЛ

### Bug 3.1 — нет связи «агент закончил» → статус таска; done-сигнал по mailbox оркестратор не читает
- **Severity:** critical
- **Файл:** архитектурно: `orchestrator/mod.rs` tool-loop (нет авто-`read_mailbox`), `swarm/mailbox.rs`, первопричина №1
- **Что ломается:** дизайн предполагает: билдер по завершении шлёт `send_mail(role:coordinator, "ready…")` (`prompt.rs:434`). Но (а) `role:coordinator` mailbox **некому читать** — нет агента-координатора (первопричина №1); (б) оркестратор в `run_chat`/`tool_loop` **никогда сам не вызывает `read_mailbox`** между turn — он реагирует только на пользовательский ввод. Значит входящая почта от билдера не превращается ни в обновление `task.status`, ни в pump следующего шага. Таск навсегда `in_progress`, агент в idle. Это ровно «не понимает, кто закончил».
- **Воспроизвести:** билдер пишет `send_mail(role:coordinator,"done")` → mail лежит в таблице непрочитанным, ни один статус не меняется, оркестратор молчит, пока пользователь не спросит.
- **Фикс:**
  1. Зарегистрировать агента-координатора (Bug 2.5 фикс) — у `role:coordinator` появляется читатель.
  2. Ввести фоновый pump (по аналогии с `architect/supervisor.rs` или `watcher::run_reply_pump`): периодически `read_mailbox(coordinator)`; на новое сообщение от билдера — эмитить событие в чат оркестратора и/или класть в `chat_queue`, чтобы оркестратор отреагировал (обновил статус таска, выдал следующий шаг).
  3. Конвенция: тело done-сообщения связывать с `task_id` (требовать в брифе `thread_id=task:<id>`), чтобы pump знал, какой таск двигать в `in_review`/`complete`.

### Bug 3.2 — статусы таска двигаются только вручную моделью; нет авто-перехода по реальному состоянию агента
- **Severity:** critical
- **Файл:** `tasks.rs:202-280` (update только по явному вызову), `orchestrator/tools.rs:598-626`
- **Что ломается:** `task.status` меняется исключительно через `update_task`, который зовёт сама модель. Нет ни одного места, где завершение работы агента (idle_done из `architect/classifier.rs` или mail-сигнал) **автоматически** двигало бы таск. Architect умеет `assign_next_todo` (двигает `todo`→`in_progress`, supervisor.rs:446), но **обратно** (`in_progress`→`in_review`/`complete`) не двигает никогда. Результат: таскборд показывает `in_progress` на задачах, где агент давно в idle. «Кто закончил» — неизвестно.
- **Воспроизвести:** включить architect, дать таск, дождаться `handoff_ready` от агента → `classify`=IdleDone, но `task.status` не меняется (нет кода перехода).
- **Фикс:**
  1. В `architect/supervisor.rs::tick` при `AgentSignal::IdleDone` для агента, у которого есть назначенный таск (`task_mgr.list(ws, "in_progress", Some(agent_id))`), переводить таск в `in_review` (а не только эскалировать/назначать следующий).
  2. Источник «done» — пара сигналов: classifier IdleDone **и/или** mail-сигнал (Bug 3.1). Требовать хотя бы один надёжный (mail с `handoff_ready`/thread=task:<id>), classifier — как fallback.
  3. Логировать переход в DecisionLog, чтобы было видно «кто закончил».

### Bug 3.3 — IdleDone ложно срабатывает на «тихо без маркеров» → таск может уйти в done, хотя агент завис/ждёт
- **Severity:** high
- **Файл:** `architect/classifier.rs:158-174`
- **Что ломается:** ветка `if is_quiet { return IdleDone }` (classifier.rs:171-174): если агент молчит дольше `idle_done_after` (default 8с) **без** done-маркеров, классифицируется как `IdleDone`. Но «тихо 8с» бывает и когда агент думает/качает зависимость/завис до `stuck_after` (45-60с). При `auto_assign_next` это спровоцирует выдачу следующего таска поверх ещё работающего агента (Bug 3.4), а после фикса 3.2 — ложный перевод таска в `in_review`.
- **Воспроизвести:** агент печатает "downloading…", замолкает на 10с (сеть) → IdleDone, хотя не закончил.
- **Фикс:**
  1. Не считать «тихо без маркеров» завершением. Возвращать `Unknown` (как для коротких пауз) до достижения `stuck_after`; `IdleDone` — только при наличии DONE-маркера или явного mail-сигнала.
  2. Поднять `idle_done_after` default и/или сделать его зависимым от типа агента.
  3. Согласовать с Bug 3.2: авто-переход таска — только по `IdleDone`+маркер, не по тишине.

### Bug 3.4 — `auto_assign_next` грузит новый таск тому же агенту по ложному IdleDone, маскируя незавершённую работу
- **Severity:** high
- **Файл:** `architect/supervisor.rs:275-282, 419-466`
- **Что ломается:** при `IdleDone`+есть `todo`, architect `assign_next_todo` пишет в stdin агента новый бриф и двигает новый таск в `in_progress`. Если IdleDone ложный (Bug 3.3), агент получает второй бриф поверх первого незаконченного → путаница в самом билдере, и **два** таска числятся на нём (`in_progress`), первый — навсегда (Bug 3.2). Таскборд окончательно расходится с реальностью.
- **Воспроизвести:** агент в ложном IdleDone + непустая очередь → ему прилетает новый бриф, первый таск завис.
- **Фикс:**
  1. Зависит от Bug 3.3 (убрать ложный IdleDone).
  2. Перед `assign_next` проверять, что у агента нет таска в `in_progress` (`task_mgr.list(ws,"in_progress",Some(agent_id))` пуст), иначе не назначать.

### Bug 3.5 — review-gate блокирует complete навсегда: ревьюера-агента не существует, голосовать некому
- **Severity:** med
- **Файл:** `tasks.rs:234-236` (`task_completable`), `swarm/review.rs` (open/vote), первопричина №1
- **Что ломается:** перевод таска в `complete` зовёт `swarm::review::task_completable`, который блокирует, пока все gate не `Pass`. Gate открывается `open_review_gate(reviewer_id)`. Но ревьюер — это агент с `role='reviewer'`, которого **никто не создаёт** (spawn всегда `builder`). Если gate открыт, проголосовать `pass` некому (нет reviewer-агента, а `vote_review_gate` сам по себе роль не проверяет — но процедурно ревью делать некому) → таск нельзя закрыть. Если же gate не открывают вовсе, ревью-механизм мёртв. В обоих случаях «complete» либо недостижим, либо бессмыслен.
- **Воспроизвести:** `open_review_gate(task)`, затем `update_task(task, status="complete")` без голоса → `task_completable` вернёт ошибку, таск завис в `in_review`.
- **Фикс:**
  1. Дать возможность спавнить агента с явной ролью: добавить параметр `role` в `spawn_agent`/`spawn_internal` и писать его в `INSERT INTO agents` (сейчас роль не пишется вообще — всегда default `builder`).
  2. Либо: разрешить координатору (синтетический агент из Bug 2.5) голосовать на gate, если ревьюера в воркспейсе нет.
  3. Документировать в промпте, что открывать gate имеет смысл только при наличии reviewer.

### Bug 3.6 — `spawn_agent` всегда создаёт `role='builder'`; роли не выставляются нигде
- **Severity:** high
- **Файл:** `agent.rs:451-466` и `agent.rs:371-379` (`INSERT INTO agents(id,workspace_id,type,cwd,status,created_at)` — колонки `role` нет), default `db.rs:262`
- **Что ломается:** это первопричина №1 в явном виде. Ни `spawn_internal`, ни `restore_session` не пишут `role`. Все агенты — `builder`. Следствия: нет coordinator (Bug 2.5, 3.1), нет reviewer (Bug 3.5), `broadcast`/rollcall по ролям `reviewer`/`scout`/`coordinator` уходят в пустоту (никто не читает), хотя `validate_role` их принимает. Swarm-координация структурно неработоспособна.
- **Воспроизвести:** `spawn_agent(claude)` × N → `SELECT DISTINCT role FROM agents` = только `builder`.
- **Фикс:**
  1. Добавить `role` в сигнатуру spawn и в оба `INSERT INTO agents` (+`ON CONFLICT … DO UPDATE` при необходимости).
  2. Прокинуть `role` в tool-схему `spawn_agent` (`orchestrator/tools.rs:62-74`) с enum `["builder","reviewer","scout"]` (coordinator — синтетический, не спавнится как PTY).
  3. После этого классы 2/3 (mail done-сигнал, review-gate) становятся реализуемыми.

---

# Класс 4. CHAT-QUEUE / PIPELINE / КОНТЕКСТ

### Bug 4.1 — `@`-mention attachments валидируются, сохраняются в очередь и **молча выбрасываются** перед моделью
- **Severity:** critical
- **Файл:** `chat_queue_worker.rs:114` (`run_chat(item.text.clone())`), `orchestrator/mod.rs:339` (`run_chat(self, text: String)`)
- **Что ломается:** весь пайплайн вокруг `Attachment` существует: `commands.rs:299` валидирует и кладёт их в `enqueue_with_attachments`, они сериализуются в `chat_queue.attachments_json`, переживают рестарт, читаются обратно в `claim_next` (`chat_queue.rs:226`). Но воркер передаёт в оркестратор **только `item.text`** — `item.attachments` нигде не используется. `run_chat` даже не имеет параметра под них. Пользователь приложил файлы через `@`-mention, увидел их в UI-бейдже, но модель про них **никогда не узнаёт**. Документация на `QueueItem.attachments` (chat_queue.rs:70-73) прямо обещает "Surfaces in the orchestrator's [WORLD STATE] block" — этого не происходит.
- **Воспроизвести:** `send_chat(text="посмотри на это", attachments=[@src/main.rs])` → в `[WORLD STATE]`/промпте никаких путей; модель отвечает вслепую.
- **Фикс:**
  1. Расширить `run_chat(self, text: String, attachments: Vec<Attachment>)` (или передавать весь `QueueItem`).
  2. В `build_messages`/`build_system_prompt` добавить блок `[ATTACHMENTS]` со списком `path` (+ опционально прочитать содержимое через `files::read` с лимитом, если это входит в дизайн).
  3. Воркер: `run_chat(item.text.clone(), item.attachments.clone())`.
  4. Тест: enqueue с attachment → перехватить собранный prompt → проверить наличие пути.

### Bug 4.2 — воркер дренирует только `ensure_current()`-сессию; смена сессии бросает очередь предыдущей
- **Severity:** high
- **Файл:** `chat_queue_worker.rs:104-110` (`drain_once` берёт `ensure_current`), `chat_queue_worker.rs:90-96` (park до poke)
- **Что ломается:** `drain_once` на каждой итерации зовёт `chat_sessions::ensure_current(&db)` и тянет `claim_next(session)` только для **текущей** сессии. Если пользователь поставил 5 сообщений в сессию A, затем переключил workspace/сессию на B (chat scope = Workspace), воркер начнёт обслуживать B, а очередь A зависнет до следующего `poke` именно в контексте A. Так как `poke` происходит только на `send_chat`, очередь A не дренируется, пока пользователь не вернётся и не отправит туда новое сообщение. `queued`-сообщения «застревают» при переключении.
- **Воспроизвести:** enqueue в сессию A (не дожидаясь дрейна), переключить на B, в A остаются `queued`-строки навсегда (пока не вернёшься и не пнёшь).
- **Фикс:**
  1. `drain_once` должен обходить **все** сессии с `queued`-строками (`SELECT DISTINCT session_id FROM chat_queue WHERE status='queued'`), а не только текущую.
  2. Либо привязать воркер к сессии явно и пинать его при `switch_workspace`/смене scope (`commands.rs:106`).
  3. На старте после `recover_inflight` — продрейнить всё накопленное, не только current.

### Bug 4.3 — `run_chat` всегда пишет в `ensure_current()`-сессию, а не в сессию обрабатываемого item
- **Severity:** high
- **Файл:** `orchestrator/mod.rs:340` (`let session_id = chat_sessions::ensure_current(&self.db)?`), вызывается из воркера с `item.text`
- **Что ломается:** воркер claim-ит item из сессии X, но `run_chat` внутри сам заново резолвит `ensure_current()` — это сессия, активная **сейчас**, а не та, из которой взят item. Если за время обработки длинного turn пользователь переключил сессию, ответ ассистента запишется в **другую** сессию. История перемешивается между сессиями. Item.session_id (который у воркера есть) полностью игнорируется оркестратором.
- **Воспроизвести:** запустить долгий turn в сессии A, переключиться на B во время генерации → assistant-сообщения уедут в B.
- **Фикс:**
  1. Прокинуть `session_id` параметром в `run_chat(self, session_id: &str, text, …)` из воркера (`item.session_id`), убрать внутренний `ensure_current`.
  2. Все `chat::insert`/`emit_message`/`delete_after` внутри turn должны использовать этот фиксированный `session_id`.

### Bug 4.4 — MAX_ITERATIONS=6 рвёт многошаговую оркестрацию без сохранения намерения
- **Severity:** med
- **Файл:** `orchestrator/mod.rs:29` (`MAX_ITERATIONS=6`), `mod.rs:572-579` (потолок)
- **Что ломается:** комментарий в `budget.rs:5` говорит "default 20", но реальный лимит — 6. Координатору на реальную задачу (spawn + create_task + claim_files + assign + send_to_agent + read результата) шести итераций мало; при достижении потолка пишется system-нота "Reached max iterations" и turn закрывается. Незавершённый план **не сохраняется** (нет промежуточного состояния/TODO), на следующем сообщении модель начинает заново, теряя ориентацию в многошаговой задаче. Каждая фантом-ретрай-итерация (Баг 1.x) тоже жрёт из этих 6 (общий `for iter in 0..MAX_ITERATIONS`, фантом делает `continue` в том же цикле — см. mod.rs:513), так что 2 фантома уменьшают полезный бюджет до 4.
- **Воспроизвести:** дать задачу на ≥7 tool-шагов → обрыв на середине с "Reached max iterations".
- **Фикс:**
  1. Поднять `MAX_ITERATIONS` (хотя бы до заявленных 20) и/или сделать настраиваемым через settings.
  2. Фантом-ретраи считать **отдельным** счётчиком, не вычитать из бюджета полезных итераций (сейчас `phantom continue` крутит тот же `iter`).
  3. При достижении потолка — просить модель кратко зафиксировать прогресс/следующий шаг (в chat или память), чтобы следующий turn продолжил, а не начал заново.

### Bug 4.5 — `delete_after` использует строгий `>` по timestamp; равные `created_at` не откатываются
- **Severity:** med
- **Файл:** `chat.rs:145-152` (`created_at>?2`), вызовы `orchestrator/mod.rs:355, 442`
- **Что ломается:** откат частичного turn (`delete_after(session, user_created_at)`) удаляет строки строго позже `user_created_at`. `created_at` — `Utc::now().to_rfc3339()` (наносекунды есть, коллизии маловероятны, но не исключены при быстрых вставках/одинаковом источнике времени). Если placeholder ассистента получил **тот же** timestamp, что и user-msg (теоретически на быстрой системе), он не удалится → в истории останется пустой/битый assistant без отката. Кроме того, откат по времени, а не по `id`-границе, хрупок: любая фоновая вставка (например system-warning из фантома) с близким timestamp может быть задета или пропущена.
- **Воспроизвести:** трудно детерминированно (зависит от разрешения часов), но риск реален на CI/контейнерах с грубым clock.
- **Фикс:**
  1. Откатывать по явной границе: запоминать `id` или авто-инкрементный `rowid` user-сообщения и удалять `WHERE rowid > ?`.
  2. Либо добавить монотонный seq-столбец в `orchestrator_chat` и сортировать/резать по нему вместо `created_at`.

---

# Класс 5. BROKER / ПРОВАЙДЕР / LIFECYCLE

### Bug 5.1 — смерть брокера не восстанавливается: `connected` держит мёртвый клиент, `pump_started` навсегда `true`
- **Severity:** critical
- **Файл:** `agent.rs:574-612` (`ensure_connected` fast-path), `agent.rs:622-627` (`pump_started.swap(true)` без сброса), `agentd/client.rs:379-412` (read_loop при EOF чистит `pending` и выходит)
- **Что ломается:** когда брокер умирает, `client.rs` read-loop ловит EOF, чистит `pending`, завершается; write-loop тоже. Но `AgentManager.connected` **остаётся `Some(ConnectedState{client})`** с мёртвым клиентом. `ensure_connected` (agent.rs:576) видит `connected.is_some()` → fast-path return Ok, новый connect никогда не происходит. Каждый последующий `write`/`list`/`spawn` уходит в дохлый клиент и возвращает `Disconnected`. `pump_started` (AtomicBool, agent.rs:622) уже `true` и `start_event_pump` делает ранний return — даже при ручном реконнекте event-pump не перезапустится. Комментарий agent.rs:177-179 это признаёт ("a hard failure ends the session"), но на практике это значит: **упал брокер → все агенты недоступны до перезапуска всего PigIDE**, хотя broker по дизайну переживает UI и мог бы быть перезапущен.
- **Воспроизвести:** убить `pigide-agentd` процесс при живом PigIDE → любой `send_to_agent`/`list_agents` отдаёт Disconnected, автоподъёма нет.
- **Фикс:**
  1. В `ensure_connected` детектить мёртвый клиент: при ошибке `Disconnected` в `write`/`list`/`spawn` сбрасывать `*self.connected.lock() = None` (и `pump_started=false`), чтобы следующий вызов переподключился через `connect_or_spawn`.
  2. Event-pump перезапускать после реконнекта (снять ранний return на `pump_started`, либо хранить handle и пересоздавать).
  3. На реконнекте вызвать `restore_session` для ресинка SQLite-mirror и переэмита `agent://spawned`.

### Bug 5.2 — `inject_skills` ломает prompt-cache: `[ACTIVE SKILLS]` дописывается ПОСЛЕ `[WORLD STATE]`, попадая в «динамический хвост»
- **Severity:** high
- **Файл:** `orchestrator/mod.rs:236-249` (порядок: `build_system_prompt` → `[WORLD STATE]`, затем `inject_skills` аппендит), `skills/composer.rs:172-191` (`out.push_str(base)` затем `[ACTIVE SKILLS]`), `providers/anthropic.rs:419-422` (`split_system` режет по первому `[WORLD STATE]`)
- **Что ломается:** prompt-cache (anthropic.rs:357) кэширует всё **до** `WORLD_STATE_MARKER` как статическую голову. Но порядок сборки такой: `build_system_prompt` кладёт BASE + `\n\n[WORLD STATE]\n` + динамику; затем `inject_skills` (mod.rs:240) и `build_memory_preamble` (mod.rs:246) **дописывают в конец** (`sys.push_str`). Итог строки: `BASE … [WORLD STATE] … <agents/tasks> … [ACTIVE SKILLS] … [MEMORY HOT] …`. `split_system` режет по первому вхождению `[WORLD STATE]` → в кэшируемую голову попадает **только BASE**, а большой и относительно стабильный блок скиллов оказывается в некэшируемом хвосте вместе с волатильным world-state. Кэш-хиты околонулевые на части, которая могла бы кэшироваться; лишние токены каждый turn.
- **Воспроизвести:** включить скиллы, сравнить `cache_creation`/`cache_read` в ответах Anthropic — голова = только BASE, скиллы пересчитываются каждый turn.
- **Фикс:**
  1. Собрать стабильную голову целиком до маркера: `BASE + [ACTIVE SKILLS]` (скиллы относительно стабильны в рамках сессии), и только потом `[WORLD STATE]` + memory preamble (волатильное).
  2. Т.е. `inject_skills` должен вставлять блок **перед** `WORLD_STATE_MARKER`, а не в конец строки. Либо `build_system_prompt` принимать уже скомпонованную голову.
  3. Память (`[MEMORY HOT]`/FTS) тоже волатильна → оставить в хвосте (после маркера). Это уже так, главное — поднять скиллы выше маркера.

### Bug 5.3 — `spawn_agent`/`open_project`/`switch_workspace` в MCP-режиме (app=None) не эмитят UI-события → рассинхрон фронта
- **Severity:** high
- **Файл:** `orchestrator/tools.rs:402-407, 433-438, 306-311, 326-344, 695-700` (`if let Some(app) = app { emit }`); MCP-путь зовёт `dispatch(app=None)` (`mcp/server.rs`, см. [[pig-ide-data-flows]] B)
- **Что ломается:** все мутации, влияющие на UI (layout после spawn, EV_WORKSPACE_CHANGED, EV_LAYOUT_CHANGED), эмитятся только при `Some(app)`. Внешний MCP-клиент (Cursor/Claude Code или соседний tile через PigMCP) вызывает `dispatch` с `app=None` (data-flows B). Значит спавн/закрытие агента или смена workspace через MCP **меняет БД и broker, но фронт PigIDE не узнаёт** — тайл не появляется/не исчезает, workspace-свитчер показывает старое. UI рассинхронизируется с реальным состоянием — прямой вклад в «потерю ориентации» (модель и пользователь видят разное).
- **Воспроизвести:** через PigMCP вызвать `spawn_agent` → агент жив в broker и SQLite, но в UI PigIDE тайла нет до ручного refresh/restore.
- **Фикс:**
  1. Дать `AgentManager`/оркестратору доступ к `AppHandle` независимо от пути dispatch (он уже есть в `AgentManager.app`), и эмитить события из менеджера (spawn уже эмитит `agent://spawned` в `agent.rs:473` — но layout-события из tools.rs теряются).
  2. Перенести emit layout/workspace-событий в слой менеджера (WorkspaceManager хранит app-handle) либо всегда передавать app в dispatch (даже в MCP — handle доступен в AppState).
  3. Альтернатива: эмитить через сохранённый глобальный handle, а не через параметр `app: Option`.

### Bug 5.4 — `is_phantom`-snippet и phantom-log пишутся в workspace.paths[0], но при `(none)` льются в CWD процесса
- **Severity:** med
- **Файл:** `orchestrator/mod.rs:475` (`current_workspace_path().unwrap_or_default()` → `""`), `phantom.rs:156-161` (`resolve_log_root("")` → `PathBuf::from(".pigmemory")`)
- **Что ломается:** если текущего workspace нет или у него пустой `paths`, `ws_path=""` → `resolve_log_root` возвращает относительный `.pigmemory` (CWD процесса PigIDE, обычно `/` или home). Phantom-лог пишется не туда, ожидаемого «при проекте» нет; на多-workspace машине логи разных проектов сливаются в один CWD-файл. Не критично, но затрудняет диагностику именно того класса багов, ради которого лог заведён.
- **Воспроизвести:** запустить оркестратор без current workspace, спровоцировать фантом → `phantom_log.jsonl` появляется в CWD, а не в проекте.
- **Фикс:**
  1. При отсутствии workspace-пути писать в стабильный per-app dir (`dirs::data_local_dir()/pigide/phantom_log.jsonl`), а не в относительный `.pigmemory`.
  2. Логировать `workspace_id` в каждой записи, чтобы при общем файле различать источник.

### Bug 5.5 — `estimate_tokens` использует `bytes/4`, недооценивая кириллицу/CJK → компакт срабатывает поздно, реальное переполнение контекста
- **Severity:** med
- **Файл:** `orchestrator/budget.rs:30-37` (`bytes/4`), комментарий budget.rs:31-33 сам признаёт «chars().count() более консервативен для кириллицы»
- **Что ломается:** для русского текста (UTF-8, ~2 байта/символ) `bytes/4` даёт ~0.5 токена на символ, тогда как реальная токенизация кириллицы у Claude/GPT часто ≥1 токена на символ (а то и больше из-за байтовых BPE-сплитов). Оценка занижена в 2-4×. PigIDE — русскоязычный продукт (промпты, память, чат на русском). Значит `should_compact` (budget.rs:101) недооценивает usage, компакт не срабатывает вовремя, и реальный запрос к провайдеру может превысить контекст → 4xx, который budget как раз должен предотвращать (его docstring budget.rs:6-7). Симптом «теряет ориентацию» в длинных русских сессиях частично отсюда.
- **Воспроизвести:** длинная сессия на русском с тяжёлыми `[Tool result]` → estimate говорит «под бюджетом», провайдер отвергает по превышению контекста.
- **Фикс:**
  1. Для не-ASCII считать консервативнее: `max(bytes/4, chars/2)` или явно `chars().count()` с коэффициентом под кириллицу.
  2. Либо детектить долю非-ASCII и поднимать множитель.
  3. Снизить `soft_threshold` запас (сейчас 0.80) или сделать его зависимым от языка.

---

# Класс 6. TASKBOARD WIRING (ХУКИ / СОБЫТИЯ)

### Bug 6.1 — `on_task_complete` (memory-стаб задачи) вызывается ТОЛЬКО из Tauri-команды, не из оркестратора/MCP
- **Severity:** critical
- **Файл:** единственный продакшн-вызов — `commands.rs:1213` (внутри `#[tauri::command] update_task`); `orchestrator/tools.rs:598-626` (`update_task` зовёт `task_mgr.update` напрямую); `mcp/server.rs:346` (dispatch тоже напрямую); `tasks.rs:202` (`TaskManager::update` хука НЕ имеет)
- **Что ломается:** хук завершения задачи (`memory::ingest::task_complete::on_task_complete` — пишет `tasks/<id>.md` стаб, эмитит `memory://note.created`, enqueue-ит в smart-lane) висит на **Tauri-команде `update_task`**, которую дёргает только фронт. Когда задачу завершает **оркестратор** (`tools::dispatch("update_task", status="complete")`) или внешний MCP-клиент — вызывается `TaskManager::update` напрямую, и хук **не срабатывает**. Итог: задачи, закрытые агентом/координатором (основной сценарий swarm!), не попадают в память, не эмитят `note.created`, smart-lane их не видит. Таскборд и память расходятся именно для авто-завершённых задач. `grep on_task_complete` подтверждает: ровно один продакшн-callsite.
- **Воспроизвести:** оркестратором `update_task(id, status="complete")` → проверить `<ws>/.pigmemory/tasks/<id>.md` — файла нет; через UI-кнопку — есть.
- **Фикс:**
  1. Перенести хук завершения **внутрь `TaskManager::update`** (после успешного UPDATE и `release_all_for_task`), чтобы он срабатывал на любом пути (UI/оркестратор/MCP).
  2. `TaskManager` нужен доступ к `MemoryService` — либо инъекция через конструктор, либо вынести хук в слой, через который проходят все три пути (но именно `update` — единственная общая точка).
  3. Убрать дублирующий вызов из `commands.rs:1213` после переноса.
  4. Эмит `memory://note.created` сделать опциональным (app может быть None в MCP), как уже сделано в `on_task_complete`.

### Bug 6.2 — обновление статуса задачи оркестратором/MCP не эмитит UI-событие → таскборд не обновляется в реальном времени
- **Severity:** high
- **Файл:** `tasks.rs:202-280` (`update` не эмитит ничего), `orchestrator/tools.rs:617` (dispatch update_task — нет emit), `commands.rs:1202` (Tauri-команда тоже не эмитит task-событие, только хук памяти)
- **Что ломается:** нет события вида `task://changed`. Когда оркестратор двигает `todo→in_progress→in_review`, фронтовый таскборд не получает уведомления — обновляется только при ручном refresh/повторном `list_tasks`. Пользователь смотрит на таскборд и видит устаревшие статусы, пока сам не перезапросит. Усиливает симптом «не понимаю, кто закончил» на уровне UI. (Сравни: spawn эмитит `agent://spawned`, layout — `EV_LAYOUT_CHANGED`, а у задач события нет вовсе — `grep EV_ tasks.rs` пусто.)
- **Воспроизвести:** оркестратор меняет статус задачи → UI-таскборд не меняется до ручного обновления.
- **Фикс:**
  1. Завести `EV_TASK_CHANGED` в `events.rs`.
  2. Эмитить его из общего места (см. Bug 6.1 — если `update` получит app-handle/emitter) на любое изменение статуса/assignee.
  3. Фронт подписывается и обновляет доску инкрементально.

### Bug 6.3 — `release_all_for_task` вызывается при complete/cancelled, но не при переходе в `in_review` — локи держатся, пока reviewer думает
- **Severity:** high
- **Файл:** `tasks.rs:224-239, 264-267` (`release_locks` только при `cancelled`/`complete`)
- **Что ломается:** файловые локи (`swarm::ownership`) снимаются только при переходе в `complete`/`cancelled`. Но дизайн с review-gate (Bug 3.5) предполагает: билдер закончил → `in_review` → reviewer смотрит → `complete`. Пока задача в `in_review`, билдер **держит локи на всех заклеймленных файлах**, и reviewer (или следующая задача) не может их заклеймить, чтобы внести правки. При типичном flow «билдер сдал, reviewer правит» возникает дедлок по владению файлами. А если задача застряла в `in_review` навсегда (Bug 3.5 — некому голосовать), локи висят вечно.
- **Воспроизвести:** claim_files задачей, перевести в `in_review`, попытаться claim тех же файлов от reviewer-задачи → blocked, хотя билдер уже «сдал».
- **Фикс:**
  1. Решить политику: либо снимать локи при `in_review` (билдер закончил писать), либо вводить «передачу владения» reviewer-задаче.
  2. Минимально — при `in_review` понижать локи до read-only или явно освобождать write-локи.
  3. Документировать, что `in_review` = «правки билдера закончены».

### Bug 6.4 — `delete_task` доступен в MCP-схеме (`is_mutating`/`is_dangerous`), но НЕ зарегистрирован в `tool_definitions`/`dispatch`
- **Severity:** med
- **Файл:** `mcp/server.rs:50, 74` (`delete_task` в is_mutating и is_dangerous), `orchestrator/tools.rs:17-234` (нет `delete_task` в `tool_definitions`), `tools.rs:260-748` (нет ветки `delete_task` в `dispatch`)
- **Что ломается:** `mcp/server.rs` классифицирует `delete_task` как mutating+dangerous, но такого инструмента **нет** ни в `tool_definitions()` (его не видно в `tools/list`), ни в `dispatch` (ветка отсутствует → `Err(Invalid("unknown tool: delete_task"))`). `TaskManager::delete` существует (`tasks.rs:282`) и вызывается из Tauri-команды (`commands.rs:1226`), но через оркестратор/MCP задачу **нельзя удалить**. Рассинхрон между списком scope-классификации и реальным реестром тулзов — мёртвая запись, вводящая в заблуждение (и потенциальный признак, что инструмент задумывался, но не доведён).
- **Воспроизвести:** MCP `tools/call delete_task` → "unknown tool: delete_task", хотя scope-проверка его знает.
- **Фикс:**
  1. Либо добавить `delete_task` в `tool_definitions` + ветку в `dispatch` (если удаление задач через оркестратора нужно).
  2. Либо убрать `delete_task` из `is_mutating`/`is_dangerous` в `mcp/server.rs` (если не нужно) — чтобы не было фантомной записи.

---

# Класс 7. SWARM-AUTH / СТРИМИНГ / SPAWN-CWD

### Bug 7.1 — спавненный агент не получает cwd проекта → билдер стартует в $HOME, не в воркспейсе
- **Severity:** critical
- **Файл:** `orchestrator/tools.rs:388-397` (`cwd` берётся только из args, не дефолтится на `ws.paths[0]`), `agentd/engine.rs:270-273` (fallback cwd = `$HOME` или `/tmp`)
- **Что ломается:** `spawn_agent` берёт `cwd` исключительно из аргументов вызова. Если модель его не передала (а в примерах промпта `spawn_agent(agent_type="aider")` — без cwd), агент спавнится с `cwd=None`. Брокер (engine.rs:270) делает fallback на `$HOME` (или `/tmp`). Поскольку broker запущен через `setsid` со stdio→/dev/null, его `$HOME`/cwd — это окружение демона, а не проект. Билдер (aider/claude) стартует **не в директории проекта**, не видит файлов воркспейса, `claim_files`/правки идут по неверным путям. Это прямой вклад в «агент не понимает, где работает». Воркспейс знает свой путь (`ws.paths[0]`), но он не прокидывается.
- **Воспроизвести:** `spawn_agent(agent_type="aider")` без cwd в воркспейсе с `paths=["/home/u/proj"]` → агент стартует в `$HOME`, `ls` показывает домашку, не проект.
- **Фикс:**
  1. В `spawn_agent` (tools.rs): если `cwd` не задан — дефолтить на `ws.paths.first()` текущего воркспейса.
  2. Если у воркспейса нет путей — явная ошибка/предупреждение, а не тихий старт в `$HOME`.
  3. Прокинуть это и в `restore_session`/`respawn_persisted`, чтобы реатач сохранял проектный cwd (сейчас `cwd` берётся из SQLite-зеркала — проверить, что туда писался проектный путь).

### Bug 7.2 — Anthropic retry/fallback ре-стримит в тот же `delta_tx` → дублирование текста в UI после частичного ответа
- **Severity:** high
- **Файл:** `orchestrator/providers/anthropic.rs:138-157` (`stream_once` шлёт дельты по мере чтения), `anthropic.rs:170-205` (`stream_with_retry` повторяет `stream_once` на тот же `delta_tx`)
- **Что ломается:** `stream_once` отправляет текстовые дельты в `delta_tx` по мере получения SSE. Если соединение упало **в середине** стрима (после нескольких дельт), `stream_once` вернёт Err, а `stream_with_retry` сделает новую попытку — снова `stream_once(&self.primary_model, req, delta_tx)` на **тот же** канал. Дельты первой (оборванной) попытки уже улетели в UI (`chat://chunk` по placeholder_id, mod.rs:422). Вторая попытка начинает текст заново → пользователь видит **дублированный/склеенный** префикс ответа. Финальный `placeholder.content` (из `into_response` последней попытки) при этом корректен, но UI-стрим уже показал мусор, и до перезаписи плейсхолдера пользователь видит дубль.
- **Воспроизвести:** оборвать соединение к Anthropic после первых дельт (или 529 в середине) → в стриминговом бабле дублируется начало ответа.
- **Фикс:**
  1. Буферизовать дельты per-attempt и отправлять в `delta_tx` только после успешного завершения попытки (теряем чистую инкрементальность, но без дублей).
  2. Либо: при старте retry слать в UI «маркер сброса» (очистить бабл) перед повторным стримом — фронт обнуляет накопленный текст по `id`.
  3. Либо retryable-ошибки определять **до** первой дельты (по HTTP-статусу в `stream_once` ответ проверяется до чтения тела — 5xx/429 ловятся там, dropping mid-stream — нет; именно mid-stream обрыв опасен).

### Bug 7.3 — `vote_review_gate` не проверяет, что голосующий — назначенный reviewer (любой может проголосовать за чужой gate)
- **Severity:** high
- **Файл:** `swarm/review.rs:80-93` (`vote` обновляет по `gate_id` без проверки голосующего), `swarm/tools.rs:290-295` (dispatch не передаёт идентичность голосующего)
- **Что ломается:** `vote(db, gate_id, verdict, reason)` обновляет gate **только по id**, без какой-либо привязки к тому, кто голосует. `gate.reviewer_id` существует, но не сверяется. Значит билдер может сам проголосовать `pass` за свой gate и закрыть задачу в обход ревью — вся суть review-gate (Bug 3.5) обнуляется. `swarm::tools::dispatch("vote_review_gate")` даже не принимает идентичность голосующего. Для системы, где review-gate — единственный барьер качества, это дыра.
- **Воспроизвести:** открыть gate с `reviewer_id="r1"`, вызвать `vote(gate_id, pass)` от имени любого агента → проходит.
- **Фикс:**
  1. `vote` должен принимать `voter_id` и сверять с `gate.reviewer_id` (если он задан) — иначе `Err(Invalid("not the assigned reviewer"))`.
  2. Опционально проверять, что у голосующего `role='reviewer'` (через `agent_role`).
  3. `swarm/tools.rs` добавить `reviewer_id`/`voter_id` в схему `vote_review_gate` и прокинуть.

### Bug 7.4 — `open_review_gate` не валидирует существование `task_id` и `reviewer_id`
- **Severity:** med
- **Файл:** `swarm/review.rs:57-78` (`open` проверяет только непустоту `task_id`)
- **Что ломается:** `open` вставляет gate с любым `task_id` (проверка только `.trim().is_empty()`) и любым `reviewer_id` без FK-валидации. Можно открыть gate на несуществующую задачу или назначить несуществующего ревьюера. Gate на фантомную задачу никогда не разблокируется/не нужен; gate с несуществующим reviewer_id (Bug 3.6 — reviewer'ов вообще нет) гарантированно висит Pending. Мусорные gate засоряют `task_completable`.
- **Воспроизвести:** `open_review_gate(task_id="nonexistent")` → успех, мёртвый gate.
- **Фикс:**
  1. Проверять `SELECT COUNT(*) FROM tasks WHERE id=?` перед вставкой.
  2. Если `reviewer_id` задан — проверять существование агента и (после Bug 3.6) его роль `reviewer`.

### Bug 7.5 — MCP-сервер слушает `127.0.0.1` но без CSRF/Origin-защиты; любой локальный процесс с ключом = полный доступ к спавну/удалению
- **Severity:** high
- **Файл:** `lib.rs:297` (`bind = 127.0.0.1:port`), `mcp/server.rs:178-210` (Bearer-only), `mcp/launcher.rs` (`tile-claude` авто-ключ со scopes read,mutate,dangerous)
- **Что ломается:** MCP-сервер биндится на loopback (хорошо), аутентификация — Bearer-ключ. Но: (1) авто-ключ `tile-claude` (по карте — scopes `read,mutate,dangerous`) выдаётся каждому спавненному claude-тайлу и кладётся в `--mcp-config`/`.mcp.json`; любой процесс, прочитавший этот файл (или env), получает **dangerous**-доступ: `spawn_agent`, `delete_workspace`, `send_to_agent` в любой воркспейс. (2) Нет проверки `Origin`/`Host` — а значит браузер на localhost (DNS-rebinding на 127.0.0.1, или вредная веб-страница) может слать POST на `/mcp`, если узнает ключ. (3) `initialize` анонимен и раскрывает версию/возможности без ключа — мелкая утечка. Для «dangerous»-операций (спавн процессов, удаление) loopback+статичный файловый ключ — слабая граница.
- **Воспроизвести:** прочитать `<cwd>/.mcp.json` или env спавненного тайла → достать `pk_…` → curl `tools/call spawn_agent` → выполнить.
- **Фикс:**
  1. Проверять заголовок `Origin`/`Host` — отвергать кросс-origin (защита от DNS-rebinding).
  2. `tile-claude` ключу давать минимальный scope (read+mutate), а `dangerous` — только явно выданным ключам; либо ограничить dangerous-операции отдельным per-tile коротким TTL-токеном.
  3. Хранить ключ не в `.mcp.json` внутри проекта (часто коммитится/расшарен), а в защищённом per-user месте; ротировать.
  4. `initialize` — оставить анонимным только минимальный ответ (уже почти так), не раскрывать список тулзов до auth (он и так за auth — ок).

### Bug 7.6 — `wait_for_agent_idle` гонит broker `last_stdout` против локального кэша; write сбрасывает оба, но stdout-таймер ведёт только pump → ложный «idle» сразу после своего же write
- **Severity:** med
- **Файл:** `agent.rs:479-496` (`write` локально пишет `last_stdout=now`), `agent.rs:230-235` (`last_stdout_age` читает локальный кэш), `orchestrator/tools.rs:497-506` (idle если `age >= quiet`)
- **Что ломается:** `send_to_agent` → `write` ставит `last_stdout[agent]=now` локально. `wait_for_agent_idle` ждёт `age >= quiet_ms` (default 1500). Сразу после write `age` мал → ок. Но обновляет `last_stdout` **только** event-pump на реальном stdout (agent.rs:636) и сам `write`. Если агент после промпта «думает» >1500ms без вывода (claude/aider часто молчат на старте дольше секунды), `last_stdout_age` пересечёт quiet-порог раньше, чем агент начнёт отвечать → `wait_for_agent_idle` вернёт `idle`, оркестратор решит «агент закончил/готов», прочитает пустой `tail_agent` и продолжит, **не дождавшись ответа**. quiet=1500ms слишком мал как «агент закончил» для LLM-CLI. Связано с Bug 2.8 и 3.3, но конкретно здесь — гонка таймера для одного агента.
- **Воспроизвести:** `send_to_agent(brief)`, сразу `wait_for_agent_idle(quiet_ms=1500)` → вернёт idle во время «раздумий» агента до первого токена.
- **Фикс:**
  1. Не считать «idle», пока агент не выдал **хотя бы один** stdout-чанк после нашего write (трекать «был ли вывод с момента последнего write»).
  2. Поднять дефолт `quiet_ms`/ввести «минимальное время до первого вывода».
  3. Возвращать структурированный статус: `idle_before_any_output` vs `idle_after_output`, чтобы оркестратор различал «ещё не начал» и «закончил».

---

## Карта зависимостей фиксов (что чинить первым)

1. **Bug 3.6 + Bug 2.5** (роли + идентичность координатора) — фундамент. Без них классы 2/3/6/7 не лечатся.
2. **Bug 5.1** (реконнект к брокеру) — без него любой сбой брокера = мёртвый PigIDE; блокирует всё взаимодействие с агентами.
3. **Bug 7.1** (cwd проекта при спавне) — без него билдер работает не в той директории; корень «не понимаю, где я».
4. **Bug 2.1 + 2.2** (focused agent + валидация получателя) — убирает «промпт не туда».
5. **Bug 6.1 + 6.2** (хук завершения + событие задачи на ВСЕХ путях) — таскборд/память синхронны при авто-завершении.
6. **Bug 4.1 + 4.3** (attachments доходят до модели + правильная сессия) — устраняет тихую потерю пользовательского ввода и перемешивание сессий.
7. **Bug 1.1 + 1.2 + 1.3** (частичный фантом + не-тихий-truncate + честный resolved) — убирает «сказал, что сделал».
8. **Bug 3.1 + 3.2** (mail-pump + авто-переход статусов) — оживляет таскборд.
9. **Bug 7.3 + 7.4 + 6.3** (auth ревью-голосов + валидация gate + снятие локов в in_review) — делает review-flow рабочим и безопасным.
10. **Bug 3.3 + 3.4 + 7.6** (убрать ложный IdleDone + гонку таймера) — точность done-сигнала.
11. **Bug 7.2** (дубль стрима при retry) — UX стриминга.
12. **Bug 5.2 + 4.4 + 5.5** (cache-split скиллов, лимит итераций, токен-оценка кириллицы) — стоимость/длина контекста.
13. **Bug 7.5** (MCP Origin/scope hardening) — безопасность dangerous-операций.
14. Остальное (1.4-1.6, 2.3-2.4, 2.6-2.8, 3.5, 4.2, 4.5, 5.3, 5.4, 6.4) — добивка по severity.

## Чего НЕ проверял (честно)
- Не запускал сборку/тесты (read-only аудит по требованию задачи).
- Frontend (focus-tracking, рендер таскборда, обработка `chat://chunk`) — вне scope backend.
- Реальные cache-hit метрики Anthropic (Bug 5.2), токен-расхождение (Bug 5.5), дубль-стрим (Bug 7.2) — выводы из кода, без живых запросов.
- Реальные тайминги гонок (Bug 2.3, 5.1, 7.6) — статический вывод по коду, без рантайм-повтора.
- `commands.rs` (2189 строк) прочитан точечно: send_chat, sessions, restore, update_task/delete_task, list_tasks. Остальные ~115 команд — не построчно.
- `mcp/launcher.rs`, `mcp/auth.rs` — не открывал построчно; выводы о `tile-claude` scope (Bug 7.5) опираются на [[pig-ide-subsystems]] + сигнатуры в `resolve.rs`. **Стоит дочитать перед фиксом 7.5.**
- `lib.rs` boot-ordering прочитан частично (MCP bind, watcher) — полную последовательность инициализации не верифицировал.
- `agentd/proto.rs`, `framing.rs`, `server.rs` (broker wire) — не открывал; протокол-уровневые баги (фрейминг, версии) вне текущего прохода.
- Memory smart-lane (`smart.rs`, `hot.rs`), project_resolver, voice, ssh, skills/claude_import — вне фокуса (оркестрация/агенты/таскборд).
