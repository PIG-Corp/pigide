import { useEffect, useSyncExternalStore } from "react";
import { applyThemeToDom, DEFAULT_THEME_ID, getTheme, type Theme } from "./catalog";
import { ipc } from "../state/ipc";

const SETTING_KEY = "ui.theme_id";

type Listener = () => void;
const listeners = new Set<Listener>();
let currentId: string = DEFAULT_THEME_ID;

function emit() {
  for (const l of listeners) l();
}

function subscribe(l: Listener): () => void {
  listeners.add(l);
  return () => listeners.delete(l);
}

function getSnapshot(): string {
  return currentId;
}

/** Public hook used by tiles, panels, picker. */
export function useTheme(): { theme: Theme; setTheme: (id: string) => void } {
  const id = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  return {
    theme: getTheme(id),
    setTheme: (next: string) => {
      if (next === currentId) return;
      currentId = next;
      const t = getTheme(next);
      applyThemeToDom(t);
      ipc.setSetting(SETTING_KEY, t.id).catch(() => undefined);
      emit();
    },
  };
}

/**
 * Mount-once initialiser — read the persisted theme, apply CSS vars and
 * notify subscribers. Safe to call from <App>'s top useEffect.
 */
export function useThemeBootstrap(): void {
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const stored = await ipc.getSetting(SETTING_KEY);
        if (cancelled) return;
        const id = stored?.trim() ? stored : DEFAULT_THEME_ID;
        currentId = id;
        applyThemeToDom(getTheme(id));
        emit();
      } catch {
        applyThemeToDom(getTheme(DEFAULT_THEME_ID));
        emit();
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);
}
