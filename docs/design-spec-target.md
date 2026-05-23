# pigide — Design Spec for `target.png`

> Source of truth: `/home/camer/pigide/target.png`. This document is the
> measurable contract for the three parallel builders. Every value here is a
> hard target — implementers must hit ±1px / ±2% on color, never invent new
> tokens, and never silently substitute "close enough" components.

**Stack alignment**: React 19 + Vite + Allotment, Tauri 2 backend. Token system
already lives in `frontend/src/styles/tokens.css` + `frontend/src/themes/catalog.ts`
(`applyThemeToDom`). All values below MUST land in those two files — do not
fork another token system.

**Thread for questions**: PigMCP `design-target-2026-05`.
**Spec version**: v1 (2026-05-21).

---

## 0. Scope guarantees

- This spec covers the static visual contract: layout grid, color, typography,
  spacing, radii, elevation, motion, focus rings.
- It does NOT freeze interaction logic. Backend stays as-is unless a region
  below explicitly mandates a change ("BACKEND CHANGE" callout).
- Animations are bounded to existing `--dur-*` / `--ease-*` tokens — no new
  motion vocabulary.

---

## 1. Outer layout grid

```
┌────────────┬──────────────────────────────────────┬────────────────┐
│            │ tiling area (TilingArea)             │                │
│  workspace │                                      │  orchestrator  │
│  sidebar   │  ┌──────────┐  ┌──────────┐          │  panel         │
│            │  │ AgentTile│  │ AgentTile│          │  (chat + queue)│
│            │  │ pty/web  │  │ pty/web  │          │                │
│            │  └──────────┘  └──────────┘          │                │
│            │  ┌──────────┐  ┌──────────┐          │                │
│            │  │ AgentTile│  │ AgentTile│          │                │
│            │  └──────────┘  └──────────┘          │                │
└────────────┴──────────────────────────────────────┴────────────────┘
       220              flex: 1                            320
       (min 160)        (min 300)                          (min 260)
```

- Outer container: `Allotment` (`frontend/src/App.tsx:236`) with
  `defaultSizes={[220, 600, 320]}`. Keep these values.
- Pane separators: `1px` solid `var(--border)`. The Allotment splitter handle
  itself takes 4px hit zone, but only the 1px line is visible.
- Floating chrome (z-order, lowest → highest):
  - `--z-sticky` — sidebar/orchestrator headers
  - `--z-overlay` — `SettingsButton` (bottom-right, 16px / 16px)
  - `--z-popover` — theme picker, quick open
  - `--z-toast` — `.toast-wrap` (top-right, 16px / 16px)
  - `--z-tooltip` — voice pill (`VoicePill`, bottom-center, 24px from bottom)
- App background flood: `var(--bg)`. Sidebar + orchestrator panels:
  `var(--bg-panel)`. Tiles surface: `var(--bg-raised)`.

---

## 2. Color tokens (theme-AGNOSTIC palette to land in `applyThemeToDom`)

The reference target reads as a deep, neutral dark with a single warm accent.
Apply this as a NEW theme entry called `target` (id `target`, name `Target`)
in `frontend/src/themes/catalog.ts`. Do not modify existing themes.

| Token            | Hex/RGBA                  | Usage                              |
|------------------|---------------------------|------------------------------------|
| `--bg`           | `#0B0C0F`                 | App body flood                     |
| `--bg-panel`     | `#101216`                 | Sidebar, orchestrator, headers     |
| `--bg-raised`    | `#15181D`                 | Tiles, modals, raised surfaces     |
| `--bg-overlay`   | `rgba(11, 12, 15, 0.72)`  | Modal backdrop                     |
| `--fg`           | `#E4E6EA`                 | Primary text                       |
| `--fg-muted`     | `#8A8F99`                 | Secondary text, timestamps         |
| `--fg-subtle`    | `#5A5F69`                 | Tertiary text, hint text           |
| `--border`       | `#1F232A`                 | 1px lines, tile borders            |
| `--border-strong`| `#2A2F38`                 | Hover/focus borders                |
| `--accent`       | `#E89A4A`                 | Active tab, primary CTA, brand     |
| `--accent-fg`    | `#0B0C0F`                 | Text on accent fill                |
| `--accent-soft`  | `rgba(232, 154, 74, 0.16)`| Active tab fill, badge bg          |
| `--accent-strong`| `rgba(232, 154, 74, 0.55)`| Focus ring high-contrast           |
| `--danger`       | `#EF4444`                 | Error toast, destructive btn       |
| `--danger-soft`  | `rgba(239, 68, 68, 0.16)` | Error toast bg                     |
| `--success`      | `#4ADE80`                 | Success toast, healthy status dot  |
| `--success-soft` | `rgba(74, 222, 128, 0.16)`| Success toast bg                   |
| `--warn`         | `#E89A4A`                 | Warn toast (mirrors accent)        |
| `--warn-soft`    | `rgba(232, 154, 74, 0.16)`| Warn toast bg                      |
| `--info`         | `#60A5FA`                 | Info toast, link                   |
| `--info-soft`    | `rgba(96, 165, 250, 0.16)`| Info toast bg                      |
| `--selection`    | `#1E3A5F`                 | Text selection bg                  |
| `--hover`        | `rgba(255, 255, 255, 0.04)` | Default hover veil                |
| `--hover-strong` | `rgba(255, 255, 255, 0.07)` | Pressed hover veil                |
| `--active`       | `rgba(255, 255, 255, 0.10)` | Selected row bg                   |
| `--ring`         | `rgba(232, 154, 74, 0.45)`| Focus ring                         |
| `--scrollbar-thumb`       | `rgba(255, 255, 255, 0.10)` | scrollbar thumb         |
| `--scrollbar-thumb-hover` | `rgba(255, 255, 255, 0.18)` | scrollbar thumb hover   |

**Status / agent dots** (used in tile chrome and sidebar agent counts):
- `running`  → `--success` (`#4ADE80`)
- `idle`     → `--fg-muted` (`#8A8F99`)
- `exited`   → `--fg-subtle` (`#5A5F69`)
- `error`    → `--danger` (`#EF4444`)
- `streaming`→ `--accent` (`#E89A4A`) with 1.4s pulse on `opacity` 0.4↔1.0

**Xterm theme** (one-to-one with css palette so terminals don't break theme):
```ts
xterm: {
  background: "#0B0C0F",
  foreground: "#E4E6EA",
  cursor: "#E89A4A",
  selectionBackground: "#1E3A5F",
  black: "#101216", red: "#EF4444", green: "#4ADE80",
  yellow: "#E89A4A", blue: "#60A5FA", magenta: "#C678DD",
  cyan: "#56B6C2", white: "#E4E6EA",
}
```

---

## 3. Typography

Font stacks already defined in `tokens.css` (`--font-sans`, `--font-mono`).
Do NOT change the stacks — only enforce sizes/weights/line-heights below.

| Class / use            | Size  | Line-height | Weight | Letter-spacing | Family       |
|------------------------|-------|-------------|--------|----------------|--------------|
| `.app-title` (sidebar) | 13px  | 1.30        | 600    | -0.005em       | sans         |
| `.section-label`       | 11px  | 1.45        | 500    | 0.06em (caps)  | sans         |
| `.body` (default)      | 13px  | 1.55        | 400    | 0              | sans         |
| `.body-md`             | 14px  | 1.50        | 400    | 0              | sans         |
| `.body-strong`         | 13px  | 1.55        | 600    | 0              | sans         |
| `.muted`               | 12px  | 1.50        | 400    | 0              | sans, fg-muted |
| `.tabular`             | 12px  | 1.50        | 500    | 0              | mono         |
| `.tile-title`          | 12px  | 1.45        | 500    | 0              | sans         |
| `.tile-status`         | 11px  | 1.45        | 500    | 0.04em         | sans, fg-muted |
| `.terminal` (xterm)    | 13px  | 1.50        | 400    | 0              | mono         |
| `.code-block`          | 12px  | 1.55        | 400    | 0              | mono         |
| `.button`              | 12px  | 1.40        | 500    | 0              | sans         |
| `.input`               | 13px  | 1.50        | 400    | 0              | sans         |

Token mapping (already in `tokens.css`):
- `--text-xs` 11 / `--text-xs-lh` 1.45
- `--text-sm` 12 / `--text-sm-lh` 1.50
- `--text-base` 13 / `--text-base-lh` 1.55
- `--text-md` 14 / `--text-md-lh` 1.50
- `--text-lg` 16 / `--text-lg-lh` 1.40
- `--text-xl` 18 / `--text-xl-lh` 1.35

Implementers MUST reference `var(--text-*)` — never hardcode `13px`. If a value
is missing here, the answer is "use the existing `--text-*` step that fits".

---

## 4. Spacing scale

Use ONLY tokens from `tokens.css`. No raw px in component styles.

`--space-0..20` → 0, 2, 4, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40.

Per-region spacing budget:

| Region                     | Outer pad        | Inner gap        |
|----------------------------|------------------|------------------|
| Workspace sidebar          | 12px             | 4px between rows |
| Sidebar header             | 12px / 14px      | 8px              |
| Sidebar workspace row      | 8px / 10px       | 6px icon↔label   |
| Tiling area                | 8px              | 6px between tiles|
| AgentTile chrome (header)  | 8px / 10px       | 8px              |
| AgentTile body             | 0 (terminal flush) | 0              |
| Orchestrator panel         | 12px             | 12px between sects |
| Orchestrator chat row      | 10px / 12px      | 6px              |
| Orchestrator composer      | 10px             | 8px              |
| Settings button            | 14px             | n/a              |
| Toast                      | 10px / 12px      | 8px gap stacked  |
| Modal                      | 24px             | 16px sections    |

---

## 5. Radii, borders, shadows

| Token         | Value | Usage                           |
|---------------|-------|---------------------------------|
| `--radius-sm` | 3px   | input fields, tiny chips        |
| `--radius-md` | 4px   | buttons, badges                 |
| `--radius-lg` | 6px   | tiles, sidebar rows             |
| `--radius-xl` | 8px   | cards, modals                   |
| `--radius-2xl`| 12px  | large modals, settings sheet    |
| `--radius-pill` | 999px | voice pill, status pills      |

Borders:
- Default: `1px solid var(--border)`.
- Hover/focus: `1px solid var(--border-strong)`.
- Active (selected workspace row, active tile): `1px solid var(--accent)`.

Shadows (already in `tokens.css`, do not redefine):
- `--shadow-1` — sidebar row hover, tile resting (subtle).
- `--shadow-2` — settings button, voice pill.
- `--shadow-3` — modals, theme picker popover.
- `--shadow-4` — quick open palette.

Focus ring (consistent across ALL focusables):
```css
outline: var(--ring-width) solid var(--ring);
outline-offset: var(--ring-offset);
```
No `box-shadow`-based rings — keyboard accessibility depends on `outline` for
forced-colors mode.

---

## 6. Region specs

### 6.1 Workspace sidebar (`frontend/src/components/WorkspaceSidebar.tsx`)

- Container: `width: 220px` (Allotment-managed, min 160), `bg-panel`,
  right border `1px solid var(--border)`.
- Header: 44px tall, flex row, padding `12px 14px`, app title left
  (`.app-title`, `--fg`), "+ new" icon-button right (28×28, `--radius-md`).
- Workspace list: vertical, 4px row gap, 12px outer padding.
- Workspace row:
  - 36px tall, `--radius-lg`, padding `8px 10px`, flex row, gap 8px.
  - Icon: 16×16, color `--fg-muted`. Active row icon: `--accent`.
  - Label: `.body`, color `--fg`. Active row label: `--fg`, weight 600.
  - Agent count badge: pill, height 18px, padding `0 6px`, font 10px / 600,
    bg `--accent-soft`, text `--accent`. Empty workspaces hide the badge.
  - Hover: `bg: var(--hover)`. Active: `bg: var(--accent-soft)`,
    1px left rail `--accent` (4px wide, full row height, `--radius-md`).
- Footer: 40px tall, sticky bottom, settings/theme/help icon row,
  `border-top: 1px solid var(--border)`.

### 6.2 Tiling area (`frontend/src/components/TilingArea.tsx`)

- Container: flexes to fill, `bg: var(--bg)`, padding 8px.
- Tile splitter: 1px `--border`. Hover splitter: 2px `--border-strong`.
- Empty state (no agents):
  - Centered group, gap 16px.
  - Icon 48×48, `--fg-subtle`.
  - Title `.body-md` `--fg`, body `.muted`.
  - CTA button (filled accent, see §7) "Spawn agent" — opens command palette.

### 6.3 AgentTile (`frontend/src/components/AgentTile.tsx`)

- Border `1px solid var(--border)`, radius `--radius-lg`, bg `--bg-raised`.
- Active tile (focused / last-interacted): border `--accent`, `--shadow-1`.
- Header bar:
  - Height 32px, padding `0 10px`, flex row, gap 8px.
  - Status dot 8×8 circle, color per §2 status mapping. Streaming pulses 1.4s.
  - Agent name `.tile-title` `--fg` truncated with `text-overflow: ellipsis`.
  - Tag chip (`claude`, `kiro-cli`, etc.): `--text-xs`, height 18px,
    padding `0 6px`, `--radius-sm`, bg `--accent-soft`, text `--accent`.
  - Right cluster: 24×24 icon-buttons (close, expand, mute), 4px gap.
  - Header bottom border: 1px `--border`.
- Body: terminal flush to edges, no inner padding, xterm theme from §2.
- Resize handle: relies on Allotment, no custom handle inside the tile.

### 6.4 Orchestrator panel (`frontend/src/components/OrchestratorPanel.tsx`)

- Container: 320px wide (Allotment), `bg-panel`, `border-left: 1px var(--border)`.
- Header (44px): title "Orchestrator" `.app-title`, status dot (per §2),
  queue count badge (mirror sidebar agent badge), gear icon right.
- Tab strip (32px, optional): Chat / Tasks / Memory. Active tab: bottom border
  2px `--accent`, label `--fg` weight 600. Inactive: `--fg-muted`.
- Chat scroller: padding `12px`, vertical gap 12px between message rows.
- Message row:
  - User: right-aligned, max-width 86%, bg `--accent-soft`, text `--fg`,
    `--radius-lg` (top-right `--radius-sm`), padding `10px 12px`.
  - Assistant: left-aligned, max-width 86%, bg `--bg-raised`, text `--fg`,
    `--radius-lg` (top-left `--radius-sm`), padding `10px 12px`.
  - System / tool: full-width, bg transparent, `.muted`, monospace tag prefix.
  - Timestamp: `.tile-status`, 4px below message body, right-aligned.
- Composer:
  - Bottom of panel, `border-top: 1px var(--border)`, padding 10px.
  - Textarea: bg `--bg-raised`, border 1px `--border`, radius `--radius-md`,
    padding `10px 12px`, min-height 44px, max-height 160px, autosize.
  - Send button: 32×32, accent fill, `--radius-md`, disabled at 40% opacity
    when empty.

### 6.5 SettingsButton, VoicePill, Toasts

- `SettingsButton`: 36×36 fab, bottom-right `16/16`, bg `--bg-raised`, border
  1px `--border`, `--shadow-2`. Icon 18×18 `--fg`.
- `VoicePill`: pill `--radius-pill`, height 36px, padding `0 14px 0 10px`,
  bg `--bg-raised`, border 1px `--border`, `--shadow-2`. Idle text `--fg-muted`.
  Listening: border `--accent`, dot `--accent` pulsing 1.4s.
- `.toast-wrap`: top-right `16/16`, gap 8px between toasts, max-width 360px.
- `.toast`: padding `10px 12px`, radius `--radius-md`, `.body`, color `--fg`,
  border 1px (kind-soft → kind), bg `var(--{kind}-soft)`. Auto-dismiss 4s
  (already in `App.tsx`).

### 6.6 Theme picker / quick open (modals)

- Backdrop: `var(--bg-overlay)` with `backdrop-filter: blur(8px)`.
- Surface: bg `--bg-raised`, `--radius-2xl`, `--shadow-4`, border 1px `--border`.
- Width: 480px (theme picker), 640px (quick open). Centered top, 96px from top.
- Search input: full-width, 44px tall, no border, divider below 1px `--border`.
- Result row: 40px, padding `0 16px`, gap 12px, hover `var(--hover)`,
  selected `var(--accent-soft)` with left rail (mirror sidebar active).

---

## 7. Component contracts (buttons / inputs / chips)

### Buttons

| Variant     | Height | Padding | Bg                      | Text          | Border             |
|-------------|--------|---------|-------------------------|---------------|--------------------|
| primary     | 32px   | 0 14px  | `--accent`              | `--accent-fg` | none               |
| secondary   | 32px   | 0 14px  | `--bg-raised`           | `--fg`        | 1px `--border`     |
| ghost       | 32px   | 0 12px  | transparent             | `--fg`        | none               |
| destructive | 32px   | 0 14px  | `--danger`              | `#fff`        | none               |
| icon-only   | 28×28  | 0       | transparent (hover `--hover`) | `--fg`  | none               |
| icon-only-lg| 36×36  | 0       | `--bg-raised`           | `--fg`        | 1px `--border`     |

All buttons: `--radius-md`, font `.button`, `transition: background var(--dur-fast) var(--ease-out), border-color var(--dur-fast) var(--ease-out)`.

Disabled state: opacity 0.45, `cursor: not-allowed`.

### Inputs

- Textarea / input: bg `--bg-raised`, border 1px `--border`, radius `--radius-md`,
  padding `10px 12px`, font `.input`, color `--fg`, placeholder `--fg-subtle`.
- Focus: border `--accent`, ring per §5.

### Chips / badges

- Height 18px, padding `0 6px`, radius `--radius-sm`, font `--text-xs` weight 500.
- Tones: accent (`--accent-soft` / `--accent`), success, danger, info, neutral
  (`var(--hover-strong)` / `--fg-muted`).

---

## 8. Motion

- Hover veil: opacity 0 → 1 in `--dur-fast` (120ms) `--ease-out`.
- Modal in: `--dur-base` (160ms), `--ease-out`. Translate-Y 4px → 0 + opacity 0 → 1.
- Toast in: same as modal. Toast out: `--dur-fast`, opacity → 0.
- Streaming dot: 1.4s `--ease-in-out` infinite alternate, 0.4 ↔ 1.0 opacity.
- Splitter drag: no transition (1:1 with cursor).
- Theme switch: NO crossfade (causes flicker on dark↔light boundaries) —
  apply tokens synchronously.

---

## 9. CSS custom properties — full list (must be present)

### From `tokens.css` (theme-agnostic)
`--font-sans`, `--font-mono`, `--text-xs[..xl]`, `--text-*-lh`, `--lh-mono`,
`--lh-tight`, `--space-0..20`, `--radius-none/sm/md/lg/xl/2xl/pill/circle`,
`--border-width`, `--border-width-strong`, `--dur-instant/fast/base/slow/slower`,
`--ease-out`, `--ease-in-out`, `--ease-spring`,
`--z-base/sticky/dropdown/overlay/popover/modal/toast/tooltip/quick-open/theme-picker`,
`--ring-width`, `--ring-offset`.

### From `applyThemeToDom` (palette + derived)
`--bg`, `--bg-panel`, `--bg-raised`, `--bg-overlay`, `--fg`, `--fg-muted`,
`--fg-subtle`, `--border`, `--border-strong`, `--accent`, `--accent-fg`,
`--accent-soft`, `--accent-strong`, `--danger`, `--danger-soft`, `--success`,
`--success-soft`, `--warn`, `--warn-soft`, `--info`, `--info-soft`,
`--selection`, `--hover`, `--hover-strong`, `--active`, `--scrollbar-thumb`,
`--scrollbar-thumb-hover`, `--shadow-1..4`, `--ring`.

If a builder needs a value that's not in this list, the answer is to compose
from existing tokens (e.g. `color-mix(in srgb, var(--fg) 70%, transparent)`)
— do NOT introduce a new variable without thread-`design-target-2026-05`
sign-off.

---

## 10. Accessibility & forced-colors

- Focus ring uses `outline`, not `box-shadow` (works in forced-colors mode).
- Min contrast: `--fg` on `--bg`, `--fg` on `--bg-panel`, `--fg` on `--bg-raised`
  must each be ≥ 7:1 (WCAG AAA body). All target values above are within budget.
- `--fg-muted` on the same backgrounds must be ≥ 4.5:1.
- `--accent` on `--accent-fg` must be ≥ 4.5:1.
- All status colors must be paired with an icon or text label — color alone is
  not the signal (see status dot + `aria-label="running"`).

---

## 11. What changes in the codebase

These are the only files this spec touches. Implementers MUST `claim_files`
before editing.

| File | Change |
|---|---|
| `frontend/src/styles/tokens.css` | Update default fallbacks at the bottom to the §2 palette so cold-load matches `Target` theme. No new tokens. |
| `frontend/src/themes/catalog.ts` | Add `target` theme entry per §2; mark as default in `THEMES` ordering (first). |
| `frontend/src/styles.css` | Per-region rules per §6. Replace any hard-coded hex with `var(--…)`. |
| `frontend/src/components/WorkspaceSidebar.tsx` | Match §6.1 dimensions, header, footer, active-rail. |
| `frontend/src/components/AgentTile.tsx` | Match §6.3 chrome (32px header, status dot, tag chip, right-cluster icons). |
| `frontend/src/components/TilingArea.tsx` | 8px outer pad, 6px tile gap, empty-state per §6.2. |
| `frontend/src/components/OrchestratorPanel.tsx` | §6.4 header, message rows, composer. |
| `frontend/src/components/SettingsButton.tsx` | 36×36 fab per §6.5. |
| `frontend/src/components/voice/VoicePill.tsx` | Pill geometry per §6.5. |
| `frontend/src/components/ThemePicker.tsx` | Modal surface per §6.6. |

**No backend changes required.** Theme registration is pure frontend.

---

## 12. Acceptance checklist (each builder runs before handoff)

- [ ] No hard-coded hex/rgb in any modified file (grep `#[0-9a-fA-F]{3,8}`,
      `rgb(`, `rgba(` outside `themes/catalog.ts`).
- [ ] All px values map to a `--space-*` step or are deliberately commented
      "geometry: not a spacing token".
- [ ] App boots into `Target` theme by default.
- [ ] Tab/Shift-Tab cycles all interactive elements with a visible ring.
- [ ] Allotment splitters render as a 1px `--border` line at rest.
- [ ] No console warnings about unknown CSS variables.
- [ ] Visual diff vs `target.png` at default sizes is within ±2px on every
      named region — capture via Playwright (`/run` skill) and attach to PR.

---

## 13. Open questions for the design lead (NONE block implementation)

1. Right-side orchestrator: does target.png show tab strip (Chat/Tasks/Memory)
   or chat-only? Spec assumes tab strip; collapse to chat-only is a
   1-component change behind `showOrchestratorTabs` prop.
2. Sidebar agent count badge — show `0` or hide? Spec hides; flip to "show 0"
   if telemetry says operators care.

These are clarifications, not blockers. Ship as specified.
