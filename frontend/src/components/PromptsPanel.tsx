import { useEffect, useMemo, useState } from "react";
import { useStore } from "../state/store";
import { ipc } from "../state/ipc";
import type { Prompt } from "../state/types";
import { Plus, Trash2 } from "./icons";

/**
 * PromptsPanel — reusable prompt library (BridgeSpace gap #18).
 *
 * Lists prompts visible to the current workspace (workspace-scoped + global),
 * lets the user create/edit/delete them, and offers a one-click "Insert into
 * chat" button that appends the prompt body into the orchestrator draft input.
 */
export function PromptsPanel() {
  const currentId = useStore((s) => s.currentId);
  const pushToast = useStore((s) => s.pushToast);
  const appendDraftInput = useStore((s) => s.appendDraftInput);

  const [items, setItems] = useState<Prompt[]>([]);
  const [filter, setFilter] = useState("");
  const [editingId, setEditingId] = useState<string | null>(null);
  const [draft, setDraft] = useState<{ name: string; body: string; tags: string }>({
    name: "",
    body: "",
    tags: "",
  });
  const [creating, setCreating] = useState(false);
  const [scope, setScope] = useState<"workspace" | "global">("workspace");

  const reload = async () => {
    try {
      const list = await ipc.listPrompts(
        currentId ? { workspace_id: currentId } : undefined,
      );
      setItems(list);
    } catch (err) {
      pushToast({ text: `list_prompts: ${err}`, kind: "error" });
    }
  };

  useEffect(() => {
    /* eslint-disable react-hooks/set-state-in-effect */
    reload();
    setEditingId(null);
    setCreating(false);
    /* eslint-enable react-hooks/set-state-in-effect */
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentId]);

  const visible = useMemo(() => {
    const f = filter.trim().toLowerCase();
    if (!f) return items;
    return items.filter(
      (p) =>
        p.name.toLowerCase().includes(f) ||
        p.body.toLowerCase().includes(f) ||
        p.tags.some((t) => t.toLowerCase().includes(f)),
    );
  }, [items, filter]);

  const startCreate = () => {
    setEditingId(null);
    setCreating(true);
    setDraft({ name: "", body: "", tags: "" });
    setScope(currentId ? "workspace" : "global");
  };

  const startEdit = (p: Prompt) => {
    setCreating(false);
    setEditingId(p.id);
    setDraft({
      name: p.name,
      body: p.body,
      tags: p.tags.join(", "),
    });
    setScope(p.workspace_id ? "workspace" : "global");
  };

  const cancel = () => {
    setEditingId(null);
    setCreating(false);
  };

  const parseTags = (s: string) =>
    s
      .split(",")
      .map((t) => t.trim())
      .filter(Boolean);

  const save = async () => {
    if (!draft.name.trim()) {
      pushToast({ text: "Name is required", kind: "error" });
      return;
    }
    try {
      if (editingId) {
        await ipc.updatePrompt({
          id: editingId,
          name: draft.name,
          body: draft.body,
          tags: parseTags(draft.tags),
        });
      } else {
        await ipc.createPrompt({
          workspace_id:
            scope === "workspace" && currentId ? currentId : null,
          name: draft.name,
          body: draft.body,
          tags: parseTags(draft.tags),
        });
      }
      cancel();
      await reload();
    } catch (err) {
      pushToast({ text: `save_prompt: ${err}`, kind: "error" });
    }
  };

  const remove = async (id: string) => {
    try {
      await ipc.deletePrompt(id);
      await reload();
    } catch (err) {
      pushToast({ text: `delete_prompt: ${err}`, kind: "error" });
    }
  };

  const insertIntoChat = (p: Prompt) => {
    appendDraftInput(p.body);
    pushToast({ text: `Inserted "${p.name}" into chat`, kind: "info" });
  };

  return (
    <div className="prompts-panel">
      <div className="prompts-header">
        <input
          className="prompts-search"
          placeholder="Filter prompts…"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <button onClick={startCreate} title="New prompt">
          <Plus size={14} />
        </button>
      </div>

      {(creating || editingId) && (
        <div className="prompts-editor">
          <input
            className="prompts-name"
            placeholder="Name"
            value={draft.name}
            onChange={(e) => setDraft({ ...draft, name: e.target.value })}
          />
          <input
            className="prompts-tags"
            placeholder="Tags (comma-separated)"
            value={draft.tags}
            onChange={(e) => setDraft({ ...draft, tags: e.target.value })}
          />
          <textarea
            className="prompts-body"
            placeholder="Prompt body"
            value={draft.body}
            onChange={(e) => setDraft({ ...draft, body: e.target.value })}
          />
          {creating && (
            <div className="prompts-scope">
              <label>
                <input
                  type="radio"
                  checked={scope === "workspace"}
                  disabled={!currentId}
                  onChange={() => setScope("workspace")}
                />
                Workspace
              </label>
              <label>
                <input
                  type="radio"
                  checked={scope === "global"}
                  onChange={() => setScope("global")}
                />
                Global
              </label>
            </div>
          )}
          <div className="prompts-editor-actions">
            <button onClick={save}>Save</button>
            <button onClick={cancel}>Cancel</button>
          </div>
        </div>
      )}

      <div className="prompts-list">
        {visible.length === 0 ? (
          <div className="prompts-empty">No prompts yet</div>
        ) : (
          visible.map((p) => (
            <div key={p.id} className="prompts-item">
              <div className="prompts-item-head">
                <span className="prompts-item-name">{p.name}</span>
                <span className="prompts-item-scope">
                  {p.workspace_id ? "workspace" : "global"}
                </span>
              </div>
              {p.tags.length > 0 && (
                <div className="prompts-item-tags">
                  {p.tags.map((t) => (
                    <span key={t}>#{t}</span>
                  ))}
                </div>
              )}
              <div className="prompts-item-body">{p.body}</div>
              <div className="prompts-item-actions">
                <button onClick={() => insertIntoChat(p)}>Insert</button>
                <button onClick={() => startEdit(p)}>Edit</button>
                <button onClick={() => remove(p.id)} title="Delete">
                  <Trash2 size={12} />
                </button>
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
