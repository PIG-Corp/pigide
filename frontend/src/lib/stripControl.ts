/// Strip terminal / TTY control sequences that may leak into a plain-text
/// input field. The chat composer is a `<textarea>` inside the same WebView
/// as several xterm.js instances; under certain conditions (xterm focus
/// reports, ghostty/WebView2 keystroke echo, IME composition races, deep
/// links forwarded from external apps) bytes that belong in the PTY end up
/// in the textarea's `value` — most commonly:
///
///   - `ESC[?62;4;9;22c`  (DECRQM mode report)
///   - `ESC[I` / `ESC[O`  (xterm focus-in / focus-out reports)
///   - `ESC[A` / `ESC[B`  (arrow keys when xterm flushes buffered input on blur)
///   - `ESC[Z`            (Shift-Tab)
///
/// We never want any of those landing in a chat message. This module is the
/// single chokepoint: the chat textareas, the voice transcript injector and
/// the deep-link appender all route their input through `stripControl`.
/// Pure / dependency-free, safe to import from store + components.

/**
 * Char-level scan that removes terminal control sequences. We avoid a
 * single composite regex because alternation + greedy quantifiers in
 * `String.prototype.replace` can drop matches on overlapping input
 * (catastrophic backtracking in V8's regex engine). The explicit loop is
 * O(n) and has no such edge cases.
 *
 * Sequences handled:
 *   - CSI    `\x1b[`  params (0x30–0x3F)  intermediates (0x20–0x2F)
 *            final (0x40–0x7E)
 *   - OSC    `\x1b]`  text               terminated by BEL or ST
 *   - DCS / SOS / PM / APC
 *           `\x1bP` / `\x1bX` / `\x1b^` / `\x1b_`  text  terminated by ST
 *   - SS3    `\x1bO`  exactly one byte (F1-F4, arrow keys)
 *   - ESC + single C0 control byte
 *   - Lone C0 control bytes (we keep TAB, LF, CR — the only C0s the
 *     user can plausibly type or paste intentionally)
 */
export function stripControl(s: string): string {
  if (!s) return s;
  let out = "";
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    // ESC — start of an escape sequence.
    if (c === 0x1b && i + 1 < s.length) {
      const next = s.charCodeAt(i + 1);
      // CSI
      if (next === 0x5b /* [ */) {
        i += 2;
        let advanced = false;
        while (i < s.length) {
          const cc = s.charCodeAt(i);
          // params: 0x30-0x3F (digits, ; : < = > ?)
          if (cc >= 0x30 && cc <= 0x3f) {
            i++;
            continue;
          }
          // intermediates: 0x20-0x2F (space ! " # $ % & ' ( ) * + , - . /)
          if (cc >= 0x20 && cc <= 0x2f) {
            i++;
            continue;
          }
          // final: 0x40-0x7E
          if (cc >= 0x40 && cc <= 0x7e) {
            i++; // consume the final byte
            advanced = true;
            break;
          }
          // Anything else terminates the CSI early (malformed).
          break;
        }
        // for-loop will do i++ next. We already consumed the final byte, so
        // step back one to avoid skipping the char immediately after.
        if (advanced) i--;
        continue;
      }
      // OSC
      if (next === 0x5d /* ] */) {
        i += 2;
        let terminated = false;
        while (i < s.length) {
          const cc = s.charCodeAt(i);
          if (cc === 0x07 /* BEL */) {
            terminated = true;
            i++;
            break;
          }
          if (cc === 0x1b && i + 1 < s.length && s.charCodeAt(i + 1) === 0x5c /* \ */) {
            i += 2;
            terminated = true;
            break;
          }
          i++;
        }
        if (terminated) i--;
        continue;
      }
      // DCS / SOS / PM / APC
      if (next === 0x50 /* P */ || next === 0x58 /* X */ || next === 0x5e /* ^ */ || next === 0x5f /* _ */) {
        i += 2;
        let terminated = false;
        while (i < s.length) {
          const cc = s.charCodeAt(i);
          if (cc === 0x1b && i + 1 < s.length && s.charCodeAt(i + 1) === 0x5c) {
            i += 2;
            terminated = true;
            break;
          }
          i++;
        }
        if (terminated) i--;
        continue;
      }
      // SS3 — single byte follows.
      if (next === 0x4f /* O */) {
        // Consume ESC + O + one trailing byte. for-loop i++ lands on the
        // char after that byte.
        i += 2;
        continue;
      }
      // ESC + single C0 control byte.
      if (next < 0x20 || next === 0x7f) {
        // Consume ESC + C0; for-loop i++ lands on the char after C0.
        i++;
        continue;
      }
      // Unrecognised ESC sequence: drop the ESC, leave the next char alone
      // (we don't want to silently eat legitimate printable bytes).
      continue;
    }
    // Lone C0 controls (we keep TAB / LF / CR).
    if (c < 0x20 && c !== 0x09 && c !== 0x0a && c !== 0x0d) continue;
    if (c === 0x7f) continue;
    out += s[i];
  }
  return out;
}

/**
 * Like {@link stripControl} but also collapses runs of spaces introduced
 * by the removal and trims leading/trailing whitespace. Useful for
 * program-generated inputs (voice transcript, deep-link `chat` route)
 * where stray terminal residue would otherwise produce ugly double spaces.
 */
export function stripControlAndCollapse(s: string): string {
  return stripControl(s).replace(/[ \t]{2,}/g, " ").trim();
}
