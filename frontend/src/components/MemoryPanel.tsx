import { useEffect, useMemo, useState } from "react";
import { useStore } from "../state/store";
import { ipc } from "../state/ipc";
import type { Backlink, Note, NoteSummary, SearchHit } from "../state/types";
import { MemoryGraph } from "./MemoryGraph";
import { Trash2, X } from "./icons";

type Mode = "list" | "graph";

/**
 * MemoryPanel — list/search/edit notes from .pigmemory/. Lives next to the
 * orchestrator chat as a tab on the right pane.
 */
export function MemoryPanel({ onClose }: { onClose?: () => void }) {
  const currentId = useStore((s) => s.currentId);
  const pushToast = useStore((s) => s.pushToast);

  const [list, setList] = useState<NoteSummary[]>([]);
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SearchHit[] | null>(null);
  const [openId, setOpenId] = useState<string | null>(null);
  const [openNote, setOpenNote] = useState<Note | null>(null);
  const [backlinks, setBacklinks] = useState<Backlink[]>([]);
  const [related, setRelated] = useState<SearchHit[]>([]);
  const [draftTitle, setDraftTitle] = useState("");
  const [draftBody, setDraftBody] = useState("");
  const [draftTags, setDraftTags] = useState("");
  const [creating, setCreating] = useState(false);
  const [newTitle, setNewTitle] = useState("");
  const [mode, setMode] = useState<Mode>("list");

  const reload = async () => {
    if (!currentId) {
      setList([]);
      return;
    }
    try {
      const ms = await ipc.listMemories({
        workspace_id: currentId,
        limit: 200,
      });
      setList(ms);
    } catch (err) {
      pushToast({ text: `list_memories: ${err}`, kind: "error" });
    }
  };

  useEffect(() => {
    reload();
    setHits(null);
    setQuery("");
    setOpenId(null);
    setOpenNote(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentId]);

  const search = async () => {
    if (!currentId) return;
    const q = query.trim();
    if (!q) {
      setHits(null);
      return;
    }
    try {
      const r = await ipc.searchMemories({
        workspace_id: currentId,
        query: q,
        limit: 20,
      });
      setHits(r);
    } catch (err) {
      pushToast({ text: `search_memories: ${err}`, kind: "error" });
    }
  };

  const open = async (id: string) => {
    setOpenId(id);
    try {
      const n = await ipc.readMemory(id);
      setOpenNote(n);
      setDraftTitle(n.title);
      setDraftBody(n.body);
      setDraftTags(n.tags.join(", "));
      const [bl, rel] = await Promise.all([
        ipc.findBacklinks(id),
        ipc.suggestConnections(id, 5),
      ]);
      setBacklinks(bl);
      setRelated(rel);
    } catch (err) {
      pushToast({ text: `read_memory: ${err}`, kind: "error" });
    }
  };

  const closeNote = () => {
    setOpenId(null);
    setOpenNote(null);
    setBacklinks([]);
    setRelated([]);
  };

  const save = async () => {
    if (!openId) return;
    try {
      const tags = draftTags
        .split(",")
        .map((t) => t.trim())
        .filter(Boolean);
      const n = await ipc.updateMemory({
        id: openId,
        title: draftTitle,
        body: draftBody,
        tags,
      });
      setOpenNote(n);
      reload();
    } catch (err) {
      pushToast({ text: `update_memory: ${err}`, kind: "error" });
    }
  };

  const remove = async () => {
    if (!openId) return;
    if (!confirm("Удалить заметку?")) return;
    try {
      await ipc.deleteMemory(openId);
      closeNote();
      reload();
    } catch (err) {
      pushToast({ text: `delete_memory: ${err}`, kind: "error" });
    }
  };

  const create = async () => {
    if (!currentId || !newTitle.trim()) return;
    try {
      const n = await ipc.createMemory({
        workspace_id: currentId,
        title: newTitle.trim(),
      });
      setNewTitle("");
      setCreating(false);
      reload();
      open(n.id);
    } catch (err) {
      pushToast({ text: `create_memory: ${err}`, kind: "error" });
    }
  };

  const visible: { id: string; slug: string; title: string; tags?: string[]; snippet?: string }[] =
    useMemo(() => {
      if (hits) {
        return hits.map((h) => ({
          id: h.id,
          slug: h.slug,
          title: h.title,
          snippet: h.snippet,
        }));
      }
      return list.map((n) => ({
        id: n.id,
        slug: n.slug,
        title: n.title,
        tags: n.tags,
      }));
    }, [hits, list]);

  if (!currentId) {
    return (
      <div className="memory-panel">
        <div className="empty-state">No workspace selected</div>
      </div>
    );
  }

  return (
    <div className="memory-panel">
      <div className="memory-header">
        <span>Memory</span>
        <span className="memory-count">{visible.length}</span>
        <span className="spacer" />
        <button
          className={mode === "graph" ? "active" : ""}
          onClick={() => setMode((m) => (m === "graph" ? "list" : "graph"))}
          title="Toggle graph view"
        >
          {mode === "graph" ? "List" : "Graph"}
        </button>
        <button onClick={() => setCreating((v) => !v)} title="New note">
          + New
        </button>
        {onClose ? (
          <button className="btn--icon" onClick={onClose} title="Close">
            <X size={12} />
          </button>
        ) : null}
      </div>

      {creating ? (
        <div className="memory-create">
          <input
            placeholder="Title"
            value={newTitle}
            onChange={(e) => setNewTitle(e.target.value)}
            autoFocus
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                create();
              }
              if (e.key === "Escape") setCreating(false);
            }}
          />
          <div className="memory-create-bar">
            <button onClick={() => setCreating(false)}>Cancel</button>
            <button onClick={create} disabled={!newTitle.trim()}>
              Create
            </button>
          </div>
        </div>
      ) : null}

      {mode === "graph" ? (
        <div className="memory-graph-wrap">
          <MemoryGraph onSelect={(id) => { setMode("list"); open(id); }} />
        </div>
      ) : (
        <>
          <div className="memory-search">
            <input
              placeholder="Search (FTS5)…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") search();
                if (e.key === "Escape") {
                  setQuery("");
                  setHits(null);
                }
              }}
            />
          </div>

          <div className="memory-list">
            {visible.length === 0 ? (
              <div className="empty-state">No notes yet</div>
            ) : null}
            {visible.map((n) => (
              <div
                key={n.id}
                className={`memory-item ${openId === n.id ? "active" : ""}`}
                onClick={() => open(n.id)}
              >
                <div className="memory-item-title">{n.title}</div>
                <div className="memory-item-slug">{n.slug}</div>
                {n.snippet ? (
                  <div
                    className="memory-item-snippet"
                    dangerouslySetInnerHTML={{ __html: highlight(n.snippet) }}
                  />
                ) : n.tags && n.tags.length > 0 ? (
                  <div className="memory-item-tags">
                    {n.tags.map((t) => (
                      <span key={t} className="tag">
                        {t}
                      </span>
                    ))}
                  </div>
                ) : null}
              </div>
            ))}
          </div>
        </>
      )}

      {openNote ? (
        <div className="memory-editor">
          <div className="memory-editor-header">
            <span className="memory-item-slug">{openNote.slug}</span>
            <span className="spacer" />
            <button onClick={save}>Save</button>
            <button className="btn--icon" onClick={remove} title="Delete">
              <Trash2 size={12} />
            </button>
            <button className="btn--icon" onClick={closeNote} title="Close">
              <X size={12} />
            </button>
          </div>
          <input
            value={draftTitle}
            onChange={(e) => setDraftTitle(e.target.value)}
            placeholder="Title"
          />
          <input
            value={draftTags}
            onChange={(e) => setDraftTags(e.target.value)}
            placeholder="Tags (comma-separated)"
          />
          <textarea
            value={draftBody}
            onChange={(e) => setDraftBody(e.target.value)}
            placeholder="Body — supports [[wikilinks]]"
            rows={10}
          />
          {backlinks.length > 0 ? (
            <div className="memory-backlinks">
              <div className="memory-section-label">Backlinks</div>
              {backlinks.map((b) => (
                <div
                  key={b.src_id}
                  className="memory-backlink"
                  onClick={() => open(b.src_id)}
                >
                  <span className="memory-backlink-title">{b.src_title}</span>
                  <span className="memory-backlink-context">{b.context}</span>
                </div>
              ))}
            </div>
          ) : null}
          {related.length > 0 ? (
            <div className="memory-backlinks">
              <div className="memory-section-label">Related</div>
              {related.map((r) => (
                <div
                  key={r.id}
                  className="memory-backlink"
                  onClick={() => open(r.id)}
                >
                  <span className="memory-backlink-title">{r.title}</span>
                  <span
                    className="memory-backlink-context"
                    dangerouslySetInnerHTML={{ __html: highlight(r.snippet) }}
                  />
                </div>
              ))}
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

function highlight(snippet: string): string {
  // Snippets come from FTS5 with `<<` / `>>` markers; convert to <mark>.
  const escaped = snippet
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
  return escaped
    .replace(/&lt;&lt;/g, "<mark>")
    .replace(/&gt;&gt;/g, "</mark>");
}
