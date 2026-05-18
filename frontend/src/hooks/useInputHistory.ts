import { useCallback, useRef } from "react";

const STORAGE_KEY = "pigide-architect-chat-history";
const MAX_HISTORY = 100;

function load(): string[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const arr = JSON.parse(raw);
    if (Array.isArray(arr)) return arr.slice(-MAX_HISTORY);
  } catch {}
  return [];
}

function save(history: string[]) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(history.slice(-MAX_HISTORY)));
  } catch {}
}

export function useInputHistory(
  value: string,
  setValue: (v: string) => void,
) {
  const historyRef = useRef<string[]>(load());
  const indexRef = useRef(-1);
  const savedDraftRef = useRef("");

  const push = useCallback((text: string) => {
    const trimmed = text.trim();
    if (!trimmed) return;
    const h = historyRef.current;
    if (h[h.length - 1] === trimmed) {
      indexRef.current = -1;
      return;
    }
    h.push(trimmed);
    if (h.length > MAX_HISTORY) h.shift();
    save(h);
    indexRef.current = -1;
  }, []);

  const handleKey = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      const ta = e.currentTarget;
      const h = historyRef.current;
      if (h.length === 0) return false;

      if (e.key === "ArrowUp") {
        const beforeCursor = ta.value.slice(0, ta.selectionStart);
        if (beforeCursor.includes("\n")) return false;

        e.preventDefault();
        if (indexRef.current === -1) {
          savedDraftRef.current = value;
          indexRef.current = h.length - 1;
        } else if (indexRef.current > 0) {
          indexRef.current--;
        } else {
          return true;
        }
        const text = h[indexRef.current];
        setValue(text);
        requestAnimationFrame(() => {
          ta.setSelectionRange(text.length, text.length);
        });
        return true;
      }

      if (e.key === "ArrowDown") {
        if (indexRef.current === -1) return false;
        const afterCursor = ta.value.slice(ta.selectionEnd);
        if (afterCursor.includes("\n")) return false;

        e.preventDefault();
        if (indexRef.current < h.length - 1) {
          indexRef.current++;
          const text = h[indexRef.current];
          setValue(text);
          requestAnimationFrame(() => {
            ta.setSelectionRange(text.length, text.length);
          });
        } else {
          indexRef.current = -1;
          const text = savedDraftRef.current;
          setValue(text);
          requestAnimationFrame(() => {
            ta.setSelectionRange(text.length, text.length);
          });
        }
        return true;
      }

      return false;
    },
    [value, setValue],
  );

  return { push, handleKey };
}
