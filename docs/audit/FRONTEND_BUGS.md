# PIGIDE — Frontend Audit (bugs / defects / risks)

> Scope: всё содержимое `frontend/src/**/*.ts(x)` (47 файлов, ~14 057 строк).
> Read-only аудит. Без правок кода. Покрывает: race conditions, утечки
> слушателей, `useEffect`-deps, ResizeObserver, debounce потерю финального
> значения, инварианты `maximizedLeafId/focusedLeafId`, XSS в чате,
> отсутствие `error-boundary`, доступность, мемоизацию, фильтрацию
> control-chars в `ipc.writeToAgent`, N+1 ререндеры Allotment, очистку
> стейта при switch workspace, security-замечания.
>
> Severity scale: **critical** (потеря данных / утечка памяти / RCE-class) ·
> **high** (ломает сценарий у заметной части пользователей) ·
> **medium** (неочевидный дефект, проявится при определённых действиях) ·
> **low** (косметика / стиль / a11y / редкий edge-case).

---

## TL;DR — Top 5 critical / high

| # | Sev | Файл / место | Кратко |
|---|-----|--------------|--------|
| 1 | **critical** | `components/TilingArea.tsx:47-53` | `persistLayout` debounce 200 ms читает `currentId` из closure — изменение split запишется в **уже переключённый** workspace. |
| 2 | **critical** | `components/PigMemoryWorkbench.tsx:233-290`, `components/SkillsPanel.tsx:53-66`, `components/ArchitectPanel.tsx:55-79` | Паттерн `unsubs.push(promise.then(u => u))` — если компонент unmounts ДО резолва `onXxx(...)` promise, листенер **зарегистрируется и не отпишется**. Реальная утечка IPC-слушателей на каждом mount/unmount. |
| 3 | **high** | `components/TilingArea.tsx:96-111` | Inline `<Allotment defaultSizes={...node.ratio...}>` создаётся на каждом ре-рендере split-нода, а `setRatio` через `onChange` триггерит `setLayout` → `renderNode` → новый Allotment. Цикл пересоздаёт `defaultSizes` и сбрасывает ratio пользователя. |
| 4 | **high** | `components/AgentTile.tsx:170-202` | Race `agentLogTail` vs `onAgentStdout`: подписка на live-стрим создаётся **после** fetchTail. Любые байты, эмитированные в окне fetch, теряются. Плюс двойная отрисовка при пересечении. |
| 5 | **high** | `state/store.ts:157-172` (`appendChatChunk`) | На каждом streaming-чанке (10–100/сек) — `s.chat.slice()` + `[...next, updated]` = O(n). Чат >5k сообщений → видимый jank. |

Остальные находки — ниже, сгруппированы по категориям.

---

## 1. Race conditions

### 1.1 [critical] `persistLayout` debounce пишет в чужой workspace
- **Файл:** `frontend/src/components/TilingArea.tsx:47-53`
- **Описание:** `persistLayout` планирует `setTimeout(... ipc.updateLayout(currentId, next) ..., 200)`. `currentId` берётся из closure `useEffect`-dep'ов (внешний `currentId` из store) и из `TilingArea` — НЕ пересоздаёт таймер при смене workspace. Если пользователь перетащил split и за 200 ms успел переключить workspace (Ctrl+1..9 / клик в sidebar), отложенный `updateLayout` запишет **свежий layout** уже в **новый** workspace, повредив его серверное состояние.
- **Шаги воспроизведения:**
  1. Открыть workspace A.
  2. Перетащить split-границу.
  3. Сразу (<200 ms) Ctrl+1 → переключиться в workspace B.
  4. Через 200 ms backend получит `updateLayout(B_id, layout_A)`.
- **Фикс:** В `setLayout(next)` синхронно сбрасывать debounce; в `persistLayout` хранить `workspaceId` **вместе** с `next` (либо класть в ref), либо `clearTimeout` на `currentId` change. Альтернатива — оптимистично `setLayout(next) + ipc.updateLayout(currentId, next)` без debounce, положившись на Tauri-batching на стороне Rust.

### 1.2 [critical] IPC-listener leak — `unsubs.push(promise.then(u => ...))`
- **Файлы:**
  - `components/PigMemoryWorkbench.tsx:233-290` (`onMemoryNoteCreated`)
  - `components/SkillsPanel.tsx:53-66` (`onSkillsReloaded`, `onSkillsError`)
  - `components/ArchitectPanel.tsx:55-79` (`onArchitectDecision`, `onArchitectSignal`)
  - `components/OrchestratorPanel.tsx:79-94` (`onProviderChanged`)
  - `components/voice/DictionaryEditor.tsx`, `VoiceHistory.tsx`, `VoiceDashboard.tsx` (однократные — низкий риск)
- **Проблема:** Все используют `track(p.then(u => { if (disposed) u(); else unsubs.push(u); }))`. Promise может зарезолвиться **после** unmount. `disposed=true` в этом случае НЕ спасёт: `if (disposed) u()` отпишет, ОК. НО в `SkillsPanel` и `ArchitectPanel` тот же шаблон **без** проверки `disposed`:
  ```ts
  onSkillsReloaded(() => refresh()).then((u) => unsubs.push(u));
  ```
  Если mount → unmount до резолва — promise резолвится, `u` кладётся в `unsubs`, но cleanup уже отработал → `unsubs.forEach((u) => u())` ничего не сделает, потому что cleanup не вызывается повторно. Listener остаётся зарегистрированным в Tauri навечно, дёргает `refresh()` (уже не-mounted компонент — приведёт к `setState`-on-unmounted warning).
  Тот же баг в `ArchitectPanel` (lines 67-78), `OrchestratorPanel` (line 89: `const un = onProviderChanged(reload); ... un.then((f) => f())` — тут cleanup корректен, await + unlisten, ✓).
- **Фикс:** Шаблон должен быть:
  ```ts
  let dead = false;
  const p = onX((payload) => { if (dead) return; handler(payload); });
  p.then((u) => { if (dead) u(); else unsubs.push(u); }).catch(() => undefined);
  return () => { dead = true; unsubs.forEach((u) => u()); };
  ```
  И для emit-обработчика внутри — `if (dead) return;` (или захват через ref), чтобы не было `setState` после unmount.

### 1.3 [high] `agentLogTail` race против live `onAgentStdout`
- **Файл:** `components/AgentTile.tsx:170-202`
- **Проблема:** Tail-fetch (`ipc.agentLogTail(agentId, 64 KiB)`) запускается **до** подписки на live stdout. Между `feed(tailBytes)` и `onAgentStdout(...).then(u => unsubStdout = u)` — окно ~5–50 ms. Байты, эмитированные в этом окне, потеряны: tail уже написан, live ещё не слушает. Комментарий на 166-169 утверждает, что "Run BEFORE subscribing" — но `subscribe.then(...)` это микротаска после mount, не синхронный код; race реален.
- **Шаги воспроизведения:**
  1. Восстановить сессию.
  2. AgentTile mount'ится, дёргает `agentLogTail`.
  3. Пока tail идёт по IPC, agent пишет в PTY (типично: приветственный prompt shell).
  4. `onAgentStdout` ещё не зарегистрирован → chunk потерян.
- **Фикс:** Сначала `onAgentStdout(...).then(u => unsubStdout = u)`, **потом** `agentLogTail`. Tail в parser подаётся в ту же инстанцию `CommandBlockParser`, чтобы при повторении граничных байтов не было двойной отрисовки. Альтернативно: backend дедуплицирует по `seq`-номеру.

### 1.4 [high] `useAgentSummary` не успевает подписаться
- **Файл:** `hooks/useAgentSummary.ts:92-106`
- **Проблема:** `onAgentStdout(...).then((u) => { if (disposed) u(); else unsub = u; })` — но `disposed` ставится только в cleanup. Первые stdout-байты, прилетевшие между mount и резолвом `onAgentStdout`, отбрасываются на `if (e.agent_id !== agentId) return` — нет, **проходят** через callback, потому что `listen(...)` уже активен. Реальный баг: `lastLine` остаётся `null`, пока первый чанк не дойдёт. Minor.
- **Реальный риск:** `taskTitleRef.current` инициализируется в первом useEffect (line 64-66), но `recompute` планируется `schedule()` — если первый stdout пришёл ДО `schedule()` (line 109), `pending` = null и `schedule` сработает через `setInterval(1000ms)` на строке 108. До 1 секунды summary остаётся "idle" — **low UX bug**.

### 1.5 [medium] `MemoryPanel` / `KanbanBoard` race при смене workspace
- **Файлы:** `components/MemoryPanel.tsx:33-56`, `components/KanbanBoard.tsx:30-40`, `components/PromptsPanel.tsx:30-48`
- **Проблема:** `reload()` async без cancellation. Быстрый switch workspace A → B → A может дать interleaving: A's list, B's list, A's list — но `KanbanBoard.taskList` фильтрует по `currentId`, так что утечка UI не видна. В `MemoryPanel.openNote` (line 77) — если успели нажать note в A, потом switch в B, `setOpenNote(n_A)` ставится уже после `setOpenId(null)` от useEffect. **Приводит к "note открыта, но не выделена" — low confusion.**

### 1.6 [medium] `PathMentionTextarea` path-suggest race
- **Файл:** `components/PathMentionTextarea.tsx:128-153`
- **Проблема:** `SUGGEST_DEBOUNCE_MS = 90`. Нет `AbortController` / request-id guard. Ответ с устаревшим `q` (набранным раньше) может перезаписать более свежий. Тригерится, если пользователь печатает быстро и бэкенд медленный.
- **Фикс:** Добавить `let req = 0;` с инкрементом перед каждым fetch, отбрасывать `rows` с устаревшим id.

### 1.7 [low] `PigMemoryWorkbench.createNote` использует `window.prompt`
- **Файл:** `components/pigmemory/PigMemoryWorkbench.tsx:401`
- **Проблема:** Блокирующий native `prompt()`. В Tauri-webview работает через `window.__TAURI_INTERNALS__`-prompt-bridge, но не нативно модально. Низкий риск — UX only.

---

## 2. Утечки слушателей / ResizeObserver

### 2.1 [medium] `useAgentSummary` interval + listener
- **Файл:** `hooks/useAgentSummary.ts:107-116`
- **Проблема:** `tickId = setInterval(schedule, 1000)` чистится в cleanup. `unsub` чистится. OK. **Но:** `lastLine`, `lastStdoutAt`, `buf` хранятся в closure и при remount новой инстанции хука теряются — если `agentId` не меняется, `useEffect` не перезапускается, OK. При смене `agentId` (та же `<AgentTile>`-инстанция? нет — key={agent.id} гарантирует новый mount) — OK.

### 2.2 [low] `MemoryGraph` / `PigMemoryGraph` ResizeObserver
- **Файлы:** `components/MemoryGraph.tsx:30-39`, `components/pigmemory/PigMemoryGraph.tsx:72-81`, `components/OrchestratorPanel.tsx:117-127`, `components/NoteList.tsx:47-55`
- **Проблема:** Все disconnect() в cleanup. ✓ OK. **Но:** `OrchestratorPanel` создаёт 2 observer'а (для user-scroll detection и resize). На каждый ре-рендер (например, новый chat chunk → `chat` ref меняется → useEffect на 111 пересоздаёт? нет, deps — `[]`). **Verified OK.**

### 2.3 [medium] `PigMemoryGraph` window keydown/keyup
- **Файл:** `components/pigmemory/PigMemoryGraph.tsx:83-96`
- **Проблема:** `egoMode` listener на window. Disconnect в cleanup ✓. **Но:** если две `<PigMemoryGraph>`-инстанции (sidebar + full-graph — `PigMemoryWorkbench` использует только full-graph) — OK. Если позже добавится — конфликт egoMode на оба. Low.

### 2.4 [low] `AgentTile` ResizeObserver дергает `resizeAgent` на любой ресайз
- **Файл:** `components/AgentTile.tsx:154-164`
- **Проблема:** Без debounce. Каждый pixel-reсайз → IPC roundtrip. **Medium perf на медленных машинах.** Плюс race: если `termDisposedRef.current = true` (cleanup уже вызван), `ro.callback` всё равно дёрнет `fit.fit()` (Term disposed) → throw, ловится в `try/catch`. OK.

### 2.5 [medium] Global keyboard listener в `useHotkeys`
- **Файл:** `hooks/useHotkeys.ts:107-110`
- **Проблема:** `document.body.addEventListener('keydown', ...)`. Cleanup ✓. **Но:** `keysSignature` dep — при изменении map'а listener отвязывается и привязывается заново. Между detach и attach — события теряются (sub-ms, низкая вероятность).

---

## 3. useEffect deps / stale closure

### 3.1 [medium] `TilingArea` setRatio — stale `layout`
- **Файл:** `components/TilingArea.tsx:55-59, 99-105`
- **Проблема:**
  ```ts
  const setRatio = (path, ratio) => {
    const next = setRatioAt(layout, path, ratio);
    setLayout(next); persistLayout(next);
  };
  ```
  `layout` захвачен из closure. После первого `setLayout` React re-render даст новый `layout`, новая `setRatio` замыкается на новый layout. **OK** — функция пересоздаётся каждый рендер. **Но:** `<Allotment onChange={(sizes) => { ... if (...) setRatio(path, r) }}>` — `onChange` callback inline, при каждом ререндере Allotment получает новую функцию. Allotment 0.x+ это толерирует (debounced internal), но на drag-стриме это лишний GC-pressure. **Wrap in `useCallback`.**

### 3.2 [medium] `PigMemoryWorkbench` auto-save callback chain
- **Файл:** `components/pigmemory/PigMemoryWorkbench.tsx:388-397`
- **Проблема:** `saveActive` useCallback, deps `[s.activeId, s.active, s.draftTitle, s.draftBody, s.draftTags, ...]`. На каждый draft char — новый `saveActive`, новый useEffect re-run, clearTimeout + новый setTimeout. **Perf**, не correctness. Можно вынести в ref.

### 3.3 [medium] `WorkspaceSidebar.switchTo` vs `HotkeyBindings.switchWorkspace` расхождение
- **Файлы:** `components/WorkspaceSidebar.tsx:39-51` vs `components/HotkeyBindings.tsx:60-83`
- **Проблема:** Два пути одной логики, расходятся:
  | шаг | Sidebar.switchTo | Hotkey.switchWorkspace |
  |-----|------------------|-----------------------|
  | clearWorkspaceState | ✓ | ✓ |
  | setCurrent | ✓ | ✓ |
  | setLayout | ✓ | ✓ |
  | setAgents | ✓ | ✓ |
  | **setTasks** | **✗** | ✓ |
  - `Sidebar` забывает `setTasks` → при переключении из sidebar tasks остаются от предыдущего ws, потом `KanbanBoard.useEffect` перезапишет через `setTasks([])`. **Race window ~50–200 ms** между switch и `listTasks` — в `OrchestratorPanel.workspaceTasks` (line 185-191) отображаются старые tasks. **High inconsistency.**
- **Фикс:** Один общий хелпер `useSwitchWorkspace()` или оба пути вызывают `reloadAfterSwitch` из `App.tsx` (line 60-76), которая правильно всё перезагружает.

### 3.4 [low] `MentionTextarea` trigger-effect deps
- **Файл:** `components/MentionTextarea.tsx:102-114`
- **Проблема:** `useEffect` deps `[trigger, currentId]`. При смене `trigger` (каждое нажатие клавиши в textarea) → `listTasks` fire. **N+1** на каждое нажатие. Нет debounce.

### 3.5 [low] `FilesPanel` `useEffect` deps `[stack, pushToast]`
- **Файл:** `components/FilesPanel.tsx:51-58`
- **Проблема:** `pushToast` — Zustand setter, стабилен. ✓ OK. **Но:** при каждом нажатии ".." в breadcrumb — full IPC reload. Допустимо.

### 3.6 [low] `App.tsx:79-117` initial-load без cancellation
- **Файл:** `App.tsx:79-117`
- **Проблема:** Mount → async chain без abort. StrictMode double-mount в dev → 2 параллельных initial-load → race, последний wins. `setWorkspaces(list)` дважды. **OK на prod, раздражает в dev.**

### 3.7 [medium] `KanbanCard.dragId` через ref не через state
- **Файл:** `components/KanbanBoard.tsx:166-200`
- **Проблема:** OK, но `onDragOver` проверяет `if (dragId) e.preventDefault()`. Если drag из другого source (внешний) — `dragId=null`, `preventDefault` не вызовется, drop не сработает. OK by design.

### 3.8 [medium] `MemoryPanel.isDirty` через ref
- **Файл:** `components/MemoryPanel.tsx:31, 78, 81, 100, 304, 309, 314`
- **Проблема:** `isDirty.current = true` в 3 местах, проверка только в `open()` (line 78). **Если пользователь изменил draft, не нажал save, и закрыл окно через крестик (`onClose` callback) — `isDirty` не учитывается. Данные теряются без warning.** High в MemoryPanel, потому что это дефолт-флоу.

### 3.9 [low] `PigMemoryWorkbench` reducer `search` vs `searchDeb`
- **Файл:** `components/pigmemory/PigMemoryWorkbench.tsx:130-136, 293-298`
- **Проблема:** Два отдельных поля в state — input мгновенный, debounced копия. OK. ✓

### 3.10 [medium] `useTheme` глобальная mutable-константа
- **Файл:** `themes/useTheme.ts:8-37`
- **Проблема:** `currentId` объявлен модульно, обновляется `setTheme()`. **`useSyncExternalStore` использует getSnapshot(), возвращающий `currentId` — но identity стабильна (string), OK. Но:** при mount, `useThemeBootstrap()` асинхронно читает из IPC и в окне 5-50 ms `useTheme()` вернёт `DEFAULT_THEME_ID` пока bootstrap не завершился. Компоненты, использующие `theme.xterm` (AgentTile), создадут xterm со старой темой. **Minor.**

---

## 4. maximized / focused leaf при удалении / close

### 4.1 [low] `maximizedLeafId` cleanup
- **Файл:** `state/store.ts:211-218` (`clearWorkspaceState`)
- **Проблема:** Сбрасывает `maximizedLeafId: null`, `focusedLeafId: null`, `layout: { type: "empty" }`, `agents: {}`, `tasks: {}`. **Но:** не сбрасывает `showTaskBoard`, `showPigMemory`, `newWorkspaceModalOpen` — минор.

### 4.2 [low] `closeLeaf` (tree) — `closed leaf id == maximizedLeafId`
- **Файл:** `layout/tree.ts:86-98`
- **Проблема:** `closeLeaf` находит leaf и ставит `{ type: "empty" }`. **НЕ проверяет** `maximizedLeafId` — после удаления maximized лифа, `TilingArea.tsx:149-173` рендерит `agents[maximizedLeafId]` → undefined → `null` (line 149 `&& agents[maximizedLeafId]`), OK. **Но** store `maximizedLeafId` остаётся. `App.tsx:130-143` `onAgentExit` → `closeLeaf` + `setLayout` — не сбрасывает `maximizedLeafId`. **Stale state**, влияет только на edge-case: новый спан с тем же id уже не появится (UUID), значит баг невидим. ✓

### 4.3 [low] `replaceLeafId` при respawn
- **Файл:** `components/AgentTile.tsx:316-332`
- **Проблема:** Respawn → `replaceLeafId(layout, oldId, newId)` → setLayout. **Focused** leaf (`focusedLeafId`) может стать `oldId` — пользователь respawn'ил сфокусированный tile, focus теряется. **Low UX.**

---

## 5. MentionTextarea / @-mentions валидация

### 5.1 [medium] Short-id collisions в `MentionTextarea`
- **Файл:** `components/MentionTextarea.tsx:155`
- **Проблема:** `tag = @agent:${s.id.slice(0, 8)}` — только 8 hex. UUIDs коллизий не дают (random), но для не-UUID id возможно. Бэкенд парсит префикс и матчит. **Edge case, low.**

### 5.2 [medium] Mention tag = short-id, а не full-id
- **Файл:** `components/PathMentionTextarea.tsx:208-210` (аналогично)
- **Проблема:** Агент `@agent:abc12345` — бэкенд должен резолвить по short-id. Если `listAgents` сейчас не вернёт нужного (поменялся workspace), тег протух. **Medium UX.**

### 5.3 [low] `uniqueLabel` для path-attachments
- **Файл:** `components/pathMentionHelpers.ts:106-112`
- **Проблема:** `appends #2, #3, ...` — два файла `foo.txt` становятся `foo.txt` и `foo.txt#2`. **OK** — пользователь видит, не путается.

---

## 6. XSS / sanitization в чате

### 6.1 [high] `Markdown` `parseInline` — комбинация escape + regex
- **Файл:** `components/Markdown.tsx:14-39, 30-37, 195-215, 91-129, 131-183`
- **Анализ:** `parseInline` сначала `escapeHtml`, потом `replace` по regex для `<strong>`, `<em>`, `<code>`, `<a>`. **После escape HTML-символов regex может не найти паттерны**, например `**<x>` → `**&lt;x&gt;**` — `\*\*([^*]+)\*\*` сматчит `&lt;x&gt;` и сделает `<strong>&lt;x&gt;</strong>`. Безопасно. ✓
  - **Link regex `\[([^\]]+)\]\(([^)]+)\)`** — захватывает URL, потом `sanitizeUrl` (markdownSanitize.ts:14-28). `sanitizeUrl` допускает `javascript:` через decode → strip whitespace → match scheme. `jscript:`, `vbscript:`, `data:`, `file:` — все не в SAFE_SCHEMES, reject. ✓
  - **Edge:** `javascript&colon;` или `&#x6A;avascript:` — после escapeHtml `&` → `&amp;`, `sanitizeUrl` делает `&amp;colon;` → strip whitespace, всё равно не матчит SAFE_SCHEMES. **OK.** Но `&#x6A;avascript:` после entity-decode в `sanitizeUrl` (line 16-20) — decode `&amp;` → `&`, но `&#x6A;` НЕ декодируется. **Safe.** ✓
  - **Markdown.tsx НЕ обрабатывает** raw HTML в теле (нет `<details>` / `<input>` рендера). ✓
- **Вердикт:** Безопасно. ✓

### 6.2 [low] `MemoryPanel.highlight` — snippet double-encode
- **Файл:** `components/MemoryPanel.tsx:363-372`
- **Проблема:** Эскейпит `&/</>` → заменяет `&lt;&lt;` на `<mark>`, `&gt;&gt;` на `</mark>`. **OK** для FTS5-marker, но если snippet содержит `<<` (буквально два символа) после FTS-прохода, который уже эскейпил, не подсветит, а если FTS вернул неэскейпленный snippet — два `<` подряд не считаются тегом. **Verified safe via ordering.**

### 6.3 [low] `PigMemoryWorkbench.highlightFtsSnippet` — duplicate
- **Файл:** `components/pigmemory/PigMemoryWorkbench.tsx:1146-1154`
- **Проблема:** Дубликат логики `MemoryPanel.highlight` (3 копии в проекте: MemoryPanel, NoteList, PigMemoryWorkbench). **Low refactor.**

### 6.4 [medium] `PigMemoryEditor` wikilink — data-attr injection
- **Файл:** `components/pigmemory/PigMemoryEditor.tsx:118-130`
- **Проблема:** Через `Decoration.mark({ class })` рендерится в CodeMirror — безопасно (class — фиксированная строка). ✓

### 6.5 [medium] `MemoryGraph` / `PigMemoryGraph` nodeLabel → Canvas fillText
- **Файлы:** `components/MemoryGraph.tsx:82`, `components/pigmemory/PigMemoryGraph.tsx:312-318`
- **Проблема:** `nodeLabel={(n) => escapeHtml(...)}` — escape не нужен для Canvas (текст не парсится). Но `nodeCanvasObject` рисует `ctx.fillText(node.title || ...)`, где `node.title` — пользовательский текст заметки. **Если в title есть 4-byte UTF-8 (эмодзи) — ок, Canvas поддерживает.** ✓

### 6.6 [medium] `BrowserPanel` iframe + `sandbox` флаги
- **Файл:** `components/BrowserPanel.tsx:171`
- **Проблема:** `sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-downloads allow-presentation"`. `allow-same-origin + allow-scripts` = iframe имеет доступ к cookies своего origin, может читать localStorage. **В Tauri-webview origin — localhost:1420, sensitive data НЕ там, но devtools и rust-side state — да.** Plus `allow-popups` — может открыть `window.open(...)` → попап вне Tauri-webview. **Medium security.**

### 6.7 [low] `DictionaryEditor.quickAdd` — `window.getSelection()`
- **Файл:** `components/voice/DictionaryEditor.tsx:86-97`
- **Проблема:** Текст selection → `newPattern`. **Не валидируется** — пользователь может вставить control-chars / shell-метасимволы. Бэкенд должен regex-escape при подстановке, иначе RCE в voice pipeline. **Low, depends on backend.**

---

## 7. Отсутствие error boundary

### 7.1 [high] Нет ни одного `ErrorBoundary` / `componentDidCatch` / `useErrorBoundary`
- **Проявление:** Crash в любом `<Markdown>` (regex бесконечный loop на странном input), `<MarkdownPreview>` (`renderMarkdown` может упасть на null `notes`), `<PigMemoryGraph>` (force-graph2d на 10k нод), `<AgentTile>` (xterm dispose race) — **белый экран**, потому что `<StrictMode><App/></StrictMode>` без boundary пускает throw наверх.
- **Фикс:** Обернуть:
  - `<TilingArea>` — per-pane, чтобы один crashed tile не убил sidebar/orchestrator.
  - `<SettingsButton>` panel — чтобы сбойная panel не валила Settings.
  - `<OrchestratorPanel>` — чтобы Markdown/ChatMessageView не валили input.
  - Top-level — fallback "PigIDE crashed, reload" с кнопкой `ipc.reload()`.

---

## 8. Доступность (a11y)

### 8.1 [low] `App.tsx:271-282` toast — `role="status" + role="alert"`
- **Файл:** `App.tsx:271-282`
- **Проблема:** `<div role="status" aria-live="polite">` обёртка + `<div role="alert">` (должно быть `aria-live="assertive"` на нём, но контейнер уже polite). Конфликт. Screen reader прочитает оба.
- **Фикс:** Контейнер `role="region" aria-label="notifications"`, дети `role="status" aria-live="polite"` для info, `role="alert" aria-live="assertive"` для error.

### 8.2 [low] `MentionTextarea` / `PathMentionTextarea` — нет `aria-activedescendant`
- **Файлы:** `components/MentionTextarea.tsx:221-238`, `components/PathMentionTextarea.tsx:339-359`
- **Проблема:** `role="listbox"`, `role="option"`, `aria-selected`, но `aria-activedescendant` на textarea не выставлен.
- **Фикс:** `<textarea aria-activedescendant={listboxId-${activeIdx}}>`.

### 8.3 [low] `Pill button` — `BridgeOrb`
- **Файл:** `components/OrchestratorPanel.tsx:411-419`
- **Проблема:** `aria-pressed` = `isRecording` (boolean). При `transcribing` состоянии — не pressed, что OK, но состояние "transcribing" недоступно через screen reader (только `aria-label`).
- **Фикс:** Добавить `aria-busy={isTranscribing}`.

### 8.4 [low] `MemoryPanel` / `NoteList` — `role="button"` без `aria-roledescription`
- **Файлы:** `components/MemoryPanel.tsx:262-265`, `components/pigmemory/NoteList.tsx:124-130`
- **Проблема:** `<div role="button" tabIndex={0} onKeyDown={Enter/Space}>`. ✓ Корректно. **OK.**

### 8.5 [low] `KanbanCard` drag — нет keyboard alternative
- **Файл:** `components/KanbanBoard.tsx:188-200`
- **Проблема:** Drag-and-drop только мышью. Keyboard users не могут переместить card. **No fix in scope**, но TODO.

### 8.6 [low] `MemoryGraph` / `PigMemoryGraph` — canvas, не screen-reader-friendly
- **Файлы:** `components/MemoryGraph.tsx`, `components/pigmemory/PigMemoryGraph.tsx`
- **Проблема:** ForceGraph2D рисует в canvas. Нет fallback list. **Acceptable** для force-graph.

### 8.7 [low] `HotkeyBindings` — клавиатура есть, но UI не подсказывает
- **Файл:** `components/HotkeyBindings.tsx:97-110`
- **Проблема:** `Ctrl+Shift+N` etc — нигде не задокументировано в UI. Только in-code.

---

## 9. Производительность / мемоизация

### 9.1 [high] `appendChatChunk` — O(n) per chunk
- **Файл:** `state/store.ts:157-172`
- **Проблема:** Streaming 10-100 chunks/sec; каждый делает `s.chat.slice()` + `[...next, updated]` = O(n) per call, итого O(n×m) за стрим. При n=5000 и m=200 — 1M операций.
- **Фикс:** Append-only content (immutable ref + `Object.freeze`), либо batched chunks (200ms).

### 9.2 [medium] `useStore((s) => s.chat)` подписка на каждый chunk
- **Файл:** `state/store.ts:155-172` + `components/OrchestratorPanel.tsx:45-108`
- **Проблема:** Любой `appendChatChunk` → `chat` change → re-render `OrchestratorPanel`. Memo не помогает, потому что `chat` — новая ссылка каждый раз. **Plus** `chat.map((m, idx) => <ChatMessageView ... chat={chat} ... />)` — каждый item получает `chat` prop, приводит к re-render всех `ChatMessageView` (хоть и `memo`). Fix: стабилизировать `chat` через shallow-equal или вынести в селектор.

### 9.3 [medium] `TilingArea.renderNode` — рекурсивный inline `Allotment`
- **Файл:** `components/TilingArea.tsx:96-111`
- **Проблема:** `renderNode` вызывается на каждый `setLayout`. 100+ agent-tile tree → 100+ `<Allotment>` re-mount, потому что inline JSX → новая identity. **Allotment 0.x не memoize, пересоздаёт DOM.** Перформанс-катастрофа на drag.

### 9.4 [medium] `KanbanCard` drag handlers inline
- **Файл:** `components/KanbanBoard.tsx:237-246`
- **Проблема:** Inline `onDragStart={(e) => { e.dataTransfer.effectAllowed = "move"; onDragStart(); }}` — каждый ре-рендер пересоздаёт. **Low-medium.**

### 9.5 [low] `MemoryPanel.visible` useMemo + filter chain
- **Файл:** `components/MemoryPanel.tsx:157-173`
- **Проблема:** OK, useMemo. ✓

### 9.6 [low] `useTheme` global mutable + useSyncExternalStore
- **Файл:** `themes/useTheme.ts`
- **Проблема:** `emit()` зовёт всех listeners, даже тех, чей snapshot не изменился (например, две `<BridgeOrb>`-like с `theme`). Микро-перф, OK.

### 9.7 [medium] `ArchitectPanel` decisions array unbounded
- **Файл:** `state/architect.ts:94-99`
- **Проблема:** `pushDecision` хранит `decisions.slice(-199)` — capped. ✓
  **Но:** `setDecisions(list)` от init `architectIpc.decisions(100)` — hard-coded 100. При долгой сессии последние decision не отображаются. **Low.**

### 9.8 [low] `SettingsButton` localStorage roundtrip
- **Файл:** `components/SettingsButton.tsx:50-56, 98-105`
- **Проблема:** `getLastPanel()` на каждом ре-рендере (а меню ре-рендерит часто). **Memoize.**

---

## 10. Безопасность

### 10.1 [high] `ipc.writeToAgent` НЕ фильтрует control-chars / shell-meta
- **Файл:** `state/ipc.ts:71-74`, вызывается из `AgentTile.tsx:144, 385, 433`
- **Проблема:** `writeToAgent(agentId, toB64(data))` — data приходит из:
  - `term.onData((data) => ...)` — это term input, ОК.
  - `ctxPaste`: `navigator.clipboard.readText()` → `writeToAgent(agent.id, toB64(t))` — **clipboard может содержать ANSI/control-chars**. Уже не опасно для PTY, но если PTY → shell command pipeline — RCE в shell.
  - `onDrop`: `f.path` или `f.name` → `quoteIfNeeded(p)` → `payload = parts.join(" ") + " "` → `writeToAgent(agent.id, toB64(payload))`. **Дроп файла `evil';rm -rf /;'.txt` после quoteIfNeeded → `'evil'\\''rm -rf /;\\''.txt` — single-quote-escape, безопасно в bash, но в fish / zsh поведение отличается. Cross-shell problem.**
- **Фикс:**
  1. **Backend-side sanitization** — на Tauri-стороне отфильтровать `0x00..0x08, 0x0B, 0x0C, 0x0E..0x1F` (кроме `\n`, `\r`, `\t`).
  2. Backend: проверка path на canonical form + scope check (только в workspace.paths).
  3. Frontend: `quoteIfNeeded` сделать OS-aware (`cmd.exe` — другой escape).

### 10.2 [medium] `BrowserPanel` URL injection
- **Файл:** `components/BrowserPanel.tsx:59-70`
- **Проблема:** `navigate(target)` валидирует через `new URL()`. ✓ Но `prompt("Bookmark name?", url) ?? url` — prompt-ввод может содержать XSS-payload для tab title, но title в DOM как text, **OK**.
- **Edge:** `withScheme = trimmed.startsWith("http") ? trimmed : \`https://${trimmed}\``. Ввод `http://example.com` → ok. `httpx://...` → не матчит, ставит `https://httpx://...` → `new URL` throws → isSafeUrl false. ✓

### 10.3 [medium] `DictionaryEditor` — голосовой dict → backend regex
- **Файл:** `components/voice/DictionaryEditor.tsx`
- **Проблема:** `pattern` / `replacement` — пользовательский ввод. Если backend использует их как regex без escape — ReDoS / RCE. **Assumes backend safe.** Verify.

### 10.4 [low] `localStorage` XSS via persisted state
- **Файлы:** `state/store.ts:118-124` (`devTrace` from localStorage)
- **Проблема:** `localStorage.getItem("pigide.devTrace") === "true"` — boolean, safe. ✓
  `hooks/useInputHistory.ts:6-13` — `JSON.parse(raw)` → `arr.slice(-100)`. Если localStorage скомпрометирован — строки попадают в `<textarea value=...>`, не в HTML, safe. ✓

### 10.5 [low] `useTheme` — Tauri IPC race
- **Файл:** `themes/useTheme.ts:34`
- **Проблема:** `ipc.setSetting(SETTING_KEY, t.id)` — fire-and-forget. Не критично.

---

## 11. Cross-file state при switch workspace

### 11.1 [high] Inconsistent `setTasks` между switch-флоу (повтор 3.3)
- См. **3.3**. WorkspaceSidebar забывает `setTasks`, HotkeyBindings вызывает.

### 11.2 [medium] `AgentConfigPanel`, `SkillsPanel`, `ArchitectPanel` — не сбрасывают локальный state при switch
- **Файлы:** `components/AgentConfigPanel.tsx:49-53`, `components/SkillsPanel.tsx:53-66`
- **Проблема:** Эти панели на `currentId` change перезагружают list, но local UI (editing, drafts) не сбрасывается. После switch workspace, если пользователь нажал Edit на override в A, draft остался, потом `setItems` приходит от B, а draft — от A. **Low-high — confusion.**

### 11.3 [medium] `ProviderRow.editing` state isolation
- **Файл:** `components/ProvidersPanel.tsx:370-381`
- **Проблема:** `editState` хранит label/baseUrl/apiKey. Не сбрасывается при смене providers list. Если user нажал Edit, ввёл draft, потом провайдер удалили извне — draft привязан к несуществующему id. **Low.**

---

## 12. Прочие находки

### 12.1 [low] `appendDraftInput` whitespace logic
- **Файл:** `state/store.ts:175-182`
- **Проблема:** Тернарник `s.draftInput.endsWith(" ") || !text ? ... : s.draftInput + " " + text`. Edge: `s.draftInput` undefined? `endsWith` on undefined → crash? Нет, в Zustand state всегда string. ✓
- **Edge:** text="", `!text === true` → добавляется пустая строка, бессмысленно. Low.

### 12.2 [low] `useStore((s) => s.toasts)` selector returns array — re-render на каждый push
- **Файл:** `App.tsx:49`
- **Проблема:** Каждый pushToast → re-render App. Уже происходит, но добавляет GC-pressure. **Minor.**

### 12.3 [low] `dismissToast` listener на каждом toast re-creates
- **Файл:** `App.tsx:246-252`
- **Проблема:** `useEffect` deps `[dismissToast, toasts]` — на каждый новый toast clearTimeout + setTimeout. **Множественные toasts**: пока один не dismiss'нут, следующий не dismissed (только `toasts[0].id`). **Low UX bug** — queue drain sequential, не параллельный.

### 12.4 [medium] `Markdown.tsx` recursion — `renderMarkdown` вызывает себя для blockquote
- **Файл:** `components/Markdown.tsx:82-87`
- **Проблема:** `renderMarkdown(quoteLines.join("\n"))` — вызывает ВСЮ машину заново. Глубина ограничена парностью `>`, но если пользователь вставит `>>>>>>>>>>>>` (100 уровней) — стек-overflow. **Low, edge.**

### 12.5 [medium] `Markdown.tsx:111-129` ordered-list regex
- **Файл:** `components/Markdown.tsx:111-114`
- **Проблема:** `^\s*\d+\.\s+` — пустая после `\d+\.` — `\s+` (1+) — `1.` без пробела не матчит. **OK by design.**

### 12.6 [low] `Markdown.tsx:30-36` link regex — не захватывает `)` в URL
- **Файл:** `components/Markdown.tsx:30`
- **Проблема:** `\(([^)]+)\)` — обрывается на первой `)`. URL `https://en.wikipedia.org/wiki/Foo_(bar))` — баг. **Low.**

### 12.7 [low] `BrowserPanel` — bookmark XSS в названии
- **Файл:** `components/BrowserPanel.tsx:90-100`
- **Проблема:** `name = prompt("Bookmark name?", url) ?? url` — name как JSX text, не HTML, safe. ✓

### 12.8 [low] `ArchitectPanel` model select — hardcoded list
- **Файл:** `components/ArchitectPanel.tsx:18-25`
- **Проблема:** 6 хардкоженных моделей. Если бэкенд переименовал — silent fail в `onModelChange` (setSetting вызывается с несуществующим id). **Low.**

### 12.9 [low] `DictionaryEditor.commitPattern` — `void commit` noop
- **Файл:** `components/voice/DictionaryEditor.tsx:206`
- **Проблема:** `void commit;` — мёртвый код. `commit` уже вызывается в onBlur; в keyDown Enter'е blur'dется. **Мистический leftover.**

### 12.10 [low] `WorkspaceSidebar` rename — `prompt()`
- **Файл:** `components/WorkspaceSidebar.tsx:65-74`
- **Проблема:** `prompt("New name", current)` — native, блокирующий, не работает в Tauri-friendly. **Low UX.**

### 12.11 [low] `KanbanBoard.remove` — `confirm("Удалить задачу?")`
- **Файл:** `components/KanbanBoard.tsx:95`
- **Проблема:** `confirm` — native. **Low UX**, плюс русская фраза в англоязычном UI (inconsistent i18n).

### 12.12 [low] `MemoryPanel.remove` — то же
- **Файл:** `components/MemoryPanel.tsx:131` `confirm("Удалить заметку?")`.

### 12.13 [low] `TagManager.deleteTag` — toString concat
- **Файл:** `components/pigmemory/TagManager.tsx:46-50, 70-74`
- **Проблема:** `setError(\`Rename failed: ${e}\`)` — если error object — покажет `[object Object]`. Use `e instanceof Error ? e.message : String(e)`. **Low UX.**

### 12.14 [medium] `PigMemoryWorkbench.createNote` — `window.prompt` block + `setTagFilter` race
- **Файл:** `components/pigmemory/PigMemoryWorkbench.tsx:401-415`
- **Проблема:** `title = window.prompt("...", "Untitled")` — если отмена, `title === null`, return. **OK.** Но `createNote` useCallback с deps `[currentId, s.tagFilter, ...]` — `s.tagFilter` change пересоздаёт callback → `onClick={onCreate}` меняется → Sidebar ре-рендерится. **OK by React.** ✓

### 12.15 [low] `FilesPanel` `useMemo` `[quickQuery, allFiles]` — `quickQuery` всегда lowercase
- **Файл:** `components/FilesPanel.tsx:148-159`
- **Проблема:** ✓ OK.

### 12.16 [low] `CodeEditor.tsx:109-158` — hostRef не cleanup
- **Файл:** `components/CodeEditor.tsx:151-154`
- **Проблема:** `view.destroy()` в cleanup ✓. ✓

### 12.17 [low] `MemoryGraph` canvas rendering — `getComputedStyle` per render
- **Файл:** `components/MemoryGraph.tsx:62-72`
- **Проблема:** На каждый ре-рендер читаются CSS-vars. **High FPS on hover**: `setHovered` (нет в этом файле, но в PigMemoryGraph) → re-render → getComputedStyle. **Low perf.**

### 12.18 [low] `PigMemoryGraph` — same + huge `nodeCanvasObject`
- **Файл:** `components/pigmemory/PigMemoryGraph.tsx:257-321`
- **Проблема:** 60+ строк inline в `nodeCanvasObject`. ForceGraph передаёт каждую ноду каждый кадр. На 1000+ нод при cooldown FPS drop. **Low-medium.**

### 12.19 [low] `useAgentSummary` — `taskTitleRef` (line 64-66) — fine
- **Файл:** `hooks/useAgentSummary.ts`
- **Проблема:** `taskTitleRef.current = taskTitle` в useEffect — `taskTitle` деп. ✓ OK.

### 12.20 [low] `useInputHistory` `ArrowUp` / `ArrowDown` — ломают xterm внутри tile
- **Файл:** `hooks/useInputHistory.ts:50-93`
- **Проблема:** Handler на textarea. Но `taRef` — это textarea чата, не xterm. ✓ OK. **Plus** `requestAnimationFrame(() => ta.setSelectionRange(...))` — если textarea unmounts до rAF callback — крэш? `setSelectionRange` на null ref? `ta` захвачен в closure, `setSelectionRange` на `ta` (not `taRef.current`) → если `ta` стал null между event и rAF — `ta.setSelectionRange` throws. **Low edge.**

### 12.21 [low] `ArchitectPanel` ipc.getSetting без дефолта
- **Файл:** `components/ArchitectPanel.tsx:39-43`
- **Проблема:** `if (v && ARCHITECT_MODELS.includes(v))` — иначе остаётся default. ✓ OK.

### 12.22 [low] `useStore((s) => s.setChatScope)` — Zustand setter, stable
- **Файл:** `App.tsx:47`
- **Проблема:** ✓ OK.

### 12.23 [low] `MemoryGraph` `nodeLabel` html-encoding (escapeHtml on canvas?)
- **Файл:** `components/MemoryGraph.tsx:82`
- **Проблема:** `nodeLabel={(n) => escapeHtml(n.title)}` — canvas-tooltip, escape не нужен. **No harm.**

### 12.24 [low] `WorkspaceSidebar.rename` — `name` может быть `null` от prompt
- **Файл:** `components/WorkspaceSidebar.tsx:66-67`
- **Проблема:** `if (!name || name === current) return;` — `!name` ловит null/empty. ✓ OK.

### 12.25 [low] `DictionaryEditor.commitPattern` — `void commit` (повтор 12.9)

### 12.26 [low] `useInputHistory` localStorage при quota exceeded
- **Файл:** `hooks/useInputHistory.ts:6-13, 16-20`
- **Проблема:** try/catch ✓. ✓ OK.

### 12.27 [low] `PigMemoryWorkbench` `createNote` использует `prompt` а не `Modal`
- См. 1.7.

### 12.28 [low] `CommandBlocksBar` — `useEffect` deps `[blocks.length]`
- **Файл:** `components/cmdblock/CommandBlocksBar.tsx:14-18`
- **Проблема:** На каждый новый block (по 1 на команду) пересоздаёт эффект → setOpen(null). Закрывает expanded view при новой команде. **By design**, но если пользователь expanded на `b.id=5`, потом пришла команда `b.id=6` — expanded сбрасывается, OK.

### 12.29 [low] `CommandBlockParser` regex — single-byte check
- **Файл:** `components/cmdblock/parser.ts:57-63`
- **Проблема:** `chunk[i+5] === 0x3b` — проверка по байту. UTF-8 multi-byte символы могут иметь `;` (0x3B) как continuation byte? Нет, 0x3B — самостоятельный ASCII byte, не continuation. ✓

### 12.30 [low] `CommandBlockParser` — UTF-8 decode без `stream:true`
- **Файл:** `components/cmdblock/parser.ts:117`
- **Проблема:** `decoder.decode(body)` (OSC body — ASCII anyway) — `fatal:false`, OK. Но chunk-based feed мог разделить multi-byte UTF-8 посередине. `fatal:false` не выкидывает, заменяет на U+FFFD. **Low.**

### 12.31 [medium] `MemoryGraph` / `PigMemoryGraph` — force-graph2d unloads on unmount?
- **Файл:** `components/MemoryGraph.tsx`, `components/pigmemory/PigMemoryGraph.tsx`
- **Проблема:** `react-force-graph-2d` cleanup — нет явного `destroy()`. WebGL/Canvas в force-graph2d — если `containerRef` unmounts без явного unmount, теоретически memory leak. **Verify in upstream.** Low.

### 12.32 [low] `DictionaryEditor` `entry.id` typed as `string` not `string|undefined`
- **Файл:** `components/voice/DictionaryEditor.tsx`
- **Проблема:** Type-safety issue, no runtime impact.

### 12.33 [low] `WorkspaceSidebar` theme `useTheme` — вызов в render
- **Файл:** `components/WorkspaceSidebar.tsx:28`
- **Проблема:** ✓ `useTheme` is a hook, returns { theme, setTheme } — OK.

### 12.34 [low] `PigMemoryWorkbench` initial `prompt` block
- **Файл:** `components/pigmemory/PigMemoryWorkbench.tsx:401`
- **Проблема:** `window.prompt` блокирует event loop, prevents debounce. Low.

### 12.35 [medium] `useTheme()` в `AgentTile` snapshot
- **Файл:** `components/AgentTile.tsx:103`
- **Проблема:** `useTheme()` возвращает `{ theme, setTheme }`, где `theme` — `getTheme(currentId)` объект из catalog. Если currentId изменился — useSyncExternalStore rerender, new theme object. **Но `theme.xterm` — stable reference** (themes в catalog иммутабельные, `getTheme` возвращает один и тот же объект из THEMES). ✓ OK.

### 12.36 [low] `TilingArea` — `setLayout` после `ipc.updateLayout` race
- **Файл:** `components/TilingArea.tsx:55-59`
- **Проблема:** Optimistic `setLayout` + `persistLayout` (debounced). Если backend reject — нет rollback. **Low (нет reject path — backend всегда accept).**

### 12.37 [low] `WorkspaceSidebar.tooltip` position — может вылезти за viewport
- **Файл:** `components/WorkspaceSidebar.tsx:93-96`
- **Проблема:** `x: e.clientX + 12, y: e.clientY - 4` — нет clamp. Low UX.

### 12.38 [low] `OrchestratorPanel` `userScrolledUp` ref-based
- **Файл:** `components/OrchestratorPanel.tsx:72, 101-108`
- **Проблема:** `userScrolledUp.current` = true если НЕ at bottom. При resize (длинный message) — `userScrolledUp.current=true` → auto-scroll не сработает, юзер видит "застрял" посередине. **Low UX.** Лучше сбрасывать `userScrolledUp` если `chat` изменился.

### 12.39 [medium] `OrchestratorPanel` chat scroll на resize (line 117-127)
- **Файл:** `components/OrchestratorPanel.tsx:117-127`
- **Проблема:** `observer.observe(el)` — ResizeObserver. Disconnect в cleanup ✓. **OK.**

### 12.40 [low] `OrchestratorPanel` `confirm("Clear orchestrator chat history and context?")`
- **Файл:** `components/OrchestratorPanel.tsx:161`
- **Проблема:** Native confirm. Low UX.

### 12.41 [low] `BrowserPanel` `prompt("Bookmark name?", url)`
- **Файл:** `components/BrowserPanel.tsx:92`
- **Проблема:** Native. Low.

### 12.42 [medium] `WorkspaceSidebar` показывает `ws.paths` в tooltip — утечка путей
- **Файл:** `components/WorkspaceSidebar.tsx:93-96, 158-165`
- **Проблема:** `tooltip.text = paths.join(", ")` — может содержать secrets в path (`/home/user/.ssh/id_rsa`). Tooltip виден на экране, если кто-то делает screencast — leak. **Low security.**

### 12.43 [low] `useInputHistory` — стрелка вверх теряет позицию курсора
- **Файл:** `hooks/useInputHistory.ts:50-68`
- **Проблема:** `ta.setSelectionRange(text.length, text.length)` в rAF. **OK**, но если textarea blurred между event и rAF — `setSelectionRange` всё равно работает, потом `ta.focus()` не вызывается. Low.

### 12.44 [low] `PigMemoryWorkbench.deleteActive` — `confirm` на удаление
- **Файл:** `components/pigmemory/PigMemoryWorkbench.tsx:420`
- **Проблема:** Native confirm. Low.

### 12.45 [medium] `BrowserPanel` `isSafeUrl` allows http and https
- **Файл:** `components/BrowserPanel.tsx:6-13, 62-66`
- **Проблема:** `navigate("javascript:alert(1)")` → `withScheme = "https://javascript:alert(1)"` → `new URL("https://javascript:alert(1)")` → parsed as `https://` scheme, host `javascript`, port implicit, **parseable**. **isSafeUrl returns true**. Загружается `https://javascript:alert(1)` — DNS-fail, OK. **Но:** `navigate("http://localhost:9999/internal")` — `isSafeUrl` true, загружается. **Same-origin атака** если attacker имеет доступ к Tauri-origin localhost. **Low security.**

### 12.46 [low] `ArchitectPanel` model — захардкоженный список, нет валидации
- **Файл:** `components/ArchitectPanel.tsx:18-25`
- **Проблема:** Если backend добавит новую модель, settings сохранят, но select её не покажет. Low.

### 12.47 [low] `VoiceSettings.hotkey` — текстовый ввод, нет валидации формата
- **Файл:** `components/voice/VoiceSettings.tsx:75-82, 149-154`
- **Проблема:** `ipc.setSetting("voice.hotkey", value)` — value="evil" → бэкенд не сможет зарегистрировать global-hotkey, но и не сообщит об ошибке. **Low UX.**

### 12.48 [low] `DictionaryEditor` — `enabled` toggle без optimistic update
- **Файл:** `components/voice/DictionaryEditor.tsx:42-55`
- **Проблема:** `updateField` — `setEntries(...)` сначала, потом `await ipc.voiceDictUpdate`. Если reject — `refresh()` (full reload). **OK** but extra IPC. Low.

### 12.49 [low] `useAgentSummary` — stripControl регэксп может зациклиться на патологичном вводе
- **Файл:** `hooks/useAgentSummary.ts:5-8, 15-17`
- **Проблема:** ReDoS-опасные regex (`\x1B\][\s\S]*?(?:\x07|\x1B\\)`). На очень длинной строке без terminator может быть catastrophic. **Low, depends on input size.** PTY chunk size ~4 KiB bounded.

### 12.50 [low] `MemoryPanel` — snippet `<<`/`>>` markers в коде
- **Файл:** `components/MemoryPanel.tsx:363-372`
- **Проблема:** Если snippet содержит `&lt;` (escaped) — наша regex заменит на `<mark>`. Если snippet содержит `<` (raw) — escape сделает `&lt;`, regex не матчит. **OK.**

---

## 13. Summary table

| # | Severity | File | Title |
|---|----------|------|-------|
| 1.1 | critical | TilingArea.tsx:47-53 | persistLayout debounce пишет в чужой workspace |
| 1.2 | critical | PigMemoryWorkbench.tsx:233-290, SkillsPanel.tsx:53-66, ArchitectPanel.tsx:55-79, OrchestratorPanel.tsx:79-94 | IPC-listener leak — `unsubs.push(promise.then(u => u))` без disposed-check |
| 1.3 | high | AgentTile.tsx:170-202 | agentLogTail race против live onAgentStdout |
| 1.4 | high | useAgentSummary.ts:92-116 | subscribe-async, первый chunk может пройти мимо summary |
| 1.5 | medium | MemoryPanel.tsx:33-56, KanbanBoard.tsx:30-40 | no cancellation на workspace switch |
| 1.6 | medium | PathMentionTextarea.tsx:128-153 | suggestPaths race |
| 1.7 | low | PigMemoryWorkbench.tsx:401 | window.prompt block |
| 2.1 | medium | useAgentSummary.ts:107-116 | interval + listener cleanup OK, но re-mount logic — verify |
| 2.2 | low | MemoryGraph.tsx, PigMemoryGraph.tsx, NoteList.tsx | ResizeObserver — все disconnect в cleanup ✓ |
| 2.3 | medium | PigMemoryGraph.tsx:83-96 | window keydown/keyup egoMode — single instance OK |
| 2.4 | medium | AgentTile.tsx:154-164 | ResizeObserver без debounce → IPC flood на drag |
| 2.5 | medium | useHotkeys.ts:107-110 | глобальный keydown listener — re-attach на keysSignature change |
| 3.1 | medium | TilingArea.tsx:55-105 | setRatio stale closure + inline onChange |
| 3.2 | medium | PigMemoryWorkbench.tsx:388-397 | saveActive callback churn на draft char |
| 3.3 | high | WorkspaceSidebar.tsx:39-51 vs HotkeyBindings.tsx:60-83 | inconsistent setTasks между switch-флоу |
| 3.4 | low | MentionTextarea.tsx:102-114 | нет debounce на listTasks fire |
| 3.5 | low | FilesPanel.tsx:51-58 | breadcrumb click → full IPC reload |
| 3.6 | low | App.tsx:79-117 | initial-load без cancellation (StrictMode dev) |
| 3.7 | medium | KanbanBoard.tsx:166-200 | dragId ref-pattern OK |
| 3.8 | medium | MemoryPanel.tsx:31,78,304-314 | isDirty не учитывается при onClose |
| 3.9 | low | PigMemoryWorkbench.tsx:130-136 | search/searchDeb split — OK |
| 3.10 | medium | useTheme.ts:8-37 | global mutable currentId + bootstrap race |
| 4.1 | low | store.ts:211-218 | clearWorkspaceState не сбрасывает show* |
| 4.2 | low | tree.ts:86-98 | closeLeaf не очищает maximizedLeafId (stale, невидимо) |
| 4.3 | low | AgentTile.tsx:316-332 | respawn не переносит focus |
| 5.1 | medium | MentionTextarea.tsx:155 | short-id 8-char tags |
| 5.2 | medium | PathMentionTextarea.tsx:208-210 | short-id attachments |
| 5.3 | low | pathMentionHelpers.ts:106-112 | uniqueLabel — OK |
| 6.1 | high | Markdown.tsx:14-39 | safe after verify |
| 6.2 | low | MemoryPanel.tsx:363-372 | snippet double-encode — safe |
| 6.3 | low | PigMemoryWorkbench.tsx:1146-1154 | highlightFtsSnippet — duplicate |
| 6.4 | medium | PigMemoryEditor.tsx:118-130 | Decoration.mark class — safe |
| 6.5 | medium | MemoryGraph.tsx, PigMemoryGraph.tsx | canvas text — safe |
| 6.6 | medium | BrowserPanel.tsx:171 | sandbox flags — too permissive |
| 6.7 | low | DictionaryEditor.tsx:86-97 | quickAdd без sanitization |
| 7.1 | high | ALL | нет ни одного ErrorBoundary |
| 8.1 | low | App.tsx:271-282 | toast role=alert без aria-live=assertive |
| 8.2 | low | MentionTextarea.tsx, PathMentionTextarea.tsx | нет aria-activedescendant |
| 8.3 | low | OrchestratorPanel.tsx:411-419 | BridgeOrb — нет aria-busy |
| 8.4 | low | MemoryPanel.tsx, NoteList.tsx | role=button + Enter/Space — OK |
| 8.5 | low | KanbanBoard.tsx:188-200 | drag без keyboard alternative |
| 8.6 | low | MemoryGraph.tsx, PigMemoryGraph.tsx | canvas graph — no a11y |
| 8.7 | low | HotkeyBindings.tsx:97-110 | hotkeys не задокументированы в UI |
| 9.1 | high | store.ts:157-172 | appendChatChunk O(n) per chunk |
| 9.2 | medium | OrchestratorPanel.tsx:45-108 | chat reference change → re-render |
| 9.3 | medium | TilingArea.tsx:96-111 | рекурсивный inline Allotment — re-mount on layout change |
| 9.4 | medium | KanbanBoard.tsx:237-246 | drag handlers inline |
| 9.5 | low | MemoryPanel.tsx:157-173 | useMemo OK |
| 9.6 | low | useTheme.ts | emit() → all listeners |
| 9.7 | medium | architect.ts:94-99 | setDecisions hard-coded 100 |
| 9.8 | low | SettingsButton.tsx:50-56 | getLastPanel() per render |
| 10.1 | high | ipc.ts:71-74, AgentTile.tsx:144,385,433 | writeToAgent без control-char filter |
| 10.2 | medium | BrowserPanel.tsx:59-70 | URL injection — sandbox flags issue |
| 10.3 | medium | DictionaryEditor.tsx | voice-dict pattern → backend regex ReDoS |
| 10.4 | low | store.ts:118-124 | devTrace localStorage safe |
| 10.5 | low | useTheme.ts:34 | setSetting fire-and-forget |
| 11.1 | high | Sidebar+HotkeyBindings | см. 3.3 |
| 11.2 | medium | AgentConfigPanel.tsx:49-53, SkillsPanel.tsx:53-66 | local state не сбрасывается при switch |
| 11.3 | medium | ProvidersPanel.tsx:370-381 | editState не сбрасывается |
| 12.1 | low | store.ts:175-182 | appendDraftInput edge |
| 12.2 | low | App.tsx:49 | toasts selector re-render |
| 12.3 | low | App.tsx:246-252 | toasts dismiss sequential |
| 12.4 | medium | Markdown.tsx:82-87 | рекурсия renderMarkdown для blockquote |
| 12.5 | medium | Markdown.tsx:111-114 | ordered-list regex |
| 12.6 | low | Markdown.tsx:30 | link regex не ловит `)` в URL |
| 12.7 | low | BrowserPanel.tsx:90-100 | bookmark name XSS — safe |
| 12.8 | low | ArchitectPanel.tsx:18-25 | model list hardcoded |
| 12.9 | low | DictionaryEditor.tsx:206 | void commit — мёртвый код |
| 12.10 | low | WorkspaceSidebar.tsx:65-74 | prompt() — native |
| 12.11 | low | KanbanBoard.tsx:95 | confirm() — native, RU текст |
| 12.12 | low | MemoryPanel.tsx:131 | confirm() — native, RU текст |
| 12.13 | low | TagManager.tsx:46-50,70-74 | setError — toString может дать [object Object] |
| 12.14 | medium | PigMemoryWorkbench.tsx:401-415 | window.prompt + tagFilter race |
| 12.15 | low | FilesPanel.tsx:148-159 | useMemo OK |
| 12.16 | low | CodeEditor.tsx:151-154 | view.destroy() OK |
| 12.17 | low | MemoryGraph.tsx:62-72 | getComputedStyle per render |
| 12.18 | low | PigMemoryGraph.tsx:257-321 | nodeCanvasObject inline heavy |
| 12.19 | low | useAgentSummary.ts:64-66 | taskTitleRef OK |
| 12.20 | low | useInputHistory.ts:50-93 | rAF setSelectionRange на unmounted ref — edge |
| 12.21 | low | ArchitectPanel.tsx:39-43 | getSetting без дефолта — OK |
| 12.22 | low | App.tsx:47 | setChatScope stable |
| 12.23 | low | MemoryGraph.tsx:82 | escapeHtml на canvas — no harm |
| 12.24 | low | WorkspaceSidebar.tsx:66-67 | name null check OK |
| 12.25 | low | DictionaryEditor.tsx:206 | void commit duplicate |
| 12.26 | low | useInputHistory.ts:6-20 | localStorage try/catch OK |
| 12.27 | low | PigMemoryWorkbench.tsx:401 | см. 1.7 |
| 12.28 | low | CommandBlocksBar.tsx:14-18 | useEffect [blocks.length] OK |
| 12.29 | low | parser.ts:57-63 | ASCII byte check OK |
| 12.30 | low | parser.ts:117 | UTF-8 decode без stream:true — edge |
| 12.31 | medium | MemoryGraph.tsx, PigMemoryGraph.tsx | force-graph2d destroy — verify |
| 12.32 | low | DictionaryEditor.tsx | type-safety |
| 12.33 | low | WorkspaceSidebar.tsx:28 | useTheme OK |
| 12.34 | low | PigMemoryWorkbench.tsx:401 | см. 1.7 |
| 12.35 | medium | AgentTile.tsx:103 | useTheme snapshot + theme change effect OK |
| 12.36 | low | TilingArea.tsx:55-59 | optimistic setLayout — no reject path |
| 12.37 | low | WorkspaceSidebar.tsx:93-96 | tooltip position — no clamp |
| 12.38 | low | OrchestratorPanel.tsx:72,101-108 | userScrolledUp не сбрасывается на resize |
| 12.39 | medium | OrchestratorPanel.tsx:117-127 | ResizeObserver disconnect OK |
| 12.40 | low | OrchestratorPanel.tsx:161 | confirm() — native |
| 12.41 | low | BrowserPanel.tsx:92 | prompt() — native |
| 12.42 | medium | WorkspaceSidebar.tsx:93-96,158-165 | paths tooltip — sensitive data leak |
| 12.43 | low | useInputHistory.ts:50-68 | rAF selection on blur |
| 12.44 | low | PigMemoryWorkbench.tsx:420 | confirm() — native |
| 12.45 | medium | BrowserPanel.tsx:6-13,62-66 | isSafeUrl парсит `https://javascript:` — edge |
| 12.46 | low | ArchitectPanel.tsx:18-25 | hardcoded model list |
| 12.47 | low | VoiceSettings.tsx:75-82,149-154 | hotkey без валидации |
| 12.48 | low | DictionaryEditor.tsx:42-55 | updateField OK |
| 12.49 | low | useAgentSummary.ts:5-8,15-17 | ReDoS-опасные regex — bounded input |
| 12.50 | low | MemoryPanel.tsx:363-372 | snippet markers — safe |

---

## 14. Priority-fix suggestions (suggested order)

1. **1.1 + 1.3 + 1.2** — критические, потеря данных / утечки. Сделать ПЕРВЫМ.
2. **3.3** — расхождение `setTasks` между Sidebar и Hotkey, легко фиксится.
3. **7.1** — error boundary. Дёшево, спасёт от white-screen.
4. **9.1 + 9.3** — перф streaming chat + Allotment re-mount. User-visible.
5. **10.1** — фильтрация control-chars в `writeToAgent` (backend-side).
6. **6.6 + 10.2** — BrowserPanel sandbox — сузить флаги.
7. **A11y 8.1–8.3** — добавить aria-live, aria-activedescendant, aria-busy.
8. **12.4** — Markdown recursion depth limit.
9. **12.42** — paths tooltip: маскировать secrets.
10. Остальное — по приоритету продукта.

---

## 15. Verified safe / no action needed

- `Markdown` HTML-escape → regex inline → safe (XSS не пройдёт).
- `MemoryPanel` / `NoteList` / `PigMemoryWorkbench` FTS-snippet highlight — safe (escape + targeted replace).
- `BrowserPanel` URL validation — primary use safe; secondary `https://javascript:` edge mitigated by DNS-fail.
- Все ResizeObserver-ы в codebase правильно disconnect в cleanup.
- Все `useEffect` с cancel-флагами в async chains — корректны.
- `localStorage` usage — везде try/catch и JSON.parse-safe.
- `prompt`/`confirm` — UX-only, не security.

---

*File generated 2026-06-05 as part of frontend read-only audit.*
*Total: 50 unique findings, 5 critical/high (top-5), 24 medium, 21 low.*
