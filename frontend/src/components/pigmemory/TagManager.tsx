// Tag manager: aggregates tags from the loaded note set and supports
// rename / delete via per-note update_memory loops. The relay surface has
// no first-class tag table, so all operations are derived.

import { useMemo, useState } from "react";
import { ipc } from "../../state/ipc";
import type { NoteSummary } from "../../state/types";
import { aggregateTags } from "./wikilink";
import { X, Pencil, Trash2 } from "../icons";

export function TagManager({
  notes,
  onClose,
  onChanged,
  onFilterByTag,
}: {
  notes: NoteSummary[];
  onClose: () => void;
  onChanged: () => Promise<void>;
  onFilterByTag: (tag: string) => void;
}) {
  const tags = useMemo(() => aggregateTags(notes), [notes]);
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);

  const renameTag = async (oldTag: string, newTagRaw: string) => {
    const newTag = newTagRaw.trim();
    if (!newTag || newTag === oldTag) {
      setEditing(null);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const affected = notes.filter((n) => n.tags.includes(oldTag));
      for (const n of affected) {
        const next = Array.from(
          new Set(n.tags.map((t) => (t === oldTag ? newTag : t))),
        );
        await ipc.updateMemory({ id: n.id, tags: next });
      }
      setEditing(null);
      await onChanged();
    } catch (e: unknown) {
      // B-12.13: an Error object would otherwise stringify to
      // `[object Object]`. Use message/stack when available, fall back to
      // String() for anything else.
      const msg = e instanceof Error ? e.message : String(e);
      setError(`Rename failed: ${msg}`);
    } finally {
      setBusy(false);
    }
  };

  const deleteTag = async (tag: string) => {
    if (
      !confirm(
        `Remove tag "${tag}" from every note that has it? This cannot be undone.`,
      )
    ) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const affected = notes.filter((n) => n.tags.includes(tag));
      for (const n of affected) {
        const next = n.tags.filter((t) => t !== tag);
        await ipc.updateMemory({ id: n.id, tags: next });
      }
      await onChanged();
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(`Delete failed: ${msg}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      className="pigmem-modal-backdrop"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="pigmem-modal" role="dialog" aria-label="Tag manager">
        <div className="pigmem-modal-header">
          <h3>Tags</h3>
          <span className="pigmem-modal-spacer" />
          <button
            className="pigmem-icon-btn"
            onClick={onClose}
            title="Close"
            aria-label="Close"
          >
            <X size={14} />
          </button>
        </div>
        <div className="pigmem-modal-body">
          {tags.length === 0 ? (
            <div className="pigmem-empty">
              No tags yet. Add `#tag` or set tags on a note.
            </div>
          ) : null}
          <ul className="pigmem-tag-list">
            {tags.map((t) => (
              <li key={t.tag} className="pigmem-tag-row">
                {editing === t.tag ? (
                  <input
                    autoFocus
                    value={draft}
                    onChange={(e) => setDraft(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") renameTag(t.tag, draft);
                      if (e.key === "Escape") setEditing(null);
                    }}
                    onBlur={() => setEditing(null)}
                    disabled={busy}
                    aria-label={`Rename tag ${t.tag}`}
                  />
                ) : (
                  <button
                    className="pigmem-tag-row-label"
                    onClick={() => onFilterByTag(t.tag)}
                    title={`Filter by #${t.tag}`}
                  >
                    <span className="pigmem-tag-chip">#{t.tag}</span>
                    <span className="pigmem-tag-row-count">{t.count}</span>
                  </button>
                )}
                <span className="pigmem-modal-spacer" />
                <button
                  className="pigmem-icon-btn"
                  onClick={() => {
                    setDraft(t.tag);
                    setEditing(t.tag);
                  }}
                  disabled={busy || editing === t.tag}
                  title="Rename"
                  aria-label={`Rename ${t.tag}`}
                >
                  <Pencil size={12} />
                </button>
                <button
                  className="pigmem-icon-btn pigmem-icon-btn--danger"
                  onClick={() => deleteTag(t.tag)}
                  disabled={busy}
                  title="Delete from every note"
                  aria-label={`Delete ${t.tag}`}
                >
                  <Trash2 size={12} />
                </button>
              </li>
            ))}
          </ul>
          {error ? <div className="pigmem-error">{error}</div> : null}
        </div>
        <div className="pigmem-modal-footer">
          <span className="pigmem-modal-hint">
            Tags are derived from notes — changes write through `update_memory`.
          </span>
          <button onClick={onClose} disabled={busy}>
            Done
          </button>
        </div>
      </div>
    </div>
  );
}
