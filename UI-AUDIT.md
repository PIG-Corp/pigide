# PigIDE — Аудит графического интерфейса (GUI / UX / UI / a11y)

> Дата: 2026-05-31
> Объём: `frontend/` (React + TypeScript + Tauri) — 39 компонентов (~10.5k строк TSX), 3 CSS-файла (~6k строк).
> Методология: 6-pillar визуальный аудит (Copywriting, Visuals, Color, Typography, Spacing, Experience Design) + a11y + UX-баги + live-проверка через Playwright (dev-сервер на :5173, скриншот + console).
> Каждая находка имеет `file:line` и пошаговый фикс. Приоритеты: **P0** (баг, ломает UX/функцию) · **P1** (важно) · **P2** (полировка).

---

## Оценка по 6 пилларам (1–4)

| Пиллар | Оценка | Комментарий |
|---|---|---|
| Copywriting | 3/4 | Тексты ясные, но смесь языков (RU-строки в EN-приложении), плейсхолдеры используются как лейблы. |
| Visuals | 3/4 | Цельная дизайн-система с токенами; портят хардкод-цвета в Bridge Orb + dead-state и сломанная анимация. |
| Color | 2/4 | Хардкод hex/rgba мимо токенов, несуществующий `--error`, `--fg-subtle` как основной вторичный текст → провал контраста WCAG AA в десятках мест. |
| Typography | 2/4 | 22 хардкода `font-family`, ~95 литералов `font-size` (49 раз `10px` вне шкалы), потеря line-height. |
| Spacing | 3/4 | Сетка 4px есть, но 60+ off-grid значений (5/7/9/13/15px) и непоследовательные паддинги одинаковых элементов. |
| Experience Design | 2/4 | Нет ErrorBoundary (любой throw → белый экран), мёртвые кнопки, нет `<form>`/Enter-submit, деструктив без подтверждения, клавиатурная недоступность ключевых зон. |
| **Итого** | **15/24** | Крепкая база дизайн-системы, подорванная контрастом, robustness и a11y. |

---

## P0 — Критичное (баги, ломают функцию/UX)

### P0-1. Нет ErrorBoundary → белый экран при любом throw в рендере
**Где:** весь `src/` (греп `ErrorBoundary|componentDidCatch|getDerivedStateFromError` → 0 совпадений). Подтверждено live: при недоступности Tauri-рантайма приложение рендерит пустой холст (App.tsx падает на подписках `onAgentExit` и т.п., строки 121+).
**Почему:** один неперехваченный throw в любом компоненте кладёт весь UI без сообщения.
**Фикс:**
1. Создай `src/components/ErrorBoundary.tsx` — классовый компонент с `getDerivedStateFromError` + `componentDidCatch(err, info)` (лог в консоль/IPC), в `render` при ошибке показывай fallback с кнопкой «Reload».
2. Оберни корень в `App.tsx`: `<ErrorBoundary><Allotment>…</Allotment></ErrorBoundary>`.
3. Подписки на события в `App.tsx` (`onAgentExit`, `onChatMessage`, …, строки ~121–205) заверни в `try { … } catch (e) { console.error(e); }`, чтобы провал `listen()` не ронял эффект.

### P0-2. `@keyframes pulse-rec` определён дважды — анимация записи сломана
**Где:** `styles.css:1097` (box-shadow-пульс) и `styles.css:4132` (transform-пульс). Второе определение молча переопределяет первое во всём файле.
**Почему:** `.voice-button.recording` (`styles.css:1086`) рассчитывает на пульсирующее кольцо box-shadow, но получает transform-пульс от `.pulse-voice`.
**Фикс:**
1. Переименуй второй кейфрейм в `styles.css:4132` в `@keyframes pulse-voice` и обнови ссылку в `.pulse-voice` (`styles.css:4137`).
2. Проверь, что `.voice-button.recording` снова получает кольцо.

### P0-3. Используется несуществующий токен `--error` → хардкод `#e55` течёт во всех темах
**Где:** `styles.css:3459` (`border-bottom-color: var(--error, #e55)`), `styles.css:3462` (`background: var(--error, #e55)`). Токен `--error` не определён нигде (есть `--danger`).
**Фикс:**
1. Замени `var(--error, #e55)` → `var(--danger)` в обеих строках.
2. Заодно убери хардкоды в dead-state блоке: `styles.css:3463` (`#fff`→`var(--accent-fg)` или `var(--fg)`), `:3479` (`#eee`→`var(--fg)`), `:3489–3491` (`#888/#333/#eee`→токены border/bg-raised/fg), `:3495` (`#555`→`var(--hover-strong)`).

### P0-4. Мёртвые кнопки в чате оркестратора (Toggle audio, Attach file)
**Где:** `OrchestratorPanel.tsx:194–201` («Toggle audio») и `:299–306` («Attach file») — у обеих есть `aria-label`, но **нет `onClick`**.
**Почему:** пользователь (и SR) видит активный контрол, жмёт — ничего не происходит.
**Фикс (выбери одно):**
- (a) Если фича не готова — добавь `disabled` + `title="Coming soon"`, либо убери кнопку до реализации.
- (b) Если готова — повесь обработчик (`onClick={toggleAudio}` / `onClick={openFilePicker}`).

### P0-5. Деструктивные действия без подтверждения
**Где:** удаление без `confirm()`:
- `ProvidersPanel.tsx:318–329` (`onDelete` провайдера, кнопка :377),
- `PromptsPanel.tsx:119–126` (delete prompt, кнопка :218),
- `SshPresetsPanel.tsx:82–89` (delete preset, кнопка :179),
- `AgentConfigPanel.tsx:101–113` (delete override),
- `HotkeyBindings.tsx:101–105` (Ctrl+Shift+W убивает тайл без спроса).
**Почему:** в проекте уже есть паттерн подтверждения (`WorkspaceSidebar`, `KanbanBoard`, `MemoryPanel`, `PigMemoryWorkbench`) — здесь он пропущен.
**Фикс:** в начале каждого `remove/onDelete` добавь `if (!confirm("Delete <X>? This cannot be undone.")) return;`. Для Ctrl+Shift+W — либо confirm, либо undo-toast. (Лучше: вынести единый `confirmDestructive(msg)` helper.)

### P0-6. Клавиатурная недоступность ключевых интерактивных зон
**Где:** `<div onClick>` / `<span onClick>` без `role`/`tabIndex`/`onKeyDown`:
- `WorkspaceSidebar.tsx:120–156` (переключение воркспейса — мышь-only),
- `KanbanBoard.tsx:236–294` (карточки задач + drag — мышь-only),
- `FilesPanel.tsx:205–219` (строки файлов) и `:228–249` (вкладки),
- `AgentTile.tsx:436–439, 595–606` (`AgentIdChip` — копирование по клику).
**Почему:** WCAG 2.1.1 — целые рабочие зоны недостижимы с клавиатуры.
**Фикс (на каждый элемент):**
1. Замени `<div onClick={fn}>` → `<div role="button" tabIndex={0} onClick={fn} onKeyDown={(e)=>{if(e.key==='Enter'||e.key===' '){e.preventDefault();fn();}}}>`.
2. Для Kanban добавь клавиатурную альтернативу drag (например, меню «Move to → status» или стрелки при фокусе).
3. Убедись, что есть видимый `:focus-visible` стиль.

### P0-7. `<iframe sandbox>` с одновременным `allow-scripts` + `allow-same-origin`
**Где:** `BrowserPanel.tsx:171`.
**Почему:** комбинация позволяет содержимому iframe снять собственный sandbox (документированный footgun MDN) — security/UX-дефект встроенного браузера.
**Фикс:** убери `allow-same-origin` если не требуется; если требуется — грузи только доверенные источники и добавь предупреждение. Минимум — задокументируй риск и ограничь схему URL (только https).

---

## P1 — Важное

### Контраст (Color)
**P1-1. `--fg-subtle` как основной вторичный текст — массовый провал WCAG AA.**
**Где:** рецепт `font-size: 10px; color: var(--fg-subtle)` повторяется 40+ раз: `styles.css:142,284(placeholder),537,603,614,793,1011,1102,1240,1282,1303,1457,1590,1626,1649,1722,1772,2266,2382,3789,3819` и в `pigmemory.css:309,411,432,488,533,776,823,954,973,1144`.
**Почему:** `--fg-subtle (#5A5F69)` на тёмных фонах < 4.5:1, особенно на 10px.
**Фикс:**
1. Для читаемого вторичного текста подними цвет до `var(--fg-muted)` и размер до `var(--text-xs)` (11px) минимум.
2. `--fg-subtle` оставь только для декоративных/disabled элементов, не для текста-контента.
3. Проверь итог инструментом контраста на всех штатных темах.

### Формы (Experience Design)
**P1-2. Нигде нет `<form>` → Enter не сабмитит.**
**Где:** `ProvidersPanel`, `NewWorkspaceModal` (только Cmd/Ctrl+Enter, :380), `SshPresetsPanel`, `PromptsPanel`, `AgentConfigPanel`.
**Фикс:** оберни поля в `<form onSubmit={(e)=>{e.preventDefault(); save();}}>`, сделай главную кнопку `type="submit"`. Это чинит Enter-submit для всех полей разом.

**P1-3. Плейсхолдеры вместо лейблов (endemic).**
**Где:** `NewWorkspaceModal` (Name), `SshPresetsPanel:116–153` (все поля), `PromptsPanel:136–166`, `AgentConfigPanel:162` (textarea), `voice/DictionaryEditor:135–242`, `BrowserPanel:114` (URL), `MentionTextarea`. **Эталон:** `ProvidersPanel.tsx:132–185` (сделано правильно).
**Почему:** placeholder исчезает при вводе и не читается частью SR (WCAG 4.1.2).
**Фикс:** на каждое поле добавь `<label htmlFor="id">` + `id` на input (как в ProvidersPanel). Если визуально лейбл не нужен — `aria-label`.

**P1-4. Ошибки валидации только тостом, без `aria-live`/`aria-describedby`.**
**Где:** `ProvidersPanel:198,232`, `NewWorkspaceModal:482–484`, `SshPresetsPanel` (port), `PromptsPanel:91`, `AgentConfigPanel:81`.
**Фикс:** контейнеру ошибки добавь `role="alert"`; свяжи с полем через `aria-describedby={errId}`/`aria-errormessage`; показывай ошибку инлайн рядом с полем, а не только тостом.

### Иконочные кнопки без имени (a11y)
**P1-5. `title=` без `aria-label` на icon-only кнопках.**
**Где:** `AgentTile.tsx:441–456,489–503`, `WorkspaceSidebar.tsx:135–152`, `PromptsPanel.tsx:142,188,215`, `SshPresetsPanel.tsx:109,178`, `AgentConfigPanel.tsx:193`, `MemoryPanel.tsx:294–298`, PigMemory-кнопки (`ActivityTimeline:73`, `SmartStatusPill:62`).
**Почему:** многие SR не озвучивают `title` на `<button>`.
**Фикс:** добавь `aria-label="<действие>"` каждой иконочной кнопке (можно скриптом-проверкой в eslint-plugin-jsx-a11y).

### Модалки и меню (a11y / UX)
**P1-6. Невалидная вложенность интерактивных элементов.**
**Где:** `SkillsPanel.tsx:198–219` — `<span role="switch">` внутри `<button class="skills-row">`.
**Фикс:** вынеси переключатель из кнопки-строки: либо строка = `<div role="button">` + соседний `<button role="switch">`, либо переработай в список с отдельной зоной toggle.

**P1-7. Меню/поповеры без закрытия по Escape/outside-click и без фокус-трапа.**
**Где:** `AgentTile.tsx:506–519` (контекст-меню — нет `role="menu"`, нет навигации стрелками), `TilingArea.tsx:185–208` (Rooms dropdown — нет Escape/outside-close), `SettingsButton.tsx:255–291` (overlay не делает фон inert, Tab уходит на фон).
**Фикс:** для каждого поповера: `role="menu"`+`menuitem`, обработчик Escape→close, клик вне→close, фокус-трап (focus в первый пункт при открытии, возврат на триггер при закрытии). Вынеси переиспользуемый хук `usePopover`.

**P1-8. Combobox без ARIA в MentionTextarea.**
**Где:** `MentionTextarea.tsx:204–240` — нет `role="combobox"`/`aria-expanded`/`aria-controls`/`aria-activedescendant`; опции — `<button role="option">` (невалидно). **Эталон:** `PathMentionTextarea.tsx:311–359`.
**Фикс:** скопируй ARIA-разметку из `PathMentionTextarea`; опции делай `<li role="option">`/`<div role="option">`, не `<button>`.

### Гонки и тихие ошибки (UX)
**P1-9. Async без cancelled-флага → перезапись свежих данных устаревшими.**
**Где:** `PromptsPanel.reload:30`, `AgentConfigPanel.reload:49`, `MemoryGraph:41`, `FilesPanel.openFile:99/listDir:51`, `BrowserPanel.bootstrap:37`, `ArchitectPanel.getSetting:39`.
**Фикс:** в каждом useEffect добавь `let cancelled=false; … if(!cancelled) setX(); return ()=>{cancelled=true;}` (как уже сделано в `VoiceSettings.tsx:20–55`).

**P1-10. Тихие падения IPC (только `console.error`).**
**Где:** `OrchestratorPanel` BridgeOrb:344 / VoiceIconButton:752, `ArchitectPanel.toggle:81` (нет catch), `SmartStatusPill.onToggle:43`, `TilingArea.listRoomTemplates:29`.
**Фикс:** добавь `pushToast({text, kind:"error"})` в catch, чтобы пользователь видел сбой (особенно для voice — отказ микрофона).

**P1-11. Двойной сабмит — кнопки не disabled во время async.**
**Где:** `OrchestratorPanel.send` (кнопка :318 disabled только по `!draft.trim()`), `PromptsPanel.save:90`, `SshPresetsPanel.save:51`, `AgentConfigPanel.save:81`, `SkillsPanel.createStub:130`.
**Фикс:** заведи `const [busy,setBusy]=useState(false)`, оборачивай async в `setBusy(true)…finally setBusy(false)`, на кнопке `disabled={busy || …}`.

### i18n
**P1-12. RU-строки в EN-интерфейсе.**
**Где:** `MemoryPanel.tsx:131` («Удалить заметку?»), `KanbanBoard.tsx:95` («Удалить задачу?»). Остальные `confirm()` — на английском.
**Фикс:** приведи к одному языку (EN, как остальной UI) или внедри i18n-словарь. Краткосрочно — замени две строки на английские.

### z-index конфликт
**P1-13. `.nws-backdrop` перекрывает theme-picker.**
**Где:** `styles.css:2917` (`z-index: 3100`) > `--z-theme-picker: 3000` (`tokens.css:100`). Также `voice-pill z-index:1100` == `--z-toast:1100` (`styles.css:1993`) → тосты могут прятаться под pill.
**Фикс:** переведи `.nws-backdrop` на `var(--z-modal)`; voice-pill — на отдельный слой ниже тостов (или подними `--z-toast`). Используй токены, не литералы.

---

## P2 — Полировка (визуальная консистентность)

### Typography
- **P2-1. 22 хардкода `font-family`** (`ui-monospace, "Fira Code", monospace`) вместо `var(--font-mono)`: `styles.css:19,46,75,84,141,164,1280,1455,1500,1614,1657,1802,1855,1915,1926,1968,2410,2437,2442,2480,2531,2586`. → заменить на токен.
- **P2-2. ~95 литералов `font-size`** вне `--text-*`; 49 раз `10px` (вне шкалы, ближайший `--text-xs:11px`), 4 раза `9px` (токена нет). → ввести `--text-2xs:10px` ИЛИ поднять до 11px; заменить литералы токенами.
- **P2-3. Потеря line-height** (~30 мест, размер без парного `-lh`). → задавать пары `font-size: var(--text-xs); line-height: var(--text-xs-lh);`.
- **P2-4. Markdown em-relative шкала** (`styles.css:4236–4239`, `pigmemory.css:868–871,1124`) вне дизайн-шкалы. → нормализовать к `--text-*`.

### Spacing
- **P2-5. 60+ off-grid значений** (5/7/9/13/15/18px). Примеры: `styles.css:1067(18px),1314(50px),2042(96px)`; `pigmemory.css:258,315(5px),337(9px),602(48px),956(64px)`. → округлить к ближайшему `--space-*` или добавить недостающие токены (`--space-9:18px`, и т.п.).
- **P2-6. Непоследовательные паддинги одинаковых элементов** (header/card/list-item/pill). → ввести семантические токены (`--header-pad`, `--card-pad`) и применить единообразно.
- **P2-7. `border-radius: 50%`** в 9 местах вместо `--radius-circle`; ~47 литералов radius. → заменить токенами.

### Color / Visuals
- **P2-8. Bridge Orb целиком на хардкод-оранжевых/серых** (`styles.css:3247–3340`): `#2a1f14/#0f0b08`, `rgba(220,150,50,…)` и др. → завязать на `--accent`/`--accent-soft` и тенями через токены, иначе компонент игнорирует тему.
- **P2-9. `.workspace-item.active` хардкод `rgba(30,58,95,…)`** (`styles.css:530,595`) — выделение не темизируется. → `var(--selection)` / `var(--accent-soft)`.
- **P2-10. Несогласованные focus-ring** (`styles.css:312` глобально vs `:606,752,3274,3526,3587,4017` локально пересоздают через outline:none+box-shadow). → единый паттерн на `--ring`/`--ring-width`/`--ring-offset`.

### Motion
- **P2-11. Литеральные длительности/easing** (`styles.css:1081,1270,3264,3728,3998,4075,3555,1090,1058,797,2009,2546,4335`). → перевести на `--dur-*`/`--ease-*`.
- **P2-12. Дублирующие prefers-reduced-motion блоки** (`styles.css:4139–4148,4177–4179` — мертвы, перекрыты глобальным :1166). `pigmemory.css` своего блока не имеет. → удалить мёртвые, добавить защитный в pigmemory.

### Overflow / responsive
- **P2-13. Flex-дети без `min-width:0`** (текст не усекается, ломает контейнер): `styles.css:534(workspace name),697(tile title),1497,1684,2414`; `pigmemory.css:84`. → добавить `min-width:0`.
- **P2-14. Фиксированные сетки без reflow**: `styles.css:3115` (`.nws-presets repeat(7,…)` сминается < 600px). → `repeat(auto-fill, minmax(…))`.
- **P2-15. Таблицы markdown без горизонтального скролла** (`styles.css:4288`). → обернуть в контейнер `overflow-x:auto`.
- **P2-16. voice-pill `max-width:460px` без `calc(100vw - …)`** (`styles.css:1982`) — может вылезти за вьюпорт. → добавить fallback.

### Дубли / мёртвый CSS
- **P2-17. `.empty-state` определён дважды** (`styles.css:611` off-grid vs `:2647` токены) → удалить первый.
- **P2-18. `@keyframes pulse`** (`styles.css:907`) не используется → удалить.
- **P2-19. `var(--x, var(--x))` self-fallback** (~35 мест: SSH/cmdblock/mention/files-tab блоки) → схлопнуть до `var(--x)`.

### Прочие UX-нюансы
- **P2-20. Backdrop закрывает модалку при drag-select** (`NewWorkspaceModal.tsx:367`, `ThemePicker.tsx:60`) — выделение текста с релизом на фоне закрывает окно. → закрывать только если `e.target===e.currentTarget` И mousedown стартовал на backdrop.
- **P2-21. NoteList: навигация стрелками сразу открывает заметку** (`pigmemory/NoteList.tsx:124–138` + `PigMemoryWorkbench:330`) → шторм `read_memory`+confirm. → стрелки только перемещают фокус, открытие по Enter.
- **P2-22. Markdown: незакрытый ```-блок съедает остаток** (`Markdown.tsx:53`, `MarkdownPreview`) → при EOF без закрытия трактовать как параграф.
- **P2-23. `prompt()`/`confirm()` блокирующие** (`WorkspaceSidebar.tsx:65`, `BrowserPanel.tsx:90`) → заменить на инлайн-инпут/модалку (консистентно с остальным UI).

---

## Сводка по объёму

| Категория | P0 | P1 | P2 |
|---|---|---|---|
| Color / контраст | 1 (#3) | 2 (#1,#13) | 3 |
| Typography | — | — | 4 |
| Spacing | — | — | 3 |
| Motion | — | — | 2 |
| Overflow/responsive | — | — | 4 |
| Дубли/мёртвый CSS | 1 (#2) | — | 3 |
| a11y | 1 (#6) | 4 (#3,#5,#6,#8) | 2 |
| UX/состояния | 3 (#4,#5,#7) | 4 (#9,#10,#11,#12) | 4 |
| Robustness | 1 (#1) | — | — |

## Рекомендованный порядок работ
1. **Спринт 1 (P0):** ErrorBoundary → pulse-rec → `--error` → мёртвые кнопки → confirm на удаление → клавиатурная доступность → iframe sandbox.
2. **Спринт 2 (P1):** контраст `--fg-subtle` (массовый автозамен) → `<form>`+лейблы+`aria-live` → aria-label иконкам → cancelled-флаги/тосты ошибок → i18n → z-index.
3. **Спринт 3 (P2):** прогон токенизации (font/size/space/radius/motion) — можно полуавтоматически через скрипт+ревью, затем дубли/overflow/мелкий UX.

> Полная WCAG-валидация требует ручного тестирования со скринридером и экспертной проверки — этот аудит покрывает дефекты, выявляемые из кода и live-прогона.
