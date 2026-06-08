import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
} from "react";
import { ipc } from "../state/ipc";
import { useStore } from "../state/store";
import type {
  Agent,
  PathAttachment,
  PathSuggestion,
  Task,
} from "../state/types";
import {
  findTrigger,
  isPathLike,
  reconcileAttachments,
  TOKEN_RE,
  tokenLeftOfCaret,
  uniqueLabel,
} from "./pathMentionHelpers";
import { stripControl } from "../lib/stripControl";

/**
 * PathMentionTextarea — Architect chat input with `@`-mention autocomplete
 * for files, directories, agents, and tasks.
 *
 * Token format in the textarea is plain text: `@[label]`. Each chip
 * carries the resolved absolute path in the parallel `attachments` array,
 * keyed by exact label match. Backspace at the right edge of a token
 * deletes the entire `@[label]` block. The token is rendered as a styled
 * chip via a `div`-mirror that sits behind the textarea (transparent text).
 *
 * Emits `onAttachmentsChange` so the parent can keep the validated list
 * in sync with what's typed; sends `(text, attachments)` to the backend
 * on submit.
 */
export type PathMentionTextareaProps = {
  value: string;
  onChange: (next: string) => void;
  attachments: PathAttachment[];
  onAttachmentsChange: (next: PathAttachment[]) => void;
  onSubmit?: () => void;
  onKeyDown?: (e: React.KeyboardEvent<HTMLTextAreaElement>) => boolean | void;
  placeholder?: string;
  rows?: number;
  className?: string;
  ariaLabel?: string;
};

export type PathMentionTextareaHandle = {
  focus: () => void;
  textarea: () => HTMLTextAreaElement | null;
};

type Suggestion =
  | { kind: "agent"; id: string; label: string; detail: string }
  | { kind: "task"; id: string; label: string; detail: string }
  | { kind: "file"; path: string; label: string; detail: string }
  | { kind: "dir"; path: string; label: string; detail: string };

const MAX_SUGGESTIONS = 20;
const SUGGEST_DEBOUNCE_MS = 90;
const MAX_ATTACHMENTS = 16;

function buildAgentLabel(a: Agent): string {
  return `${a.agent_type}#${a.id.slice(0, 8)}`;
}

function buildTaskLabel(t: Task): string {
  return t.title || t.id.slice(0, 8);
}

export const PathMentionTextarea = forwardRef<
  PathMentionTextareaHandle,
  PathMentionTextareaProps
>(function PathMentionTextarea(
  {
    value,
    onChange,
    attachments,
    onAttachmentsChange,
    onSubmit,
    onKeyDown: onKeyDownExternal,
    placeholder,
    rows = 1,
    className,
    ariaLabel,
  },
  ref,
) {
  const taRef = useRef<HTMLTextAreaElement | null>(null);
  const mirrorRef = useRef<HTMLDivElement | null>(null);
  const currentId = useStore((s) => s.currentId);
  const agents = useStore((s) => s.agents);
  // B-1.6: monotonically increasing request id for suggestPaths — see useEffect below.
  const suggestReqIdRef = useRef(0);

  const [trigger, setTrigger] = useState<{ start: number; query: string } | null>(null);
  const [tasks, setTasks] = useState<Task[]>([]);
  const [pathRows, setPathRows] = useState<PathSuggestion[]>([]);
  const [activeIdx, setActiveIdx] = useState(0);
  const listboxId = useMemo(() => `pmt-list-${Math.random().toString(36).slice(2, 8)}`, []);

  useImperativeHandle(ref, () => ({
    focus: () => taRef.current?.focus(),
    textarea: () => taRef.current,
  }));

  // Lazy-load tasks the first time a trigger appears.
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

  // Debounced backend path suggestion. Only fires for non-empty triggers
  // whose query has at least 1 char OR ends with `/` (browse-the-dir UX).
  // B-1.6: tag every in-flight fetch with a request id and drop responses
  // whose id no longer matches the latest one — otherwise a slow backend
  // can let a stale `q` overwrite a fresher suggestion.
  useEffect(() => {
    if (!trigger) {
      setPathRows([]);
      return;
    }
    const q = trigger.query;
    if (q.length === 0) {
      setPathRows([]);
      return;
    }
    const reqId = ++suggestReqIdRef.current;
    const t = setTimeout(() => {
      ipc
        .suggestPaths(q, currentId ?? null)
        .then((rows) => {
          if (reqId !== suggestReqIdRef.current) return;
          setPathRows(rows);
        })
        .catch(() => {
          if (reqId !== suggestReqIdRef.current) return;
          setPathRows([]);
        });
    }, SUGGEST_DEBOUNCE_MS);
    return () => {
      clearTimeout(t);
    };
  }, [trigger, currentId]);

  const suggestions = useMemo<Suggestion[]>(() => {
    if (!trigger) return [];
    const q = trigger.query.toLowerCase();
    const acc: Suggestion[] = [];
    // Agents and tasks only matter for short, non-path queries.
    if (!isPathLike(trigger.query)) {
      for (const a of Object.values(agents) as Agent[]) {
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
    }
    for (const r of pathRows) {
      acc.push({
        kind: r.kind,
        path: r.path,
        label: r.label,
        detail: r.path,
      });
    }
    return acc.slice(0, MAX_SUGGESTIONS);
  }, [trigger, agents, tasks, pathRows]);

  useEffect(() => {
    setActiveIdx(0);
  }, [suggestions.length]);

  const insertSuggestion = useCallback(
    (s: Suggestion) => {
      const ta = taRef.current;
      if (!ta || !trigger) return;
      const before = value.slice(0, trigger.start);
      const after = value.slice(trigger.start + 1 + trigger.query.length);
      let token: string;
      let nextAttachments = attachments;
      if (s.kind === "agent") {
        token = `@agent:${s.id.slice(0, 8)}`;
      } else if (s.kind === "task") {
        token = `@task:${s.id.slice(0, 8)}`;
      } else {
        if (attachments.length >= MAX_ATTACHMENTS) {
          return;
        }
        const label = uniqueLabel(s.label, attachments);
        token = `@[${label}]`;
        nextAttachments = [
          ...attachments,
          { kind: s.kind, path: s.path, label },
        ];
      }
      const next = `${before}${token} ${after}`;
      onChange(next);
      if (nextAttachments !== attachments) {
        onAttachmentsChange(nextAttachments);
      }
      setTrigger(null);
      requestAnimationFrame(() => {
        const pos = (before + token + " ").length;
        ta.setSelectionRange(pos, pos);
        ta.focus();
      });
    },
    [trigger, value, attachments, onChange, onAttachmentsChange],
  );

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
        insertSuggestion(suggestions[activeIdx]);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setTrigger(null);
        return;
      }
    }
    if (onKeyDownExternal?.(e)) return;
    if (e.key === "Backspace" && !e.shiftKey && !e.metaKey && !e.ctrlKey) {
      const ta = taRef.current;
      if (ta && ta.selectionStart === ta.selectionEnd) {
        const found = tokenLeftOfCaret(value, ta.selectionStart);
        if (found) {
          e.preventDefault();
          const next = value.slice(0, found.start) + value.slice(found.end);
          onChange(next);
          onAttachmentsChange(reconcileAttachments(next, attachments));
          requestAnimationFrame(() => {
            ta.setSelectionRange(found.start, found.start);
            ta.focus();
          });
          return;
        }
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
    // WebView2. Drop only the leading prefix of dropped bytes; for
    // mid-string drops we re-derive the caret by character offset.
    const raw = e.target.value;
    const next = stripControl(raw);
    if (next !== raw) {
      e.target.value = next;
      const caret = e.target.selectionStart ?? raw.length;
      const cleanedBefore = stripControl(raw.slice(0, caret)).length;
      e.target.setSelectionRange(cleanedBefore, cleanedBefore);
    }
    onChange(next);
    onAttachmentsChange(reconcileAttachments(next, attachments));
    setTrigger(findTrigger(next, e.target.selectionStart));
  };

  const onClickInternal = (e: React.MouseEvent<HTMLTextAreaElement>) => {
    const ta = e.currentTarget;
    setTrigger(findTrigger(ta.value, ta.selectionStart));
  };

  // Scroll the chip overlay in lock-step with the textarea.
  const onScrollSync = () => {
    const ta = taRef.current;
    const m = mirrorRef.current;
    if (ta && m) {
      m.scrollTop = ta.scrollTop;
      m.scrollLeft = ta.scrollLeft;
    }
  };

  const open = trigger !== null && suggestions.length > 0;

  return (
    <div
      className="path-mention-wrap"
      role="combobox"
      aria-haspopup="listbox"
      aria-expanded={open}
      aria-owns={listboxId}
    >
      <div ref={mirrorRef} className="path-mention-mirror" aria-hidden="true">
        {renderMirror(value)}
      </div>
      <textarea
        ref={taRef}
        className={`path-mention-textarea ${className ?? ""}`}
        aria-label={ariaLabel}
        aria-autocomplete="list"
        aria-controls={listboxId}
        aria-activedescendant={
          open ? `${listboxId}-${activeIdx}` : undefined
        }
        placeholder={placeholder}
        value={value}
        onChange={onChangeInternal}
        onKeyDown={onKeyDown}
        onClick={onClickInternal}
        onScroll={onScrollSync}
        rows={rows}
        spellCheck={false}
      />
      {open && (
        <ul id={listboxId} className="path-mention-pop" role="listbox">
          {suggestions.map((s, i) => (
            <li
              key={`${s.kind}:${"id" in s ? s.id : s.path}`}
              id={`${listboxId}-${i}`}
              role="option"
              aria-selected={i === activeIdx}
              className={`path-mention-item ${i === activeIdx ? "active" : ""}`}
              onMouseDown={(e) => {
                e.preventDefault();
                insertSuggestion(s);
              }}
              onMouseEnter={() => setActiveIdx(i)}
            >
              <span className={`path-mention-kind ${s.kind}`}>{s.kind}</span>
              <span className="path-mention-label">{s.label}</span>
              <span className="path-mention-detail">{s.detail}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
});

/// Render the chip-aware mirror. The mirror's text content is identical to
/// the textarea's value modulo the styled `<span>` wrappers around each
/// `@[label]` token, so visual layout stays in lock-step.
function renderMirror(value: string): React.ReactNode[] {
  const out: React.ReactNode[] = [];
  let last = 0;
  TOKEN_RE.lastIndex = 0;
  let m: RegExpExecArray | null;
  let key = 0;
  while ((m = TOKEN_RE.exec(value)) !== null) {
    if (m.index > last) {
      out.push(value.slice(last, m.index));
    }
    out.push(
      <span key={`chip-${key++}`} className="path-mention-chip">
        @[<span className="path-mention-chip-label">{m[1]}</span>]
      </span>,
    );
    last = m.index + m[0].length;
  }
  if (last < value.length) {
    out.push(value.slice(last));
  }
  return out;
}
