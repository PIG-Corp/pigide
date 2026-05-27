// Virtualised, debounced list of NoteSummary cards. Rendered as the sidebar
// of the PigMemory workbench. Supports search highlighting, tag chips,
// active state, keyboard nav.

import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
} from "react";

interface ItemRow {
  id: string;
  slug: string;
  title: string;
  kind?: import("../../state/types").NoteKind;
  tags?: string[];
  snippet?: string;
  updatedAt?: string;
}

const ROW_HEIGHT = 64;
const OVERSCAN = 6;

export interface NoteListHandle {
  scrollToId: (id: string) => void;
}

export const NoteList = forwardRef<
  NoteListHandle,
  {
    items: ItemRow[];
    activeId: string | null;
    onSelect: (id: string) => void;
    emptyMessage?: string;
    showSnippet?: boolean;
  }
>(function NoteList(
  { items, activeId, onSelect, emptyMessage, showSnippet },
  ref,
) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [viewport, setViewport] = useState({ height: 0, scrollTop: 0 });

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      setViewport((v) => ({ ...v, height: el.clientHeight }));
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  useImperativeHandle(ref, () => ({
    scrollToId(id: string) {
      const idx = items.findIndex((it) => it.id === id);
      if (idx < 0) return;
      const el = containerRef.current;
      if (!el) return;
      const top = idx * ROW_HEIGHT;
      const bot = top + ROW_HEIGHT;
      if (top < el.scrollTop) {
        el.scrollTop = top;
      } else if (bot > el.scrollTop + el.clientHeight) {
        el.scrollTop = bot - el.clientHeight;
      }
    },
  }));

  const totalHeight = items.length * ROW_HEIGHT;
  const startIdx = Math.max(
    0,
    Math.floor(viewport.scrollTop / ROW_HEIGHT) - OVERSCAN,
  );
  const endIdx = Math.min(
    items.length,
    Math.ceil((viewport.scrollTop + viewport.height) / ROW_HEIGHT) + OVERSCAN,
  );
  const slice = useMemo(
    () => items.slice(startIdx, endIdx),
    [items, startIdx, endIdx],
  );

  const onKeyDown = (e: React.KeyboardEvent, id: string, idx: number) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onSelect(id);
      return;
    }
    if (e.key === "ArrowDown" && startIdx + idx + 1 < items.length) {
      e.preventDefault();
      onSelect(items[startIdx + idx + 1].id);
    }
    if (e.key === "ArrowUp" && startIdx + idx - 1 >= 0) {
      e.preventDefault();
      onSelect(items[startIdx + idx - 1].id);
    }
  };

  if (items.length === 0) {
    return (
      <div className="pigmem-list pigmem-list--empty">
        <div className="pigmem-empty">{emptyMessage ?? "No notes"}</div>
      </div>
    );
  }

  return (
    <div
      ref={containerRef}
      className="pigmem-list"
      onScroll={(e) =>
        setViewport((v) => ({ ...v, scrollTop: e.currentTarget.scrollTop }))
      }
    >
      <div style={{ height: totalHeight, position: "relative" }}>
        {slice.map((it, i) => {
          const realIdx = startIdx + i;
          const isActive = it.id === activeId;
          return (
            <div
              key={it.id}
              role="button"
              tabIndex={0}
              aria-current={isActive ? "true" : undefined}
              onClick={() => onSelect(it.id)}
              onKeyDown={(e) => onKeyDown(e, it.id, i)}
              className={`pigmem-row ${isActive ? "pigmem-row--active" : ""}`}
              style={{
                position: "absolute",
                top: realIdx * ROW_HEIGHT,
                left: 0,
                right: 0,
                height: ROW_HEIGHT,
              }}
            >
              <div className="pigmem-row-title">
                {it.kind ? (
                  <span
                    className={`pigmem-row-kind-dot pigmem-row-kind-dot--${it.kind}`}
                    aria-label={`${it.kind} note`}
                    title={it.kind}
                  />
                ) : null}
                {it.title || it.slug}
              </div>
              {showSnippet && it.snippet ? (
                <div
                  className="pigmem-row-snippet"
                  dangerouslySetInnerHTML={{ __html: highlightSnippet(it.snippet) }}
                />
              ) : (
                <div className="pigmem-row-meta">
                  {it.tags && it.tags.length > 0 ? (
                    <div className="pigmem-row-tags">
                      {it.tags.slice(0, 3).map((t) => (
                        <span key={t} className="pigmem-tag-chip">
                          #{t}
                        </span>
                      ))}
                      {it.tags.length > 3 ? (
                        <span className="pigmem-tag-chip pigmem-tag-chip--muted">
                          +{it.tags.length - 3}
                        </span>
                      ) : null}
                    </div>
                  ) : (
                    <span className="pigmem-row-slug">{it.slug}</span>
                  )}
                  {it.updatedAt ? (
                    <span className="pigmem-row-time">
                      {formatRelative(it.updatedAt)}
                    </span>
                  ) : null}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
});

function highlightSnippet(snippet: string): string {
  const escaped = snippet
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
  return escaped
    .replace(/&lt;&lt;/g, "<mark>")
    .replace(/&gt;&gt;/g, "</mark>");
}

function formatRelative(iso: string): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "";
  const diffMs = Date.now() - t;
  const sec = Math.floor(diffMs / 1000);
  if (sec < 60) return `${sec}s`;
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}m`;
  const h = Math.floor(min / 60);
  if (h < 24) return `${h}h`;
  const d = Math.floor(h / 24);
  if (d < 7) return `${d}d`;
  const w = Math.floor(d / 7);
  if (w < 4) return `${w}w`;
  return new Date(t).toLocaleDateString();
}
