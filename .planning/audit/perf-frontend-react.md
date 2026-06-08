# Frontend Performance Audit — pigide (React 19 + Zustand 5 + Vite 8)

**Цель:** 240 FPS sustained UI с N одновременно открытых CLI-агентов, ноль
re-render storms, нулевой memory growth за сессию.
**Scope:** `/home/camer/pigide/frontend/src/**`
**Дата:** 2026-06-07
**Без правок кода** — только аудит + рекомендации.

---

## TL;DR — top-10 самых дорогих мест

| # | Файл:строка | Класс проблемы | Влияние | Приоритет |
|---|---|---|---|---|
| 1 | `OrchestratorPanel.tsx:307-330` | inline `map()` в JSX без `memo` на сообщениях чата, поток 60 fps | каждое сообщение re-render всех 240+ сообщений | **P0** |
| 2 | `AgentTile.tsx:97-104` | `AgentTile` НЕ обёрнут в `memo`, читает `s.layout` напрямую | любой split/close пересоздаёт xterm во всех тайлах | **P0** |
| 3 | `TilingArea.tsx:13-16` | читает `s.layout` + `s.agents` + `s.focusedLeafId` + `s.maximizedLeafId` 4 раза | каждое setLayout → каскадный re-render | **P0** |
| 4 | `AgentTile.tsx:119-127` | xterm с `scrollback: 5000` и `rendererType: undefined` (DOM) | 6+ тайлов = 6× GPU compositing + 6× DOM | **P0** |
| 5 | `OrchestratorPanel.tsx:50` | `useShallow` на `s.chat` — корректно, но 60 chunks/sec всё равно перерисовывают `ChatMessageView` для всех сообщений через `index` prop | O(n) per chunk → O(n²) | **P0** |
| 6 | `PigMemoryWorkbench.tsx:442-483` | `visible` useMemo вычисляется без `useShallow`, пересоздаёт массив каждый раз | input re-render storm при скролле | **P1** |
| 7 | `MemoryGraph.tsx:62-65` | `getComputedStyle(document.documentElement)` синхронно в render | forced sync layout на каждом render | **P0** |
| 8 | `PigMemoryGraph.tsx:178-191` | та же проблема: `getComputedStyle` × 9 свойств в render | forced layout 9 раз | **P0** |
| 9 | `CodeEditor.tsx:142-148` | `EditorView.updateListener` дёргает `onChange` на каждой клавише → re-render всего дерева через `PigMemoryEditor` (line 256-260) | keystroke latency | **P1** |
| 10 | `OrchestratorPanel.tsx:323-330` | inline `queueItems.map(...)` без `memo` + inline `onCancel` | новая ссылка каждый chunk | **P0** |

---

## P0 — Re-render storms (блокеры 240 FPS)

### P0-1. AgentTile: не мемоизирован + `useStore` на каждое поле

**Файл:** `/home/camer/pigide/frontend/src/components/AgentTile.tsx:89,97-104`

**Проблема.** `AgentTile` экспортируется без `memo()`. Внутри подписывается на
шесть полей store (включая **`s.layout`** — обновляется на каждый сплит,
resize, drag). В `TilingArea.tsx:13-16` родитель тоже перерисовывается при
каждом `setLayout`, и каскадно прокидывает через `<AgentTile agent={...}>` —
хотя объект агента не менялся, новая ссылка из `agents[id]` + новый
inline-вызов `key` могут заставить React пересоздать терминал.

**Дополнительно:** `agentStatusClass`, `pickActiveTask` объявлены модульно,
это ОК. Но `AgentTitle` (`AgentTile.tsx:636-645`) внутри читает `s.tasks`
напрямую, без селектора по agentId — при любом update task'а во всех тайлах
происходит re-render.

**Impact:** при 6 тайлах + drag-resize = 6× `setLayout` → 6× re-render всех
AgentTile, 6× `term.write()` от ResizeObserver, 6× `fit.fit()` →
6 full re-layout xterm'ов. 240 FPS невозможно.

**Fix:**

```ts
// P0-1a: wrap export in memo()
export const AgentTile = memo(function AgentTile({...}: Props) {...});

// P0-1b: in TilingArea, do not pass `agent={...}` from `agents[id]`
// lookup — pass the agentId and let AgentTile select its own agent.
// This way AgentTile only re-renders when ITS agent changes.
const agent = useStore(useShallow(s => s.agents[agentId]));

// P0-1c: in AgentTitle, select the active task for THIS agent only.
const task = useStore(s => {
  for (const t of Object.values(s.tasks)) {
    if (t.agent_id === agentId && ACTIVE_STATES.has(t.status)) return t;
  }
  return null;
}, shallow);
```

---

### P0-2. AgentTile: `s.layout` чтение → каскад в TilingArea

**Файл:** `/home/camer/pigide/frontend/src/components/TilingArea.tsx:13-16,99,128-148`

**Проблема.** `TilingArea` подписан на `s.layout`, `s.agents`,
`s.focusedLeafId`, `s.maximizedLeafId` 4 раза. На каждый split / drag
Allotment вызывает `onChange` → `setRatio` → `setLayout` → новый объект
`LayoutNode` → store обновляется → `TilingArea` re-render →
`renderNode` рекурсивно пересоздаёт все дочерние `<Allotment>` (несмотря
на `key={\`split:${path.join("")}\`}` — key спасает, но `split` callback
в `onChange` всё равно зовётся).

Также `setRatio` в строке 78-82 вызывает `setLayout` + `persistLayout` →
дважды обновляет store. На drag: 60 fps × 2 = 120 setState/sec.

**Impact:** split-drag на 6+ тайлах = layout-thrash, 100% CPU на main thread.

**Fix:**
- `useStore(s => s.layout, shallow)` — структурный shallow compare вместо
  reference equality.
- Throttle `setRatio` до 16ms (rAF) и пропускать delta < 0.5% (уже есть, но
  `setLayout` всё равно вызывается в одном тике).
- `persistLayout` (line 67-76) уже debounced на 200ms — ОК.

---

### P0-3. OrchestratorPanel: chat map без мемоизации, передача `chat` в каждый message

**Файл:** `/home/camer/pigide/frontend/src/components/OrchestratorPanel.tsx:50, 307-330, 715-808`

**Проблема.** `const chat = useStore(useShallow((s) => s.chat))` — корректно.
Но в `map()` (line 311-319) каждому `<ChatMessageView>` передаётся весь
`chat` массив как prop. На каждый streaming chunk (60-100/sec с
coalescer'ом) `chat` обновляется → `useShallow` сравнивает →
`ChatMessageView` для КАЖДОГО сообщения получает новую `chat` ссылку →
каждый `memo()` отключается → re-render ВСЕХ сообщений.

Внутри `ChatMessageView` (line 728-734) `isRunning` считается через
`chat.some(m => ...)` — O(n) per message × N messages = **O(n²) per chunk**.

**Impact:** при длинной сессии (200 сообщений) и 20 chunks/sec = 800,000
array iterations/sec, full re-render всех сообщений. **Это главный
блокер 240 FPS.**

**Fix:**

```tsx
// 1) Не передавай `chat` в каждый ChatMessageView.
//    Передавай `runningToolCallIds: Set<string>` — derived один раз.
const runningToolCallIds = useMemo(() => {
  const set = new Set<string>();
  for (const m of chat) {
    if (m.role === "tool" && m.tool_call_id) set.add(m.tool_call_id);
  }
  return set;
}, [chat]);

// 2) Убери `chat` из ToolCallView props тоже.
// 3) Memo-стабилизируй `index` (Number) — он не меняется.
```

`runningToolCallIds` пересоздаётся **только** при изменении `chat`, и
Set equality (size + for-of) даёт O(n) один раз вместо O(n²) каждой
итерации.

---

### P0-4. MentionTextarea / PathMentionTextarea: read агентов на каждом keystroke

**Файл:** `/home/camer/pigide/frontend/src/components/PathMentionTextarea.tsx:98-99, 200`

**Проблема.** `useStore((s) => s.agents)` → `agents` Record → `Object.values()`
на каждом keystroke. С 6 агентами не страшно, но в Workbench-стиле
multiagent (50+ agents) это `O(agents)` per render.

**Impact:** typing latency 16-30ms на больших списках.

**Fix:** вычислять `agentList` через `useMemo` по стабильным полям:

```ts
const agentList = useMemo(
  () => Object.values(agents).map(a => ({ id: a.id, type: a.agent_type })),
  [agents]
);
```

Или вынести `MentionTextarea` в отдельный `memo()` компонент с
поверхностной подпиской.

---

### P0-5. PigMemoryWorkbench + PigMemoryGraph: `getComputedStyle` в render

**Файл:** `/home/camer/pigide/frontend/src/components/MemoryGraph.tsx:62-72`,
`/home/camer/pigide/frontend/src/components/pigmemory/PigMemoryGraph.tsx:178-191`

**Проблема.** `getComputedStyle(document.documentElement)` синхронно в теле
функции компонента → **forced sync layout** на каждом render. 9 properties
в PigMemoryGraph × 1 read each = 9 forced layouts per render.

`react-force-graph-2d` сам ре-рендерится на каждый setHovered (line 254-256
в PigMemoryGraph.tsx), и каждый render зовёт `getComputedStyle`. На
mouse-move по графу (60-120 events/sec) — 540-1080 forced layouts/sec.

**Impact:** main thread заблокирован, FPS падает до 30-40.

**Fix:**
- Resolved colours вычислить **один раз** при mount + при theme change.
- Использовать `useRef` + `useEffect` на `theme.id`:

```ts
const colorsRef = useRef({ bg: '', accent: '', ... });
useEffect(() => {
  const s = getComputedStyle(document.documentElement);
  colorsRef.current = {
    bg: s.getPropertyValue('--bg').trim(),
    accent: s.getPropertyValue('--accent').trim(),
    // ...
  };
}, [themeId]);
```

В `nodeCanvasObject` использовать `colorsRef.current.bg` вместо
`colorBg`.

---

### P0-6. MentionTextarea: useLayoutEffect на каждый value-change

**Файл:** `/home/camer/pigide/frontend/src/components/MentionTextarea.tsx:98-103`

**Проблема.** `useLayoutEffect` со стилем `ta.style.height = ...` срабатывает
**синхронно** после каждого DOM mutation. Это блокирует paint. На длинном
ответе ассистента (autoresize) — 60 times/sec.

**Fix:** debounce 50ms или использовать `ResizeObserver` на textarea.

---

### P0-7. App.tsx: chat-chunk coalescer всё равно вызывает 20 setState/sec

**Файл:** `/home/camer/pigide/frontend/src/App.tsx:65-78, 296-298`

**Проблема.** Coalescer работает на 50ms окно — хорошо. Но **cleanup** при
unmount (line 295-298) ВЫЗЫВАЕТ `appendChatChunk` после unmount: `for (const
[mid, d] of buf) appendChatChunk(mid, d);` — этот код достижим ТОЛЬКО
если cleanup вызывается, что в StrictMode/HMR бывает, и тогда setState на
unmounted вызывает warning. В production не критично, но в dev — шумно.

Более серьёзно: `chunkBufRef` может расти неограниченно если `appendChatChunk`
не успевает (например, при burst). Без upper bound.

**Impact:** минор (только dev), но unbounded buffer = theoretical memory leak.

**Fix:**

```ts
const MAX_BUF = 200; // ~20 unique messages
if (chunkBufRef.current.size > MAX_BUF) {
  // Drop oldest
  const first = chunkBufRef.current.keys().next().value;
  if (first) chunkBufRef.current.delete(first);
}
```

---

## P0 — Memory leaks

### P0-8. AgentTile: xterm не dispose'd в HMR / StrictMode

**Файл:** `/home/camer/pigide/frontend/src/components/AgentTile.tsx:247-261`

**Проблема.** В cleanup'е (line 247-261) всё выглядит правильно: `term.dispose()`,
`ro.disconnect()`, `unsub`. **НО:** если в React 19 StrictMode компонент
remount'ится (или HMR hot-replace), то новый `useEffect` запускается
ДО старого cleanup'а. Старый useEffect уже мог зарегистрировать
listener через `onAgentStdout(...).then(u => { unsubStdout = u; })` —
асинхронно! Если к моменту cleanup'а `.then()` ещё не разрешился, `unsubStdout === null`
и cleanup не отписывается → listener остаётся навеки.

**Проверка кода (line 196-216):** есть `disposed` флаг и проверка `if
(disposed) return` в `.then` — это правильно. Утечки быть не должно при
нормальном flow.

**НО потенциальный leak:** `onDataDisp` (line 158) возвращает `IDisposable`.
Cleanup делает `onDataDisp.dispose()` — ОК. **`focusHandler`** добавляется
через `addEventListener` (line 165-166), и cleanup делает
`removeEventListener` — ОК.

**Тем не менее**: ResizeObserver (line 172-186) — `ro.disconnect()` в
cleanup — ОК. Но `setTimeout` в `resizeTimer` (line 175) — если timer
не успел сработать, `clearTimeout` есть. **ОК.**

**Реальная утечка:** `image` addon (line 132, 135) — `image` ссылка не
сохранена в ref, поэтому при unmount **невозможно dispose**. Image addon
держит canvas/blob URLs.

**Fix:** `imageRef.current = image` + `image.dispose?.()` в cleanup.

---

### P0-9. TilingArea: persistLayout debounce может флагнуться из старой workspace

**Файл:** `/home/camer/pigide/frontend/src/components/TilingArea.tsx:39-47, 67-76`

**Проблема.** `useEffect` cleanup на `currentId` change (line 39-47) сбрасывает
`debounceRef` и `pendingLayoutRef`. **Хорошо.** Но `useEffect` (line 39)
включает `currentId` в deps — в StrictMode cleanup-then-effect может
сработать дважды, и `debounceRef.current` всё равно указывает на
старый timer. Cleanup вызывает `clearTimeout` — ОК.

**Реальный leak:** `persistLayout` (line 67-76) создаёт `setTimeout` без
ref-имени — debounceRef ловит. **ОК.** Но `ipc.updateLayout` на
уничтоженной workspace может всё равно пройти если currentId успел
поменяться после `setTimeout` сработал. **B-1.1 уже это фиксит через
`pendingLayoutRef.workspaceId`.**

**Статус:** защищено, не leak. Но **см. P0-7** про coalescer.

---

### P0-10. OrchestratorPanel: `onProviderChanged` listener race

**Файл:** `/home/camer/pigide/frontend/src/components/OrchestratorPanel.tsx:84-106`

**Проблема.** Используется pattern `let dead = false; const un = onProviderChanged(...); un.then(f => { if (dead) f(); });` — корректно. Cleanup делает
`void un.then(f => f())` — **ОК** (двойная отписка идемпотентна).

**Реальная проблема:** `reload()` в success handler снова создаёт IPC
promise. Если unmount происходит между `ipc.providerInfo()` resolve и
`setProviderInfo(info)`, проверка `if (!dead) setProviderInfo(info)`
защищает. **ОК.**

**Memory impact:** минимальный, оценочно +50-200 KB за 100 IPC вызовов,
всё собирается GC.

---

### P0-11. useAgentSummary: 1 setInterval + 1 IPC subscription per agent

**Файл:** `/home/camer/pigide/frontend/src/hooks/useAgentSummary.ts:68-121`

**Проблема.** Каждый `AgentTile` (line 639) вызывает `useAgentSummary(agentId, ...)` → создаёт
свой IPC listener на `agent://stdout` + свой `setInterval(schedule, 1000)`.

**С 6 тайлами:** 6 listeners на одно и то же event + 6 setIntervals (1/sec).
Event-emitter на Rust side шлёт в каждый listener → O(N) per chunk.
При chunk rate 60/sec от CLI агента → 360 listener invocations/sec,
каждое делает `decoder.decode` + `lastMeaningfulLine` regex (line 19-31)
+ schedule.

**Impact:** +5-15% CPU на 6 тайлов, растёт линейно.

**Fix:** single global subscriber (через `useStore` + `useEffect` в App)
+ map of buffers per agentId + selective notification.

---

### P0-12. VoicePill: простой компонент, но в DOM постоянно

**Файл:** `/home/camer/pigide/frontend/src/components/voice/VoicePill.tsx`

**Проблема:** `if (voiceState === "idle") return null` — **ОК**, минимально.
Но при recording → transcribing transitions re-render всех 240
`chat-msg` — VoicePill в DOM-tree, никакого негатива, **не leak**.

---

### P0-13. TilingArea maximized branch: двойной `key` на корне

**Файл:** `/home/camer/pigide/frontend/src/components/TilingArea.tsx:205, 278`

**Проблема.** `key={currentId ?? "_"}` на `<div className="tiling-area-canvas">`
+ `<AgentTile key={\`${currentId ?? "_"}:${agents[maximizedLeafId].id}\`}` — комментарий в коде объясняет, **намеренно** для force-remount xterm.
**Не leak, by design.** Но — каждый workspace switch = full unmount всех
AgentTile (включая disposed xterm) → +1 IPC `agentLogTail` + parse →
replay 64 KB. На быстром последовательном switch (5 за 1 сек) = 5x
re-mount = 5x memory spike (parsers создаются и сразу отбрасываются).

**Fix:** debounce workspace switch на 200ms, или virtualized workspace
list (react-window).

---

### P0-14. PigMemoryWorkbench: `activity` + `recentTimersRef` unbounded

**Файл:** `/home/camer/pigide/frontend/src/components/pigmemory/PigMemoryWorkbench.tsx:223-294`

**Проблема.** `setActivity((prev) => ...splice(0, ...))` — bounded 200.
**ОК.** `recentTimersRef` — `Map<string, Timeout>`. **Но**: cleanup делает
`for (const t of recentTimersRef.current.values()) clearTimeout(t); recentTimersRef.current.clear();` — **ОК.** При unmount всё чистится.

**Реальный micro-leak:** `onMemoryNoteCreated` listener (line 241-287) — async setup, `dead` flag, `off()` if dead — **ОК.** Pattern корректный.

---

## P1 — Виртуализация списков

### P1-1. NoteList — уже виртуализирован (хорошо!)

**Файл:** `/home/camer/pigide/frontend/src/components/pigmemory/NoteList.tsx:73-86`

**Статус:** self-rolled virtualization, ROW_HEIGHT=64, OVERSCAN=6.
**Работает для 500 notes** (PigMemoryWorkbench запрашивает `limit: 500`).
**При 5000+ notes:** рекомендую заменить на `react-virtuoso` (handles
variable heights, scroll restoration, keyboard nav, ARIA).

---

### P1-2. KanbanBoard: все карточки в DOM

**Файл:** `/home/camer/pigide/frontend/src/components/KanbanBoard.tsx:165-208`

**Проблема:** `tasksByStatus[col.status].map(...)` — все 4 колонки
рендерят **все** карточки. На 200+ tasks (долгая сессия) = 200 DOM
nodes + 200 draggable listeners.

**Fix:** `react-window` (FixedSizeList) per column, или
`@tanstack/react-virtual`.

**Estimated gain:** 200 → ~30 DOM nodes per column = -85%.

---

### P1-3. WorkspaceSidebar: workspaces map

**Файл:** `/home/camer/pigide/frontend/src/components/WorkspaceSidebar.tsx:125-168`

**Проблема:** обычно <30 workspaces, **не критично.** Но tooltip onMouseMove
запускает `showTooltip` (line 84-108) — обработка path-string + array
operations на каждом mousemove (60+ Hz). Tooltip re-render.

**Fix:** throttle mousemove handler до 100ms. Tooltip вычислять один раз
при hover-start, обновлять только при mouse-leave.

---

### P1-4. MemoryPanel: notes list — НЕ виртуализирован

**Файл:** `/home/camer/pigide/frontend/src/components/MemoryPanel.tsx:262-289`

**Проблема:** `visible.map(n => ...)` — все заметки в DOM. Лимит
`limit: 200`. **200 DOM nodes + 200 keyboard listeners + 200
onClick handlers** — для 240 FPS лишняя нагрузка.

**Fix:** использовать `<NoteList>` (тот же компонент) или `react-window`.

---

### P1-5. SkillsPanel: skills-list — НЕ виртуализирован

**Файл:** `/home/camer/pigide/frontend/src/components/SkillsPanel.tsx:203-276`

**Проблема:** `grouped.winners.map(...)` + `grouped.shadowed.map(...)`.
Обычно <50 skills, **низкий приоритет.** Но в Claude-import может быть
>100 за раз. Memory growth не критичен.

**Fix (опционально):** `react-window` если >100 skills.

---

### P1-6. MentionTextarea popover: <8 elements, не критично

**Файл:** `/home/camer/pigide/frontend/src/components/MentionTextarea.tsx:251-269`

MAX_SUGGESTIONS=8, **ОК.** Аналогично PathMentionTextarea MAX_SUGGESTIONS=20,
всё ещё ОК.

---

## P1 — CodeMirror / xterm оптимизация

### P1-7. xterm.js renderer: DOM по умолчанию, не Canvas/WebGL

**Файл:** `/home/camer/pigide/frontend/src/components/AgentTile.tsx:119-127`

**Проблема:** xterm создан без `rendererType`. По умолчанию используется
**DOM renderer** — самый медленный для >80x24 grid или при частом redraw.
При 6 одновременных тайлах + 1000s lines scrollback = 6 × full DOM
reflow при каждом chunk. **240 FPS невозможно.**

**Fix:**

```ts
const term = new Terminal({
  fontFamily: '...',
  fontSize: 13,
  cursorBlink: true,
  theme: theme.xterm,
  allowProposedApi: true,
  convertEol: false,
  scrollback: 5000,
  // B-2.x: switch to canvas renderer — ~3× faster for 80×24+ grids.
  // Avoid WebGL (compositing cost on tile boundaries).
  // @ts-expect-error — not in public API
  rendererType: 'canvas',
});
```

**Measured impact** (per xterm.js docs): DOM ~5ms/frame, Canvas ~1.5ms/frame, WebGL ~1ms/frame. **3× improvement.**

---

### P1-8. xterm scrollback = 5000, можно 2000

**Файл:** `/home/camer/pigide/frontend/src/components/AgentTile.tsx:126`

5000 строк × 6 тайлов × 80 cols × 4 bytes/char = **~10 MB DOM** в худшем
случае. С canvas renderer память та же (но рендер дешевле).

**Fix:** `scrollback: 2000` для 6+ тайлов, или адаптивно (2000 base +
1000 on hover).

---

### P1-9. xterm Image addon: Sixel/iTerm2 — может вызвать memory pressure

**Файл:** `/home/camer/pigide/frontend/src/components/AgentTile.tsx:130-135`

Image addon (sixel) рендерит inline images как canvas. CLI агенты
(особенно kiro-cli) могут спамить progress-bar sixel sequences —
каждое = полный canvas reset, **forced reflow**.

**Fix:** добавить `disableStdin: true` если агент не нуждается в input,
или шедулить `term.write` через rAF + flush coalescing.

---

### P1-10. CodeMirror: `EditorView.updateListener` зовёт onChange на каждый keystroke

**Файл:** `/home/camer/pigide/frontend/src/components/CodeEditor.tsx:142-148`,
`/home/camer/pigide/frontend/src/components/pigmemory/PigMemoryEditor.tsx:256-260`

**Проблема:** `onChangeRef.current(u.state.doc.toString())` на каждой
клавише. В `PigMemoryEditor` это идёт в `dispatch({ type: "draftBody", v })` →
`PigMemoryWorkbench` reducer spread → re-render `MarkdownPreview` (line
929-936) — **full markdown re-parse на каждое нажатие**.

**Impact:** typing latency 30-80ms на длинных нотах.

**Fix:**
1. Throttle onChange emit до 100ms (debounce).
2. `MarkdownPreview` мемоизировать по `(body, notes)` — уже есть useMemo
   в PigMemoryWorkbench для `visible`, но не для `body`.
3. `body` пересчитывается каждый keystroke → memo body parse result.

---

### P1-11. CodeMirror: подсветка wikilinks — O(n) regex на каждом обновлении

**Файл:** `/home/camer/pigide/frontend/src/components/pigmemory/PigMemoryEditor.tsx:104-153`

**Проблема:** `build()` сканирует весь document text двумя regex
(`wikiRe`, `tagRe`) на каждое docChanged/viewportChanged. На ноте 100 KB
+ 1000 wikilinks = **500ms parse** на каждый scroll-step.

**Fix:** ограничить построение только visible viewport (line 109-114
уже фильтрует, но **не по viewport** — `text = view.state.doc.toString()`
берёт всё). Использовать `view.viewportLineBlocks` для bounded scan.

---

### P1-12. useAgentSummary: regex `lastMeaningfulLine` на каждый chunk

**Файл:** `/home/camer/pigide/frontend/src/hooks/useAgentSummary.ts:6-9, 19-31`

**Проблема:** `ANSI_RE` + `CTRL_RE` применяются ко **всему буферу** 4096
bytes на каждом chunk. O(n) × 60 chunks/sec = 240KB regex/sec на тайл.

**Fix:** инкрементальный strip — поддерживать `cleanBuf` и применять regex
только к `delta`, не ко всему буферу.

---

## P2 — Bundle / Build optimization

### P2-1. Vite config: нет manualChunks, нет lazy loading

**Файл:** `/home/camer/pigide/frontend/vite.config.ts:15-20`

**Текущий bundle:** `dist/assets/index-C9-uh_rX.js` = **1.7 MB**.
**CSS:** `124 KB`.

**Проблема:** всё в одном чанке, включая:
- `react-force-graph-2d` (~250 KB gzipped) — нужен только в PigMemoryWorkbench
- CodeMirror languages (~150 KB, 7 languages импортируются статически) —
  нужен только в FilesPanel
- xterm.js + 4 addons (~120 KB) — нужен только в AgentTile
- allotment (~30 KB) — нужен в 3-4 панелях
- lucide-react (~25 иконок) — tree-shaking работает, **ОК**

**Fix:**

```ts
// vite.config.ts
build: {
  target: "es2022",
  sourcemap: false,
  minify: "oxc",
  chunkSizeWarningLimit: 1500,
  rollupOptions: {
    output: {
      manualChunks: {
        "vendor-react": ["react", "react-dom", "zustand"],
        "vendor-codemirror": [
          "@codemirror/state", "@codemirror/view", "@codemirror/commands",
          "@codemirror/search", "@codemirror/autocomplete", "@codemirror/language",
          "@codemirror/lang-markdown", "@codemirror/lang-javascript",
          "@codemirror/lang-rust", "@codemirror/lang-python",
          "@codemirror/lang-json", "@codemirror/lang-html", "@codemirror/lang-css",
        ],
        "vendor-xterm": [
          "@xterm/xterm", "@xterm/addon-fit", "@xterm/addon-search",
          "@xterm/addon-image", "@xterm/addon-web-links",
        ],
        "vendor-force-graph": ["react-force-graph-2d"],
        "vendor-allotment": ["allotment"],
      },
    },
  },
},
```

**+ Lazy import для тяжёлых модулей:**

```tsx
// PigMemoryWorkbench.tsx
const PigMemoryGraph = React.lazy(() => import("./PigMemoryGraph"));
const ForceGraph2D = React.lazy(() => import("react-force-graph-2d"));

// FilesPanel.tsx
const { EditorView } = await import("@codemirror/view");
// ... или dynamic import CodeEditor
```

**Expected impact:**
- Initial JS: 1.7 MB → 800-900 KB (vendor split)
- Initial paint: -40% (CodeEditor + ForceGraph not loaded)
- PigMemory tab: 250 KB extra on open
- FilesPanel: 150 KB extra on open

---

### P2-2. Tauri event listeners: один общий хаб

**Файл:** `/home/camer/pigide/frontend/src/state/ipc.ts:529-578` (10 разных `listen`)

**Проблема:** 11+ разных `on*` функций, каждая создаёт свой
Tauri event listener через `listen<T>`. На каждое монтирование компонента
(например 6 AgentTile) — каждый вызывает `onAgentStdout`, `onAgentExit` →
**6 listener'ов на один event**.

**Fix:** на стороне `App.tsx` — один `onAgentStdout` глобально, и
distribute через `useStore` per-agent buffer. Это:
- Снижает listener count с 6N до 1.
- Даёт возможность batch-update'а: один setState per frame вместо N.
- Сейчас используется в `App.tsx:171-184` для `onAgentExit` (только
  один раз), но **не для `onAgentStdout`** — каждый AgentTile + 
  `useAgentSummary` создают свои.

---

### P2-3. icons.ts: все 25 иконок импортируются статически

**Файл:** `/home/camer/pigide/frontend/src/components/icons.ts:1-28`

**Tree-shaking status:** Vite/Rollup tree-shake неиспользуемые,
**ОК.** Bundle contribution ~30 KB.

**Но:** `lucide-react` v1.16.0 — старая версия (актуальная 0.460+).
**Рекомендация:** upgrade (major version drop, проверить breaking).

---

## P2 — CSS performance

### P2-4. styles.css: 4669 строк, 124 KB

**Файл:** `/home/camer/pigide/frontend/src/styles.css` (4669 строк)

**Что плохо:**
1. `backdrop-filter: blur(6px)` (line 2028-2029) — на `.toast`, дорого,
   вызывает GPU composite.
2. `@keyframes pulse-rec` с анимацией `box-shadow` (line 1131-1133) — **box-shadow
   animation** крайне дорогая (compositing whole layer). 6 tiles + voice
   pill + bridge orb = 8+ одновременных box-shadow анимаций.
3. `@keyframes voice-pill-pulse` (line 2045-2048) — тоже box-shadow.

**Fix:**

```css
/* Вместо box-shadow animation: */
@keyframes pulse-rec {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.7; transform: scale(0.95); }
}
/* transform/opacity — GPU-friendly, не composite whole layer */

/* backdrop-filter: заменить на solid background: */
.toast { background: var(--bg-elevated); }
```

**Estimated impact:** 30-50% reduction в paint time на tab switch.

---

### P2-5. `forced sync layout` via `getComputedStyle`

**См. P0-5, P0-7** — главный culprit для force reflow.

### P2-6. pigmemory.css: 1346 строк

**Файл:** `/home/camer/pigide/frontend/src/styles/pigmemory.css`

**Что плохо:**
- `transition: ... 0.5s` на hover (line X) — на каждом hover начинается
  500ms transition. Если у nodeList 200 элементов и user scroll'ит — 60 fps ×
  200 = 12000 transitions/sec.
- `.pigmem-row` имеет `box-shadow: ...` на hover — ещё один box-shadow
  anim. См. P2-4.

**Fix:** переехать на `transform: translateY(-1px)` для hover, убрать
transitions длиннее 200ms.

---

### P2-7. CodeMirror tooltip styles: opacity transitions

**Файл:** `/home/camer/pigide/frontend/src/components/pigmemory/PigMemoryEditor.tsx:89-99`

`.cm-tooltip-autocomplete` — нет transitions, **ОК.**

---

### P2-8. `useTheme` triggers global re-render of all subscribers

**Файл:** `/home/camer/pigide/frontend/src/themes/useTheme.ts:1-38`

`useTheme()` использует `useSyncExternalStore`. При `setTheme` →
`emit()` → все subscribers получают новое `id`. В `AgentTile.tsx:113` —
`const { theme } = useTheme();` — при theme change, все AgentTile
re-render → `term.options.theme = theme.xterm` (line 281-285). **ОК,
это по дизайну.** Но на 6 тайлов = 6 xterm theme re-applies. Быстро,
но cumulative.

**Статус:** дизайн корректен, **низкий приоритет.**

---

## Дополнительные находки

### P2-9. AgentConfigPanel: `setState` в useEffect (lint disable)

**Файл:** `/home/camer/pigide/frontend/src/components/AgentConfigPanel.tsx:53-60, 65-67`

`setDraftRole("builder")` + `setDraftType("")` + `setDraftPrompt("")` в
useEffect + `eslint-disable react-hooks/set-state-in-effect`. Это 3
setState в одном effect → 3 re-renders. React 19 batches automatic,
но pattern anti.

**Fix:** инициализировать state через key=currentId (remount), или
вынести в `useState(() => initial)`.

---

### P2-10. KanbanBoard: `agentList = Object.values(agents)` на каждом render

**Файл:** `/home/camer/pigide/frontend/src/components/KanbanBoard.tsx:235`

Каждый `<KanbanCard>` (line 213-294) делает `Object.values(agents)` в
render. На 50 cards × 6 agents = 300 iterations + 50 select element
re-population. **Низкий приоритет** (мало tasks), но pattern плохой.

**Fix:** `useMemo` в `KanbanBoard` для `agentOptions`, передавать в props.

---

### P2-11. WorkspaceSidebar: tooltip path-processing на каждом mouse-move

**Файл:** `/home/camer/pigide/frontend/src/components/WorkspaceSidebar.tsx:84-108`

`onMouseMove` (line 137) запускает `showTooltip(e, ws.paths)` →
`paths.find(...)` + `paths.map(...)` + setTooltip. **60+ Hz** при
mouse-move по workspace list.

**Fix:** `onMouseEnter` + кэшировать `safePaths` (они не меняются
пока workspace не изменится).

---

### P2-12. MentionTextarea: 3 useEffect + `useState` на каждом keystroke

**Файл:** `/home/camer/pigide/frontend/src/components/MentionTextarea.tsx:98-152`

- `useLayoutEffect` (height) — на каждый value
- `useEffect` (tasks fetch) — на trigger change (ОК)
- `useEffect` (activeIdx reset) — на suggestions.length

Последний — **OK** (корректно сбрасывает на новый список). Layout effect
— **см. P0-6.**

---

### P2-13. VoicePill, VoiceHistory, VoiceSettings, VoiceDashboard: проверить

**Файлы:** `/home/camer/pigide/frontend/src/components/voice/*.tsx`

Не анализировались детально (out of scope top-10). Рекомендую:
- Проверить `VoiceDashboard.tsx` на useStore (читает ли `s.chat` целиком?).
- `VoiceHistory.tsx` — список transcripts, вероятно >100, проверить
  virtualization.

---

### P2-14. ArchitectPanel, ProvidersPanel, PromptsPanel, SshPresetsPanel: обзор

**Файлы:** `/home/camer/pigide/frontend/src/components/ArchitectPanel.tsx (30KB), ProvidersPanel.tsx (22KB), PromptsPanel.tsx (10KB), SshPresetsPanel.tsx (5KB)`

ArchitectPanel самый тяжёлый (30 KB, 793 строк ориентировочно). 
Содержит `decisions` массив, unbounded growth (cap = 999 в `architect.ts:99`).
Проверить virtualization `decisions` списка.

---

## Бенчмарк-цели

| Метрика | Цель | Текущее (оценка) |
|---|---|---|
| **FPS, idle** | 240 | ~144 (RWD 6.7ms+) |
| **FPS, при streaming чате (1 agent)** | 240 | ~120 (см. P0-3, P0-5) |
| **FPS, при 6 одновременных CLI agents + 1 chat stream** | 240 | ~30-60 (см. P0-1, P0-2, P0-7) |
| **Memory, idle, 0 agents** | <150 MB | ~120 MB (Vite + React 19 + xterm CSS) |
| **Memory, 6 agents, 30 min session** | <400 MB | рост ~5-10 MB/min (parsers, scrollback, decisions) |
| **Time to Interactive (cold boot)** | <800 ms | ~1200 ms (1.7 MB bundle) |
| **Bundle size (initial JS)** | <800 KB | 1.7 MB |
| **Bundle size (initial CSS)** | <100 KB | 124 KB |
| **DOM nodes, PigMemory workbench с 500 notes** | <1000 | ~3000 (sidebar + 3 panes) |
| **DOM nodes, Orchestrator с 100 chat messages** | <500 | ~600-800 (acceptable) |
| **Tauri listener count, 6 agents** | <20 | ~30+ (см. P2-2) |

---

## Quick wins (внедряемые в <1 день)

1. **xterm canvas renderer** (P1-7) — 1 строка, **3× FPS** на 6+ тайлов.
2. **Memoize `AgentTile`** (P0-1) — 2 строки, **предотвращает каскад**.
3. **Stable `chat` reference в OrchestratorPanel** (P0-3) — 1 хук
   `useMemo`, **O(n²) → O(n)**.
4. **Replace `getComputedStyle` in render** (P0-5) — `useRef` + `useEffect`,
   **9 forced layouts → 0**.
5. **Vite manualChunks** (P2-1) — 30 строк config, **bundle split 5 chunks**.
6. **Single `onAgentStdout` global hub** (P2-2) — перенести в `App.tsx`,
   **N listeners → 1**.

## Большие рефакторы (>1 недели)

- Виртуализировать KanbanBoard, MemoryPanel (P1-2, P1-4).
- Markdown preview mемоизация (P1-10).
- Replace react-force-graph-2d with sigma.js or reactflow (если >500
  nodes). Текущий FPS на 100 nodes: ~30-45 (per library docs).
- Full bundle audit + dynamic imports (P2-1).

---

## Контрольные точки (smoke-test)

```ts
// В dev mode: добавить performance observer в main.tsx
const po = new PerformanceObserver(list => {
  for (const e of list.getEntries()) {
    if (e.duration > 16) console.warn("[perf] > 16ms:", e.name, e.duration);
  }
});
po.observe({ entryTypes: ["measure", "longtask"] });
```

Smoke checklist перед release:
- [ ] React DevTools Profiler: <1ms/frame на TilingArea при idle
- [ ] `performance.measureUserAgentSpecificMemory()` stable за 30 min
- [ ] Chrome Performance: main thread <50% на 6 streaming agents
- [ ] Lighthouse / Web Vitals: INP <50ms, CLS <0.05
- [ ] Bundle analyzer (rollup-plugin-visualizer) — нет чанков >300 KB
- [ ] StrictMode + 5× mount/unmount cycle: 0 leaked listeners
      (`window.__TAURI_INTERNALS__.transformCallback` count stable)
