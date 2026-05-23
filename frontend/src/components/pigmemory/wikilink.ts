// Wikilink + tag plumbing shared by editor, preview, and graph code.

import type { NoteSummary } from "../../state/types";

export interface WikiTarget {
  /** Raw `[[…]]` payload, including any `|alias`. */
  raw: string;
  /** Target before the `|`, with whitespace trimmed. */
  target: string;
  /** Optional display text from `[[target|display]]`. */
  display: string;
  /** Byte offsets in the source. */
  start: number;
  end: number;
}

export interface ParsedTag {
  raw: string;
  tag: string;
  start: number;
  end: number;
}

const WIKI_RE = /\[\[([^\]\n]+)\]\]/g;
const TAG_RE = /(^|\s)#([\p{L}\p{N}][\p{L}\p{N}_-]*)/gu;

export function extractWikilinks(body: string): WikiTarget[] {
  const out: WikiTarget[] = [];
  WIKI_RE.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = WIKI_RE.exec(body)) !== null) {
    const raw = m[0];
    const inner = m[1];
    const pipe = inner.indexOf("|");
    const target = (pipe >= 0 ? inner.slice(0, pipe) : inner).trim();
    const display = pipe >= 0 ? inner.slice(pipe + 1).trim() : target;
    if (!target) continue;
    out.push({
      raw,
      target,
      display,
      start: m.index,
      end: m.index + raw.length,
    });
  }
  return out;
}

export function extractTags(body: string): ParsedTag[] {
  const out: ParsedTag[] = [];
  TAG_RE.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = TAG_RE.exec(body)) !== null) {
    const lead = m[1] ?? "";
    const tag = m[2];
    if (!tag) continue;
    const start = m.index + lead.length;
    out.push({ raw: `#${tag}`, tag, start, end: start + tag.length + 1 });
  }
  return out;
}

/** Resolve a wikilink target to a NoteSummary using slug or title (case-insensitive). */
export function resolveWikilink(
  target: string,
  notes: NoteSummary[],
): NoteSummary | null {
  const t = target.trim().toLowerCase();
  if (!t) return null;
  for (const n of notes) {
    if (n.slug.toLowerCase() === t) return n;
  }
  for (const n of notes) {
    if (n.title.toLowerCase() === t) return n;
  }
  return null;
}

/** Detect whether the cursor is inside an unclosed `[[…` token. */
export function activeWikilinkPrefix(
  text: string,
  caret: number,
): { prefix: string; start: number } | null {
  if (caret < 2) return null;
  // Walk backwards looking for the most recent `[[`. Bail on newline / `]]`.
  let i = caret - 1;
  while (i >= 1) {
    const ch = text[i];
    if (ch === "\n") return null;
    if (text[i - 1] === "]" && ch === "]") return null;
    if (text[i - 1] === "[" && ch === "[") {
      const start = i - 1;
      const prefix = text.slice(start + 2, caret);
      // Reject if the prefix contains `]]` already (closed).
      if (prefix.includes("]]")) return null;
      return { prefix, start };
    }
    i -= 1;
  }
  return null;
}

/** Aggregate all distinct tags from a list of NoteSummary. */
export function aggregateTags(list: NoteSummary[]): { tag: string; count: number }[] {
  const counts = new Map<string, number>();
  for (const n of list) {
    for (const t of n.tags) {
      const tag = t.trim();
      if (!tag) continue;
      counts.set(tag, (counts.get(tag) ?? 0) + 1);
    }
  }
  return Array.from(counts, ([tag, count]) => ({ tag, count })).sort(
    (a, b) => b.count - a.count || a.tag.localeCompare(b.tag),
  );
}
