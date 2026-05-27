// PigMemoryWorkbench — full-canvas tab. 3-pane layout (sidebar + main + inspector)
// with a header toolbar. Wires the entire PigMemory feature surface together.

import {
  useCallback,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
} from "react";
import { Allotment } from "allotment";
import { ipc, onMemoryNoteCreated } from "../../state/ipc";
import { useStore } from "../../state/store";
import type {
  Backlink,
  GraphData,
  MemoryStatus,
  Note,
  NoteSummary,
  SearchHit,
  TagSummary,
} from "../../state/types";
import {
  Plus,
  Trash2,
  X,
} from "../icons";
import { NoteList, type NoteListHandle } from "./NoteList";
import { PigMemoryEditor } from "./PigMemoryEditor";
import { MarkdownPreview } from "./MarkdownPreview";
import { PigMemoryGraph, type PigMemoryGraphHandle } from "./PigMemoryGraph";
import { ActivityTimeline, type ActivityEvent } from "./ActivityTimeline";
import { SmartStatusPill } from "./SmartStatusPill";
import { TagManager } from "./TagManager";
import { aggregateTags } from "./wikilink";

type SortMode = "updated" | "title" | "created";
type ViewMode = "edit" | "preview" | "split";

interface VisibleRow {
  id: string;
  slug: string;
  title: string;
  kind?: import("../../state/types").NoteKind;
  tags?: string[];
  snippet?: string;
  updatedAt?: string;
}

interface State {
  list: NoteSummary[];
  graph: GraphData | null;
  tagSummaries: TagSummary[];
  status: MemoryStatus | null;
  search: string;
  searchDeb: string;
  hits: SearchHit[] | null;
  tagFilter: string | null;
  sort: SortMode;
  activeId: string | null;
  active: Note | null;
  draftTitle: string;
  draftBody: string;
  draftTags: string;
  dirty: boolean;
  view: ViewMode;
  showGraphTab: boolean;
  showTagManager: boolean;
  backlinks: Backlink[];
  related: SearchHit[];
  loadingActive: boolean;
}

type Action =
  | { type: "list"; list: NoteSummary[] }
  | { type: "graph"; graph: GraphData }
  | { type: "tagSummaries"; tags: TagSummary[] }
  | { type: "status"; status: MemoryStatus }
  | { type: "search"; q: string }
  | { type: "searchDeb"; q: string }
  | { type: "hits"; hits: SearchHit[] | null }
  | { type: "tagFilter"; tag: string | null }
  | { type: "sort"; sort: SortMode }
  | { type: "activeId"; id: string | null }
  | { type: "active"; note: Note | null; backlinks?: Backlink[]; related?: SearchHit[] }
  | { type: "draftTitle"; v: string }
  | { type: "draftBody"; v: string }
  | { type: "draftTags"; v: string }
  | { type: "clean" }
  | { type: "view"; view: ViewMode }
  | { type: "graphTab"; v: boolean }
  | { type: "tagManager"; v: boolean }
  | { type: "loadingActive"; v: boolean };

const initialState: State = {
  list: [],
  graph: null,
  tagSummaries: [],
  status: null,
  search: "",
  searchDeb: "",
  hits: null,
  tagFilter: null,
  sort: "updated",
  activeId: null,
  active: null,
  draftTitle: "",
  draftBody: "",
  draftTags: "",
  dirty: false,
  view: "split",
  showGraphTab: false,
  showTagManager: false,
  backlinks: [],
  related: [],
  loadingActive: false,
};

function reducer(s: State, a: Action): State {
  switch (a.type) {
    case "list":
      return { ...s, list: a.list };
    case "graph":
      return { ...s, graph: a.graph };
    case "tagSummaries":
      return { ...s, tagSummaries: a.tags };
    case "status":
      return { ...s, status: a.status };
    case "search":
      return { ...s, search: a.q };
    case "searchDeb":
      return { ...s, searchDeb: a.q };
    case "hits":
      return { ...s, hits: a.hits };
    case "tagFilter":
      return { ...s, tagFilter: a.tag };
    case "sort":
      return { ...s, sort: a.sort };
    case "activeId":
      return { ...s, activeId: a.id };
    case "active":
      return {
        ...s,
        active: a.note,
        backlinks: a.backlinks ?? s.backlinks,
        related: a.related ?? s.related,
        draftTitle: a.note?.title ?? "",
        draftBody: a.note?.body ?? "",
        draftTags: a.note ? a.note.tags.join(", ") : "",
        dirty: false,
        loadingActive: false,
      };
    case "draftTitle":
      return { ...s, draftTitle: a.v, dirty: true };
    case "draftBody":
      return { ...s, draftBody: a.v, dirty: true };
    case "draftTags":
      return { ...s, draftTags: a.v, dirty: true };
    case "clean":
      return { ...s, dirty: false };
    case "view":
      return { ...s, view: a.view };
    case "graphTab":
      return { ...s, showGraphTab: a.v };
    case "tagManager":
      return { ...s, showTagManager: a.v };
    case "loadingActive":
      return { ...s, loadingActive: a.v };
  }
}

export function PigMemoryWorkbench() {
  const currentId = useStore((s) => s.currentId);
  const pushToast = useStore((s) => s.pushToast);
  const setShowPigMemory = useStore((s) => s.setShowPigMemory);

  const [s, dispatch] = useReducer(reducer, initialState);
  const listRef = useRef<NoteListHandle | null>(null);
  const graphRef = useRef<PigMemoryGraphHandle | null>(null);
  const saveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // ----- Loaders ----------------------------------------------------------
  const reloadList = useCallback(async () => {
    if (!currentId) {
      dispatch({ type: "list", list: [] });
      return;
    }
    try {
      const ms = await ipc.listMemories({ workspace_id: currentId, limit: 500 });
      dispatch({ type: "list", list: ms });
    } catch (err) {
      pushToast({ text: `list_memories: ${err}`, kind: "error" });
    }
  }, [currentId, pushToast]);

  const reloadGraph = useCallback(async () => {
    if (!currentId) {
      return;
    }
    try {
      const [g, tags, status] = await Promise.all([
        ipc.memoryGraph(currentId),
        ipc.memoryTags(currentId).catch(() => [] as TagSummary[]),
        ipc.memoryStatus(currentId).catch(() => null),
      ]);
      dispatch({ type: "graph", graph: g });
      dispatch({ type: "tagSummaries", tags });
      if (status) dispatch({ type: "status", status });
    } catch (err) {
      pushToast({ text: `memory_graph: ${err}`, kind: "error" });
    }
  }, [currentId, pushToast]);

  useEffect(() => {
    reloadList();
    reloadGraph();
  }, [reloadList, reloadGraph]);

  // ----- Live ingest events -----------------------------------------------
  // Track node IDs that just received a memory://note.created event for ~3s
  // so the graph can paint a glow halo + a brief pulse animation.
  const [recentNodeIds, setRecentNodeIds] = useState<Set<string>>(new Set());
  const recentTimersRef = useRef<Map<string, ReturnType<typeof setTimeout>>>(new Map());

  // Kind filter — null = "all". Same Set instance is passed to the graph
  // (for dimming) and the sidebar list (for filtering rows).
  const [kindFilter, setKindFilter] = useState<Set<string> | null>(null);

  // Activity log for the bottom timeline strip (last 4h, capped at 200).
  const [activity, setActivity] = useState<ActivityEvent[]>([]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    (async () => {
      const off = await onMemoryNoteCreated((evt) => {
        if (cancelled) return;
        // Mark recent.
        setRecentNodeIds((prev) => {
          const next = new Set(prev);
          next.add(evt.id);
          return next;
        });
        // Append to activity log (capped at 200, oldest dropped).
        setActivity((prev) => {
          const next: ActivityEvent[] = [
            ...prev,
            {
              id: evt.id,
              slug: evt.slug,
              title: evt.title,
              kind: evt.kind,
              source_kind: evt.source_kind,
              at: Date.now(),
            },
          ];
          if (next.length > 200) next.splice(0, next.length - 200);
          return next;
        });
        // Auto-clear after 3s.
        const existing = recentTimersRef.current.get(evt.id);
        if (existing) clearTimeout(existing);
        const t = setTimeout(() => {
          setRecentNodeIds((prev) => {
            if (!prev.has(evt.id)) return prev;
            const next = new Set(prev);
            next.delete(evt.id);
            return next;
          });
          recentTimersRef.current.delete(evt.id);
        }, 3000);
        recentTimersRef.current.set(evt.id, t);
        // Refresh graph + list so the new node appears.
        reloadGraph();
        reloadList();
      });
      if (cancelled) {
        off();
      } else {
        unlisten = off;
      }
    })();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
      for (const t of recentTimersRef.current.values()) clearTimeout(t);
      recentTimersRef.current.clear();
    };
  }, [reloadGraph, reloadList]);

  // ----- Debounced search -------------------------------------------------
  useEffect(() => {
    const t = setTimeout(() => {
      dispatch({ type: "searchDeb", q: s.search });
    }, 180);
    return () => clearTimeout(t);
  }, [s.search]);

  useEffect(() => {
    if (!currentId) return;
    const q = s.searchDeb.trim();
    if (!q) {
      dispatch({ type: "hits", hits: null });
      return;
    }
    let cancelled = false;
    (async () => {
      try {
        const r = await ipc.searchMemories({
          workspace_id: currentId,
          query: q,
          limit: 50,
        });
        if (!cancelled) dispatch({ type: "hits", hits: r });
      } catch (err) {
        if (!cancelled) {
          pushToast({ text: `search_memories: ${err}`, kind: "error" });
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [currentId, s.searchDeb, pushToast]);

  // ----- Active note ------------------------------------------------------
  const openNote = useCallback(
    async (id: string) => {
      if (s.dirty) {
        if (
          !confirm(
            "Unsaved changes will be lost. Open the other note anyway?",
          )
        )
          return;
      }
      dispatch({ type: "activeId", id });
      dispatch({ type: "loadingActive", v: true });
      try {
        const [n, bl, rel] = await Promise.all([
          ipc.readMemory(id),
          ipc.findBacklinks(id),
          ipc.suggestConnections(id, 8),
        ]);
        dispatch({ type: "active", note: n, backlinks: bl, related: rel });
        listRef.current?.scrollToId(id);
      } catch (err) {
        pushToast({ text: `read_memory: ${err}`, kind: "error" });
        dispatch({ type: "loadingActive", v: false });
      }
    },
    [s.dirty, pushToast],
  );

  // ----- Mutations --------------------------------------------------------
  const saveActive = useCallback(async () => {
    if (!s.activeId || !s.active) return;
    try {
      const tags = s.draftTags
        .split(",")
        .map((t) => t.trim())
        .filter(Boolean);
      const n = await ipc.updateMemory({
        id: s.activeId,
        title: s.draftTitle,
        body: s.draftBody,
        tags,
      });
      dispatch({ type: "active", note: n });
      reloadList();
      reloadGraph();
    } catch (err) {
      pushToast({ text: `update_memory: ${err}`, kind: "error" });
    }
  }, [
    s.activeId,
    s.active,
    s.draftTitle,
    s.draftBody,
    s.draftTags,
    pushToast,
    reloadList,
    reloadGraph,
  ]);

  // Auto-save 1s after the user stops typing — feels like Bear/Bridgemind.
  useEffect(() => {
    if (!s.dirty || !s.activeId) return;
    if (saveTimerRef.current) clearTimeout(saveTimerRef.current);
    saveTimerRef.current = setTimeout(() => {
      saveActive();
    }, 1000);
    return () => {
      if (saveTimerRef.current) clearTimeout(saveTimerRef.current);
    };
  }, [s.dirty, s.activeId, saveActive]);

  const createNote = useCallback(async () => {
    if (!currentId) return;
    const title = window.prompt("Title for the new note", "Untitled");
    if (!title || !title.trim()) return;
    try {
      const tags = s.tagFilter ? [s.tagFilter] : [];
      const n = await ipc.createMemory({
        workspace_id: currentId,
        title: title.trim(),
        tags,
      });
      await reloadList();
      reloadGraph();
      openNote(n.id);
    } catch (err) {
      pushToast({ text: `create_memory: ${err}`, kind: "error" });
    }
  }, [currentId, s.tagFilter, reloadList, reloadGraph, openNote, pushToast]);

  const deleteActive = useCallback(async () => {
    if (!s.activeId || !s.active) return;
    if (!confirm(`Delete "${s.active.title}"? This cannot be undone.`)) return;
    try {
      await ipc.deleteMemory(s.activeId);
      dispatch({ type: "activeId", id: null });
      dispatch({ type: "active", note: null, backlinks: [], related: [] });
      reloadList();
      reloadGraph();
    } catch (err) {
      pushToast({ text: `delete_memory: ${err}`, kind: "error" });
    }
  }, [s.activeId, s.active, reloadList, reloadGraph, pushToast]);

  // Prefer authoritative tag list from backend; fall back to client agg.
  const allTags = useMemo(() => {
    if (s.tagSummaries.length > 0) {
      return s.tagSummaries.map((t) => ({ tag: t.name, count: t.count }));
    }
    return aggregateTags(s.list);
  }, [s.tagSummaries, s.list]);

  const visible = useMemo<VisibleRow[]>(() => {
    let rows: VisibleRow[];
    if (s.hits) {
      const byId = new Map(s.list.map((n) => [n.id, n]));
      rows = s.hits.map((h) => ({
        id: h.id,
        slug: h.slug,
        title: h.title,
        kind: byId.get(h.id)?.kind,
        snippet: h.snippet,
        tags: byId.get(h.id)?.tags,
        updatedAt: byId.get(h.id)?.updated_at,
      }));
    } else {
      rows = s.list.map((n) => ({
        id: n.id,
        slug: n.slug,
        title: n.title,
        kind: n.kind,
        tags: n.tags,
        updatedAt: n.updated_at,
      }));
    }
    if (s.tagFilter) {
      rows = rows.filter((r) => r.tags?.includes(s.tagFilter!));
    }
    if (kindFilter && kindFilter.size > 0) {
      rows = rows.filter((r) => r.kind && kindFilter.has(r.kind));
    }
    if (!s.hits) {
      rows = rows.slice().sort((a, b) => {
        if (s.sort === "title") return a.title.localeCompare(b.title);
        if (s.sort === "created") {
          const ai = s.list.find((n) => n.id === a.id);
          const bi = s.list.find((n) => n.id === b.id);
          return (bi?.updated_at ?? "").localeCompare(ai?.updated_at ?? "");
        }
        return (b.updatedAt ?? "").localeCompare(a.updatedAt ?? "");
      });
    }
    return rows;
  }, [s.hits, s.list, s.tagFilter, s.sort, kindFilter]);

  // Cmd/Ctrl+S anywhere inside the workbench triggers save.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") {
        if (s.dirty) {
          e.preventDefault();
          saveActive();
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [s.dirty, saveActive]);

  if (!currentId) {
    return (
      <div className="pigmem-workbench">
        <div className="pigmem-empty pigmem-empty--centered">
          Select a workspace to view its PigMemory.
        </div>
      </div>
    );
  }

  return (
    <div className="pigmem-workbench">
      <header className="pigmem-toolbar">
        <div className="pigmem-toolbar-brand">
          <span className="pigmem-toolbar-dot" />
          <span className="pigmem-toolbar-title">PigMemory</span>
          <span className="pigmem-toolbar-count">{s.list.length}</span>
          {s.status && s.status.unresolved_links > 0 ? (
            <span
              className="pigmem-toolbar-warn"
              title={`${s.status.unresolved_links} unresolved wikilink${
                s.status.unresolved_links === 1 ? "" : "s"
              }`}
            >
              {s.status.unresolved_links} unresolved
            </span>
          ) : null}
        </div>

        <div className="pigmem-toolbar-search">
          <input
            type="search"
            placeholder="Search notes…"
            value={s.search}
            onChange={(e) => dispatch({ type: "search", q: e.target.value })}
            aria-label="Search notes"
          />
          {s.search ? (
            <button
              className="pigmem-icon-btn"
              onClick={() => dispatch({ type: "search", q: "" })}
              title="Clear search"
              aria-label="Clear search"
            >
              <X size={12} />
            </button>
          ) : null}
        </div>

        <div className="pigmem-toolbar-spacer" />

        <div className="pigmem-segmented" role="tablist" aria-label="View mode">
          {(["edit", "split", "preview"] as ViewMode[]).map((v) => (
            <button
              key={v}
              className={s.view === v ? "is-active" : ""}
              onClick={() => dispatch({ type: "view", view: v })}
              role="tab"
              aria-selected={s.view === v}
            >
              {v}
            </button>
          ))}
        </div>

        <button
          className={`pigmem-toolbar-btn ${s.showGraphTab ? "is-active" : ""}`}
          onClick={() => dispatch({ type: "graphTab", v: !s.showGraphTab })}
          title="Toggle full-canvas graph"
          aria-pressed={s.showGraphTab}
        >
          Graph
        </button>
        <button
          className="pigmem-toolbar-btn"
          onClick={() => dispatch({ type: "tagManager", v: true })}
          title="Manage tags"
        >
          Tags
        </button>
        <SmartStatusPill workspaceId={currentId} />
        <button
          className="pigmem-toolbar-btn pigmem-toolbar-btn--primary"
          onClick={createNote}
          title="New note"
        >
          <Plus size={12} /> New
        </button>
        <button
          className="pigmem-icon-btn"
          onClick={() => setShowPigMemory(false)}
          title="Close PigMemory"
          aria-label="Close"
        >
          <X size={14} />
        </button>
      </header>

      {s.showGraphTab ? (
        <div className="pigmem-graph-fullscreen">
          <PigMemoryGraph
            ref={graphRef}
            data={s.graph}
            activeId={s.activeId}
            onSelect={(id) => {
              dispatch({ type: "graphTab", v: false });
              openNote(id);
            }}
            searchTerm={s.searchDeb}
            recentNodeIds={recentNodeIds}
            kindFilter={kindFilter}
          />
          <ActivityTimeline
            events={activity}
            onFocus={(id) => {
              dispatch({ type: "graphTab", v: false });
              openNote(id);
            }}
          />
        </div>
      ) : (
        <Allotment defaultSizes={[260, 700, 320]} proportionalLayout={false}>
          <Allotment.Pane minSize={220} preferredSize={280} snap>
            <Sidebar
              s={s}
              listRef={listRef}
              dispatch={dispatch}
              onOpen={openNote}
              onCreate={createNote}
              tags={allTags}
              visible={visible}
              kindFilter={kindFilter}
              setKindFilter={setKindFilter}
            />
          </Allotment.Pane>
          <Allotment.Pane minSize={360}>
            <Workspace
              state={s}
              dispatch={dispatch}
              notes={s.list}
              onOpen={openNote}
              onSave={saveActive}
              onDelete={deleteActive}
            />
          </Allotment.Pane>
          <Allotment.Pane minSize={240} preferredSize={320} snap>
            <Inspector
              state={s}
              graph={s.graph}
              onOpen={openNote}
              onFocusGraph={(id) => {
                dispatch({ type: "graphTab", v: true });
                requestAnimationFrame(() => graphRef.current?.focusNode(id));
              }}
            />
          </Allotment.Pane>
        </Allotment>
      )}

      {s.showTagManager ? (
        <TagManager
          notes={s.list}
          onClose={() => dispatch({ type: "tagManager", v: false })}
          onChanged={async () => {
            await reloadList();
            await reloadGraph();
            if (s.activeId) {
              const fresh = await ipc.readMemory(s.activeId).catch(() => null);
              if (fresh) {
                dispatch({ type: "active", note: fresh });
              }
            }
          }}
          onFilterByTag={(tag) => {
            dispatch({ type: "tagFilter", tag });
            dispatch({ type: "tagManager", v: false });
          }}
        />
      ) : null}
    </div>
  );
}

// ============================================================================
// Sidebar
// ============================================================================
function Sidebar({
  s,
  listRef,
  dispatch,
  onOpen,
  onCreate,
  tags,
  visible,
  kindFilter,
  setKindFilter,
}: {
  s: State;
  listRef: React.MutableRefObject<NoteListHandle | null>;
  dispatch: React.Dispatch<Action>;
  onOpen: (id: string) => void;
  onCreate: () => void;
  tags: { tag: string; count: number }[];
  visible: VisibleRow[];
  kindFilter: Set<string> | null;
  setKindFilter: React.Dispatch<React.SetStateAction<Set<string> | null>>;
}) {
  return (
    <div className="pigmem-sidebar">
      <div className="pigmem-sidebar-controls">
        <select
          className="pigmem-select"
          value={s.sort}
          onChange={(e) =>
            dispatch({ type: "sort", sort: e.target.value as SortMode })
          }
          disabled={s.hits !== null}
          aria-label="Sort notes"
        >
          <option value="updated">Recently updated</option>
          <option value="title">Title (A→Z)</option>
          <option value="created">Recently created</option>
        </select>
        {s.tagFilter ? (
          <button
            className="pigmem-tag-filter-pill"
            onClick={() => dispatch({ type: "tagFilter", tag: null })}
            title="Clear tag filter"
          >
            #{s.tagFilter}
            <X size={10} />
          </button>
        ) : null}
      </div>

      <div className="pigmem-sidebar-kinds" role="toolbar" aria-label="Kind filter">
        {(["all", "concept", "entity", "task", "chat", "source"] as const).map((k) => {
          const isAll = k === "all";
          const active = isAll
            ? !kindFilter || kindFilter.size === 0
            : kindFilter?.has(k) ?? false;
          return (
            <button
              key={k}
              className={`pigmem-kind-chip pigmem-kind-chip--${k} ${active ? "is-active" : ""}`}
              onClick={() => {
                if (isAll) {
                  setKindFilter(null);
                } else {
                  setKindFilter((prev) => {
                    const next = new Set(prev ?? []);
                    if (next.has(k)) next.delete(k);
                    else next.add(k);
                    return next.size === 0 ? null : next;
                  });
                }
              }}
              title={isAll ? "Show all kinds" : `Filter to ${k}s`}
            >
              {k}
            </button>
          );
        })}
      </div>

      {tags.length > 0 ? (
        <div className="pigmem-sidebar-tags" role="toolbar" aria-label="Tags">
          {tags.slice(0, 12).map((t) => (
            <button
              key={t.tag}
              className={`pigmem-tag-chip pigmem-tag-chip--btn ${
                s.tagFilter === t.tag ? "is-active" : ""
              }`}
              onClick={() =>
                dispatch({
                  type: "tagFilter",
                  tag: s.tagFilter === t.tag ? null : t.tag,
                })
              }
              title={`${t.count} note${t.count === 1 ? "" : "s"}`}
            >
              #{t.tag}
              <span className="pigmem-tag-chip-count">{t.count}</span>
            </button>
          ))}
        </div>
      ) : null}

      <NoteList
        ref={listRef}
        items={visible}
        activeId={s.activeId}
        onSelect={onOpen}
        showSnippet={s.hits !== null}
        emptyMessage={
          s.hits !== null
            ? "No matches"
            : s.tagFilter
              ? "No notes with this tag"
              : "No notes yet — press New to create one"
        }
      />

      <div className="pigmem-sidebar-footer">
        <button
          className="pigmem-toolbar-btn pigmem-toolbar-btn--primary pigmem-sidebar-create"
          onClick={onCreate}
        >
          <Plus size={12} /> New note
        </button>
      </div>
    </div>
  );
}

// ============================================================================
// Workspace (main pane: editor + preview)
// ============================================================================
function Workspace({
  state,
  dispatch,
  notes,
  onOpen,
  onSave,
  onDelete,
}: {
  state: State;
  dispatch: React.Dispatch<Action>;
  notes: NoteSummary[];
  onOpen: (id: string) => void;
  onSave: () => void;
  onDelete: () => void;
}) {
  if (!state.active) {
    if (state.list.length === 0) {
      return (
        <div className="pigmem-blank">
          <div className="pigmem-onboarding">
            <h2 className="pigmem-onboarding-title">PigMemory is ready</h2>
            <p className="pigmem-onboarding-text">
              Memory fills itself as you work — no manual saves required.
            </p>
            <ul className="pigmem-onboarding-bullets">
              <li>Finish a task → a `task` note appears with summary &amp; files-touched.</li>
              <li>Run an agent → a `chat` note logs the session, then concepts get extracted.</li>
              <li>Open the next chat → relevant notes are already in the agent's context.</li>
            </ul>
          </div>
        </div>
      );
    }
    return (
      <div className="pigmem-blank">
        <div className="pigmem-blank-card">
          <h2>PigMemory</h2>
          <p>
            A workspace-local knowledge graph. Notes live in
            <code> .pigmemory/ </code>
            as plain markdown. Connect them with
            <code> [[wikilinks]] </code>
            and tag them with
            <code> #tags </code>.
          </p>
          <p className="pigmem-blank-hint">
            Pick a note from the sidebar, or create a new one.
          </p>
        </div>
      </div>
    );
  }

  const showEditor = state.view === "edit" || state.view === "split";
  const showPreview = state.view === "preview" || state.view === "split";

  return (
    <div className="pigmem-workspace">
      <div className="pigmem-doc-header">
        <input
          className="pigmem-doc-title"
          value={state.draftTitle}
          onChange={(e) => dispatch({ type: "draftTitle", v: e.target.value })}
          placeholder="Untitled"
        />
        <span className="pigmem-doc-slug">/{state.active.slug}</span>
        <span className="pigmem-toolbar-spacer" />
        <span
          className={`pigmem-doc-status ${state.dirty ? "is-dirty" : "is-saved"}`}
        >
          {state.dirty ? "Unsaved" : "Saved"}
        </span>
        <button
          className="pigmem-toolbar-btn"
          onClick={onSave}
          disabled={!state.dirty}
          title="Save (⌘S)"
        >
          Save
        </button>
        <button
          className="pigmem-icon-btn pigmem-icon-btn--danger"
          onClick={onDelete}
          title="Delete"
          aria-label="Delete note"
        >
          <Trash2 size={12} />
        </button>
      </div>
      <div className="pigmem-doc-meta">
        <input
          className="pigmem-doc-tags"
          value={state.draftTags}
          onChange={(e) => dispatch({ type: "draftTags", v: e.target.value })}
          placeholder="Add tags, comma-separated"
          aria-label="Tags"
        />
      </div>
      <div className="pigmem-doc-body">
        {showEditor && showPreview ? (
          <Allotment proportionalLayout defaultSizes={[1, 1]}>
            <Allotment.Pane minSize={300}>
              <PigMemoryEditor
                noteId={state.active.id}
                initial={state.draftBody}
                onChange={(v) => dispatch({ type: "draftBody", v })}
                onSave={onSave}
                notes={notes}
              />
            </Allotment.Pane>
            <Allotment.Pane minSize={280}>
              <div className="pigmem-preview-host">
                <MarkdownPreview
                  body={state.draftBody}
                  notes={notes}
                  onNavigate={onOpen}
                  onTag={(tag) =>
                    dispatch({ type: "tagFilter", tag })
                  }
                />
              </div>
            </Allotment.Pane>
          </Allotment>
        ) : showEditor ? (
          <PigMemoryEditor
            noteId={state.active.id}
            initial={state.draftBody}
            onChange={(v) => dispatch({ type: "draftBody", v })}
            onSave={onSave}
            notes={notes}
          />
        ) : (
          <div className="pigmem-preview-host">
            <MarkdownPreview
              body={state.draftBody}
              notes={notes}
              onNavigate={onOpen}
              onTag={(tag) => dispatch({ type: "tagFilter", tag })}
            />
          </div>
        )}
      </div>
    </div>
  );
}

// ============================================================================
// Inspector (right pane: backlinks + related + mini-graph)
// ============================================================================
function Inspector({
  state,
  graph,
  onOpen,
  onFocusGraph,
}: {
  state: State;
  graph: GraphData | null;
  onOpen: (id: string) => void;
  onFocusGraph: (id: string) => void;
}) {
  if (!state.active) {
    return (
      <div className="pigmem-inspector">
        <div className="pigmem-inspector-empty">
          Open a note to see its connections.
        </div>
      </div>
    );
  }

  return (
    <div className="pigmem-inspector">
      <Section title="Outgoing">
        <OutgoingLinks state={state} graph={graph} onOpen={onOpen} />
      </Section>
      <Section title="Backlinks" count={state.backlinks.length}>
        {state.backlinks.length === 0 ? (
          <div className="pigmem-empty pigmem-empty--small">
            No notes link here yet.
          </div>
        ) : (
          state.backlinks.map((b) => (
            <button
              key={b.src_id}
              className="pigmem-link-card"
              onClick={() => onOpen(b.src_id)}
            >
              <span className="pigmem-link-card-title">{b.src_title}</span>
              {b.context ? (
                <span className="pigmem-link-card-snippet">{b.context}</span>
              ) : null}
            </button>
          ))
        )}
      </Section>
      <Section title="Related" count={state.related.length}>
        {state.related.length === 0 ? (
          <div className="pigmem-empty pigmem-empty--small">
            Nothing closely related.
          </div>
        ) : (
          state.related.map((r) => (
            <button
              key={r.id}
              className="pigmem-link-card"
              onClick={() => onOpen(r.id)}
            >
              <span className="pigmem-link-card-title">{r.title}</span>
              <span
                className="pigmem-link-card-snippet"
                dangerouslySetInnerHTML={{
                  __html: highlightFtsSnippet(r.snippet),
                }}
              />
            </button>
          ))
        )}
      </Section>
      <Section title="Local graph">
        <button
          className="pigmem-inspector-graph-launch"
          onClick={() => state.activeId && onFocusGraph(state.activeId)}
          disabled={!state.activeId}
        >
          Open in full graph →
        </button>
      </Section>
    </div>
  );
}

function Section({
  title,
  count,
  children,
}: {
  title: string;
  count?: number;
  children: React.ReactNode;
}) {
  return (
    <section className="pigmem-section">
      <header className="pigmem-section-header">
        <h4>{title}</h4>
        {typeof count === "number" ? (
          <span className="pigmem-section-count">{count}</span>
        ) : null}
      </header>
      <div className="pigmem-section-body">{children}</div>
    </section>
  );
}

function OutgoingLinks({
  state,
  graph,
  onOpen,
}: {
  state: State;
  graph: GraphData | null;
  onOpen: (id: string) => void;
}) {
  const out = useMemo(() => {
    if (!graph || !state.activeId) return [];
    const idToTitle = new Map(graph.nodes.map((n) => [n.id, n.title]));
    return graph.links
      .filter((l) => l.source === state.activeId)
      .map((l) => ({
        target: l.target,
        text: l.target_text,
        ambiguous: l.ambiguous,
        title: l.target ? idToTitle.get(l.target) ?? null : null,
      }));
  }, [graph, state.activeId]);

  if (out.length === 0) {
    return (
      <div className="pigmem-empty pigmem-empty--small">
        No outgoing wikilinks. Type <code>[[</code> in the editor to add one.
      </div>
    );
  }
  return (
    <ul className="pigmem-outgoing">
      {out.map((o, i) => (
        <li key={`${o.target ?? "?"}-${i}`}>
          {o.target ? (
            <button
              className="pigmem-outgoing-resolved"
              onClick={() => onOpen(o.target!)}
            >
              {o.title ?? o.text}
            </button>
          ) : (
            <span
              className={`pigmem-outgoing-unresolved ${
                o.ambiguous ? "is-ambiguous" : ""
              }`}
              title={
                o.ambiguous
                  ? "Multiple notes match this title — disambiguate by slug"
                  : "No matching note yet"
              }
            >
              {o.text}
              {o.ambiguous ? " (ambiguous)" : ""}
            </span>
          )}
        </li>
      ))}
    </ul>
  );
}

function highlightFtsSnippet(snippet: string): string {
  const escaped = snippet
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
  return escaped
    .replace(/&lt;&lt;/g, "<mark>")
    .replace(/&gt;&gt;/g, "</mark>");
}
