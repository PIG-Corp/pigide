---
status: diagnosed
trigger: "Arrow keys and backspace don't work in agent chat textareas; user sees raw escape sequences ([A[B etc.) inserted into the terminal instead of cursor movement/deletion."
created: 2026-06-05T00:00:00Z
updated: 2026-06-05T00:00:00Z
---

## Current Focus

hypothesis: `sanitizeForPty()` in `frontend/src/components/AgentTile.tsx` strips the `ESC` (0x1B) byte and the `DEL` (0x7F) byte from every keystroke sent to the PTY. xterm.js emits arrow keys as `ESC [ A` and backspace as `0x7F`; after sanitisation, arrows become the literal printable string `[A` and backspace becomes the empty string. The PTY (and the CLI running inside it) therefore never receives the escape, never sees a backspace.
test: Run the exact regex `/[\x00-\x08\x0B-\x0C\x0E-\x1F\x7F]/g` over the byte sequences xterm.js emits for ArrowUp, ArrowDown, Backspace, Ctrl+C, Tab, Enter.
expecting: ArrowUp/Down/Left/Right and Backspace/Ctrl+C survive only as printable garbage. Tab/CR/LF are preserved (those are explicitly excluded from the regex).
next_action: Report root cause and minimal fix.

## Symptoms

expected: ArrowUp/Down/Left/Right move the caret/history inside the CLI running in the agent tile; Backspace deletes characters.
actual: Arrow keys insert literal text `[A`, `[B`, `[C`, `[D` into the agent terminal; backspace does nothing. The terminal also shows `[O`, `[I` (focus reports) and `[?62;4;9;22c` (DECRQM mode response) as visible noise because every emitted ESC is silently dropped at the source.
errors: none — bug is silent stripping.
reproduction: Focus an agent tile, press any arrow key or backspace. The keystroke is consumed by xterm.js, encoded to `ESC [ A` (or `0x7F`), passed to `sanitizeForPty` in `AgentTile.tsx#L73`, which removes the leading `ESC` (in the `\x0E-\x1F` class) or strips the `0x7F` entirely.
started: introduced by commit 8913018 / 71d090d era when `sanitizeForPty` was added to the `term.onData` handler.

## Eliminated

- hypothesis: Custom `onKeyDown` in `MentionTextarea` / `PathMentionTextarea` swallows ArrowUp/ArrowDown.
  evidence: Those handlers only intercept arrows when the `@`-mention popup is open (lines 170–191 of `MentionTextarea.tsx`, 244–266 of `PathMentionTextarea.tsx`). They don't run in the AgentTile path. Also, MentionTextarea is wired only to OrchestratorPanel (the right pane), not the agent chat itself.
  timestamp: 2026-06-05T00:00:00Z

- hypothesis: `useHotkeys` document-level keydown listener intercepts the keys.
  evidence: `useHotkeys` (frontend/src/hooks/useHotkeys.ts:91) skips editable targets unless the binding includes Shift (line 97). None of the registered hotkeys bind arrow keys or Backspace anyway.
  timestamp: 2026-06-05T00:00:00Z

- hypothesis: `useInputHistory` swallows ArrowUp/ArrowDown.
  evidence: That hook is not used by AgentTile at all — it only exists for OrchestratorPanel-style history, and the symptoms are in agent tiles.
  timestamp: 2026-06-05T00:00:00Z

- hypothesis: `stripControl` in MentionTextarea/PathMentionTextarea strips arrow bytes that race into the textarea via xterm focus loss.
  evidence: That code path only runs in the right-side orchestrator/architect chats. The user reports the bug in the AGENT chat (xterm.js terminal). And even if it ran here, the textarea wouldn't be the destination — the agent terminal IS the xterm.js instance.
  timestamp: 2026-06-05T00:00:00Z

## Evidence

- timestamp: 2026-06-05T00:00:00Z
  checked: `frontend/src/components/AgentTile.tsx:72–75`
  found: `const CTRL_CHARS = /[\x00-\x08\x0B-\x0C\x0E-\x1F\x7F]/g;` followed by `function sanitizeForPty(s) { return s.replace(CTRL_CHARS, ""); }`.
  implication: `\x1B` (ESC, decimal 27) is inside `\x0E-\x1F` (decimal 14–31) and gets stripped; `\x7F` (DEL / backspace on many keyboards) is also stripped. Tab/LF/CR are preserved.

- timestamp: 2026-06-05T00:00:00Z
  checked: `frontend/src/components/AgentTile.tsx:153–157`
  found: `term.onData((data) => { ipc.writeToAgent(agent.id, toB64(sanitizeForPty(data))) })`.
  implication: Every keystroke xterm.js produces is routed through `sanitizeForPty` before being written to the broker → PTY. Arrows and backspace are mutilated here.

- timestamp: 2026-06-05T00:00:00Z
  checked: Live regex test on xterm-emitted byte sequences.
  found:
    Up arrow `\x1b[A` (bytes 1b 5b 41) → `[A` (bytes 5b 41)
    Backspace `\x7f` → `` (empty)
    Ctrl+C `\x03` → `` (empty)
    Tab `\t` → `\t` (preserved)
    CR / LF preserved
    Arrow sequence `\x1b[A\x1b[B\x1b[C\x1b[D` → `[A[B[C[D`
  implication: Exact match for the visible garbage the user reports. Ctrl+C (SIGINT) is also broken — Ctrl+D, Ctrl+Z, Ctrl+R, all C0 keys are stripped.

- timestamp: 2026-06-05T00:00:00Z
  checked: `frontend/src/components/AgentTile.tsx:67–75` comment + git history reference (B-10.1).
  found: The comment claims "we keep \\t, \\n, \\r (legitimate paste of multi-line text) and printable ASCII + valid UTF-8". But the regex implementation does NOT exclude ESC, despite ESC being the prefix of every interactive control sequence (arrows, function keys, Home/End, Alt+key, focus reports, paste-bracketing, mouse reports, …). The author conflated "output sanitisation" (where ESC must die) with "input forwarding to PTY" (where ESC must survive).
  implication: This is a category error — the wrong filter was applied at the input-forwarding chokepoint.

- timestamp: 2026-06-05T00:00:00Z
  checked: `src-tauri/src/sanitize.rs`
  found: A separate `sanitize()` exists in Rust (strips ANSI and C0/C1 for *display*). It is NOT applied to PTY writes — the broker forwards bytes to the child PTY raw. So the frontend filter is the SOLE chokepoint corrupting keystrokes.
  implication: The fix must live in `AgentTile.tsx`. Backend is not the culprit.

## Resolution

root_cause: |
  `sanitizeForPty()` in `frontend/src/components/AgentTile.tsx` (lines 72–75) strips ESC (0x1B) and DEL (0x7F) from every byte sequence that xterm.js sends to the PTY:

  ```ts
  const CTRL_CHARS = /[\x00-\x08\x0B-\x0C\x0E-\x1F\x7F]/g;
  function sanitizeForPty(s: string): string {
    return s.replace(CTRL_CHARS, "");
  }
  ```

  Because xterm.js encodes:
    - ArrowUp / Down / Left / Right  as  `ESC [ A | B | C | D`
    - Backspace                       as  `0x7F` (or `0x08`, both stripped)
    - Ctrl+letter, Ctrl+C, Ctrl+D, Ctrl+Z  as raw C0 bytes (0x00–0x1F)
    - Home/End/PgUp/PgDn, F1–F12, Alt+key, paste-bracketing, focus reports  as ESC-prefixed sequences

  …every one of those is either deleted entirely or has its `ESC` prefix removed, leaving the printable tail (`[A`, `[B`, …) to be sent to the PTY as literal text. The CLI running inside the PTY sees `[`, `A` and inserts them into its line buffer. The original purpose of the filter — preventing "garbage" from spamming the IPC channel — was based on a misreading of what arrives via `term.onData`: that callback only fires for legitimate user input, all of which an interactive terminal MUST be allowed to forward verbatim.

fix: |
  Remove the `sanitizeForPty` call from the `term.onData` handler. xterm.js only emits bytes that correspond to real user keystrokes — there is no "garbage" to filter on the input side. (If a defence-in-depth chokepoint is wanted later, restrict the filter to NUL `\x00` only, which is the single byte a healthy PTY never wants and the only one with no legitimate input meaning.)

  Minimal diff in `frontend/src/components/AgentTile.tsx`:

  ```ts
  // BEFORE (line 153-157):
  const onDataDisp = term.onData((data) => {
    ipc.writeToAgent(agent.id, toB64(sanitizeForPty(data))).catch((err) => {
      console.error("write_to_agent failed", err);
    });
  });

  // AFTER:
  const onDataDisp = term.onData((data) => {
    ipc.writeToAgent(agent.id, toB64(data)).catch((err) => {
      console.error("write_to_agent failed", err);
    });
  });
  ```

  Also remove (or repurpose) the two paste sites at lines 419 and 467 that wrap user-pasted clipboard / drag-drop payloads in `sanitizeForPty`. Those are clipboard text (the CTRL_CHARS regex was probably fine there) but it would be cleaner to either:
    a) leave clipboard sanitisation as-is (only paste is sanitised, keystrokes are raw), or
    b) delete `sanitizeForPty` and `CTRL_CHARS` entirely (lines 67–75) and let pasted text flow through unchanged — pasting an ESC into a terminal is a legitimate operation.

  Recommended: option (a). Smaller blast radius and preserves the original B-10.1 intent for clipboard-only paths.

verification: |
  To verify after fix:
    1. Spawn an agent (`kiro-cli` or `claude`).
    2. Focus the tile, type some characters.
    3. Press ArrowLeft / ArrowRight — the CLI line editor moves the cursor.
    4. Press ArrowUp / ArrowDown — the CLI walks shell/prompt history.
    5. Press Backspace — the last character is deleted.
    6. Press Ctrl+C — running command receives SIGINT.
    7. Press Tab — autocomplete still works (unchanged: tab was preserved by the broken filter too).

files_changed:
  - frontend/src/components/AgentTile.tsx (lines 72–75 and 153–157 minimum; optionally drop the helper entirely if paste sites are also reverted)
