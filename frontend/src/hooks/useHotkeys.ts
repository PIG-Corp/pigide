import { useEffect, useRef } from "react";

export type HotkeyHandler = (e: KeyboardEvent) => void;
export type HotkeyMap = Record<string, HotkeyHandler>;

interface ParsedHotkey {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
  key: string;
}

const isMac = (() => {
  if (typeof navigator === "undefined") return false;
  return /Mac|iPhone|iPad|iPod/i.test(navigator.platform);
})();

function parseHotkey(spec: string): ParsedHotkey {
  const tokens = spec.toLowerCase().split("+").map((t) => t.trim()).filter(Boolean);
  const parsed: ParsedHotkey = {
    ctrl: false,
    alt: false,
    shift: false,
    meta: false,
    key: "",
  };
  if (tokens.length === 0) return parsed;
  const key = tokens[tokens.length - 1];
  parsed.key = key;
  for (let i = 0; i < tokens.length - 1; i += 1) {
    const m = tokens[i];
    if (m === "ctrl") {
      // On macOS map ctrl -> meta (Cmd) so Ctrl+T equals Cmd+T.
      if (isMac) parsed.meta = true;
      else parsed.ctrl = true;
    } else if (m === "alt" || m === "option") {
      parsed.alt = true;
    } else if (m === "shift") {
      parsed.shift = true;
    } else if (m === "meta" || m === "cmd" || m === "command" || m === "win") {
      parsed.meta = true;
    }
  }
  return parsed;
}

function eventKeyMatches(e: KeyboardEvent, key: string): boolean {
  const ek = e.key.toLowerCase();
  if (ek === key) return true;
  // Map common synonyms.
  if (key === "esc" && ek === "escape") return true;
  if (key === "escape" && ek === "esc") return true;
  if (key === "space" && ek === " ") return true;
  if (key === "plus" && ek === "+") return true;
  if (key === "minus" && ek === "-") return true;
  return false;
}

function matches(e: KeyboardEvent, h: ParsedHotkey): boolean {
  if (e.ctrlKey !== h.ctrl) return false;
  if (e.altKey !== h.alt) return false;
  if (e.shiftKey !== h.shift) return false;
  if (e.metaKey !== h.meta) return false;
  return eventKeyMatches(e, h.key);
}

function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return true;
  if (target.isContentEditable) return true;
  return false;
}

export function useHotkeys(map: HotkeyMap): void {
  // U-91 / H-33: keep a ref to the latest map so the listener closure always
  // sees current handlers without needing to re-attach on every render.
  const mapRef = useRef(map);
  mapRef.current = map;

  // Stable dependency: only re-attach when the set of bound keys changes.
  const keysSignature = Object.keys(map).sort().join(",");

  useEffect(() => {
    const compiled: { spec: string; parsed: ParsedHotkey }[] = [];
    for (const spec of Object.keys(mapRef.current)) {
      compiled.push({ spec, parsed: parseHotkey(spec) });
    }

    const onKeyDown = (e: KeyboardEvent) => {
      const editable = isEditableTarget(e.target);
      for (const c of compiled) {
        if (!matches(e, c.parsed)) continue;
        // Skip when typing in an editable element unless the hotkey requires Shift
        // (we use Shift as the "force-fire-even-in-input" convention).
        if (editable && !c.parsed.shift) continue;
        const handler = mapRef.current[c.spec];
        if (!handler) continue;
        e.preventDefault();
        e.stopPropagation();
        handler(e);
        return;
      }
    };

    document.body.addEventListener("keydown", onKeyDown);
    return () => {
      document.body.removeEventListener("keydown", onKeyDown);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [keysSignature]);
}
