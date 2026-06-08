import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useStore } from "../state/store";
import { ipc } from "../state/ipc";
import type { Agent, Task } from "../state/types";
import { stripControl } from "../lib/stripControl";

/**
 * MentionTextarea — drop-in textarea with `@…` autocomplete (BridgeSpace gap
 * #25). Triggers on the `@` character; suggests:
 *   • agents (current workspace) — inserts `@agent:<id>` (Kiro tag)
 *   • tasks  (current workspace) — inserts `@task:<id>`
 *
 * Selection collapses with Tab or Enter; Esc closes the popover. The
 * cosmetic prefix in the visible text is the `@<short>` form so messages
 * stay readable.
 */
export type MentionTextareaProps = {
  value: string;
  onChange: (next: string) => void;
  onSubmit?: () => void;
  placeholder?: string;
  rows?: number;
  className?: string;
  ariaLabel?: string;
};

export type MentionTextareaHandle = {
  focus: () => void;
  textarea: () => HTMLTextAreaElement | null;
};

type Suggestion = {
  kind: "agent" | "task";
  id: string;
  label: string;
  detail: string;
};

const MAX_SUGGESTIONS = 8;

function buildAgentLabel(a: Agent): string {
  const short = a.id.slice(0, 8);
  return `${a.agent_type}#${short}`;
}

function buildTaskLabel(t: Task): string {
  return t.title || t.id.slice(0, 8);
}

function findTrigger(value: string, caret: number): { start: number; query: string } | null {
  // Walk backwards from caret to find a recent `@` that is at the start of
  // the buffer or follows whitespace, with no whitespace between it and the
  // caret.
  if (caret === 0) return null;
  for (let i = caret - 1; i >= 0 && i >= caret - 64; i--) {
    const ch = value[i];
    if (ch === "@") {
      if (i > 0) {
        const prev = value[i - 1];
        if (prev !== " " && prev !== "\n" && prev !== "\t") return null;
      }
      return { start: i, query: value.slice(i + 1, caret) };
    }
    if (ch === " " || ch === "\n" || ch === "\t") return null;
  }
  return null;
}

export const MentionTextarea = forwardRef<MentionTextareaHandle, MentionTextareaProps>(
  function MentionTextarea(
    { value, onChange, onSubmit, placeholder, rows = 1, className, ariaLabel },
    ref,
  ) {
    const taRef = useRef<HTMLTextAreaElement | null>(null);
    const currentId = useStore((s) => s.currentId);
    const agents = useStore((s) => s.agents);

    const [trigger, setTrigger] = useState<{ start: number; query: string } | null>(null);
    const [tasks, setTasks] = useState<Task[]>([]);
    const [activeIdx, setActiveIdx] = useState(0);
    // B-8.2: stable id prefix for the listbox + option ids so the
    // textarea can advertise which option is "active" to screen readers.
    const listboxId = `mtn-listbox`;

    useImperativeHandle(ref, () => ({
      focus: () => taRef.current?.focus(),
      textarea: () => taRef.current,
    }));

    useLayoutEffect(() => {
      const ta = taRef.current;
      if (!ta) return;
      ta.style.height = "auto";
      ta.style.height = `${Math.min(ta.scrollHeight, 200)}px`;
    }, [value]);

    // Lazily fetch tasks the first time a mention pop appears.
    useEffect(() => {
      if (!trigger || !currentId) return;
      let cancelled = false;
      ipc
        .listTasks({ workspace_id: currentId })
        .then((rows) => {
          if (!cancelled) setTasks(rows);
        })
        .catch(() => undefined);
      return () => {
        cancelled = true;
      };
    }, [trigger, currentId]);

    const suggestions = useMemo<Suggestion[]>(() => {
      if (!trigger) return [];
      const q = trigger.query.toLowerCase();
      const agentList = Object.values(agents);
      const acc: Suggestion[] = [];
      for (const a of agentList) {
        const label = buildAgentLabel(a);
        if (!q || label.toLowerCase().includes(q) || a.id.toLowerCase().includes(q)) {
          acc.push({
            kind: "agent",
            id: a.id,
            label,
            detail: a.cwd ?? a.status,
          });
        }
      }
      for (const t of tasks) {
        const label = buildTaskLabel(t);
        if (!q || label.toLowerCase().includes(q) || t.id.toLowerCase().includes(q)) {
          acc.push({
            kind: "task",
            id: t.id,
            label,
            detail: t.status,
          });
        }
      }
      return acc.slice(0, MAX_SUGGESTIONS);
    }, [trigger, agents, tasks]);

    useEffect(() => {
      setActiveIdx(0);
    }, [suggestions.length]);

    const insert = (s: Suggestion) => {
      const ta = taRef.current;
      if (!ta || !trigger) return;
      const before = value.slice(0, trigger.start);
      const after = value.slice(trigger.start + 1 + trigger.query.length);
      const tag = s.kind === "agent" ? `@agent:${s.id.slice(0, 8)}` : `@task:${s.id.slice(0, 8)}`;
      const next = `${before}${tag} ${after}`;
      onChange(next);
      setTrigger(null);
      requestAnimationFrame(() => {
        const pos = (before + tag + " ").length;
        ta.setSelectionRange(pos, pos);
        ta.focus();
      });
    };

    const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      if (trigger && suggestions.length > 0) {
        if (e.key === "ArrowDown") {
          e.preventDefault();
          setActiveIdx((i) => (i + 1) % suggestions.length);
          return;
        }
        if (e.key === "ArrowUp") {
          e.preventDefault();
          setActiveIdx((i) => (i - 1 + suggestions.length) % suggestions.length);
          return;
        }
        if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey)) {
          e.preventDefault();
          insert(suggestions[activeIdx]);
          return;
        }
        if (e.key === "Escape") {
          e.preventDefault();
          setTrigger(null);
          return;
        }
      }
      if (e.key === "Enter" && !e.shiftKey) {
        if (!trigger && onSubmit) {
          e.preventDefault();
          onSubmit();
        }
      }
    };

    const onChangeInternal = (e: React.ChangeEvent<HTMLTextAreaElement>) => {
      // Strip stray terminal control sequences (xterm focus reports,
      // DECRQM mode reports, buffered arrow / Shift-Tab bytes) that can
      // land in the textarea when focus moves between tiles under Tauri
      // WebView2. The caret may shift if leading bytes were dropped; if
      // so, restore the caret to the new end-of-prefix position.
      const raw = e.target.value;
      const clean = stripControl(raw);
      if (clean !== raw) {
        const caret = e.target.selectionStart ?? raw.length;
        const delta = raw.length - clean.length;
        e.target.value = clean;
        const nextCaret = Math.max(0, caret - delta);
        e.target.setSelectionRange(nextCaret, nextCaret);
      }
      onChange(clean);
      const caret = e.target.selectionStart;
      setTrigger(findTrigger(clean, caret));
    };

    return (
      <div className="mention-wrap">
        <textarea
          ref={taRef}
          className={className}
          aria-label={ariaLabel}
          placeholder={placeholder}
          value={value}
          onChange={onChangeInternal}
          onKeyDown={onKeyDown}
          onClick={(e) => {
            const ta = e.currentTarget;
            setTrigger(findTrigger(ta.value, ta.selectionStart));
          }}
          // B-8.2: ARIA combobox wiring. The textarea acts as the
          // combobox input; the listbox below it is the popup. Screen
          // readers announce the option pointed to by `aria-activedescendant`.
          role="combobox"
          aria-autocomplete="list"
          aria-expanded={trigger !== null && suggestions.length > 0}
          aria-controls={trigger !== null && suggestions.length > 0 ? listboxId : undefined}
          aria-activedescendant={
            trigger !== null && suggestions.length > 0
              ? `${listboxId}-${activeIdx}`
              : undefined
          }
          rows={rows}
        />
        {trigger && suggestions.length > 0 && (
          <div className="mention-pop" id={listboxId} role="listbox">
            {suggestions.map((s, i) => (
              <button
                key={`${s.kind}:${s.id}`}
                id={`${listboxId}-${i}`}
                role="option"
                aria-selected={i === activeIdx}
                className={`mention-item ${i === activeIdx ? "active" : ""}`}
                onMouseDown={(e) => {
                  e.preventDefault();
                  insert(s);
                }}
                onMouseEnter={() => setActiveIdx(i)}
              >
                <span className={`mention-kind ${s.kind}`}>{s.kind}</span>
                <span className="mention-label">{s.label}</span>
                <span className="mention-detail">{s.detail}</span>
              </button>
            ))}
          </div>
        )}
      </div>
    );
  },
);
