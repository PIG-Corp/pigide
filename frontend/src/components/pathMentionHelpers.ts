/// Pure helpers for {@link PathMentionTextarea}. Kept dependency-free
/// (no React, no `@tauri-apps/*`) so they're trivially unit-testable in
/// node + tsc without a DOM.
///
/// Token format used inside the textarea is plain text: `@[label]`. The
/// regex below is the single source of truth for that shape — both the
/// React component and the tests import it from here.

import type { PathAttachment } from "../state/types";

/// Match every `@[label]` token in the input. Stateful (`g`) — callers
/// must reset `lastIndex = 0` before each scan.
export const TOKEN_RE = /@\[([^\]\n]+)\]/g;

/**
 * Find an active `@`-trigger immediately to the left of the caret.
 * Returns null if the caret isn't inside an open trigger. A trigger is:
 *   - `@` at start, after whitespace, or after `,`
 *   - followed by zero or more non-whitespace characters
 *   - the caret must NOT have crossed a closing `]` or whitespace.
 */
export function findTrigger(
  value: string,
  caret: number,
): { start: number; query: string } | null {
  if (caret === 0) return null;
  for (let i = caret - 1; i >= 0 && i >= caret - 256; i--) {
    const ch = value[i];
    if (ch === "@") {
      const prev = i > 0 ? value[i - 1] : "";
      // Word boundary: start of buffer, whitespace, or comma.
      if (
        prev !== "" &&
        prev !== " " &&
        prev !== "\n" &&
        prev !== "\t" &&
        prev !== ","
      ) {
        return null;
      }
      // If the candidate `@` is the leading char of an existing token
      // `@[...`, the next char will be `[`. We treat `@[` as a finalised
      // chip (not an open trigger).
      if (value[i + 1] === "[") return null;
      return { start: i, query: value.slice(i + 1, caret) };
    }
    if (ch === " " || ch === "\n" || ch === "\t" || ch === "]") return null;
  }
  return null;
}

/// Walk every `@[label]` token in `text` and return an Attachment list
/// whose `label`s match (in order). Tokens that don't appear in `pool`
/// are dropped silently — they were typed manually or remain from a
/// stale state. Order-preserving and dedup-aware: if the same attachment
/// label appears N times, only the first matching pool entry is used.
export function reconcileAttachments(
  text: string,
  pool: PathAttachment[],
): PathAttachment[] {
  const matches: string[] = [];
  let m: RegExpExecArray | null;
  TOKEN_RE.lastIndex = 0;
  while ((m = TOKEN_RE.exec(text)) !== null) {
    matches.push(m[1]);
  }
  const out: PathAttachment[] = [];
  for (const label of matches) {
    const found = pool.find((a) => a.label === label && !out.includes(a));
    if (found) out.push(found);
  }
  return out;
}

/// On backspace at offset `caret`, find the `@[…]` token whose closing
/// `]` is the char immediately to the left and return its `[start, end)`
/// (so the caller can `value.slice(0,start) + value.slice(end)`). Returns
/// null when the caret isn't sitting at a token boundary.
export function tokenLeftOfCaret(
  value: string,
  caret: number,
): { start: number; end: number } | null {
  if (caret === 0) return null;
  if (value[caret - 1] !== "]") return null;
  // Walk back to find the matching `@[`.
  for (let i = caret - 2; i >= 0 && i >= caret - 256; i--) {
    if (value[i] === "@" && value[i + 1] === "[") {
      return { start: i, end: caret };
    }
    if (value[i] === "@" || value[i] === "\n") return null;
  }
  return null;
}

/// Decide whether a query string should hit the backend path suggester.
/// We always send to backend so file/dir suggestions work for ANY query,
/// but we also surface agents/tasks for short single-word queries that
/// don't look like paths (no `/`, no `~`, no leading `.`).
export function isPathLike(q: string): boolean {
  return q.includes("/") || q.startsWith("~") || q.startsWith("./") || q.startsWith(".");
}

/// Ensure the chip label is unique within the message — if the user
/// attaches two different files that share a basename, append `#2`, `#3`,
/// etc. The full absolute path lives in the attachment record either way.
export function uniqueLabel(label: string, existing: PathAttachment[]): string {
  const taken = new Set(existing.map((a) => a.label));
  if (!taken.has(label)) return label;
  let i = 2;
  while (taken.has(`${label}#${i}`)) i++;
  return `${label}#${i}`;
}
