import { useEffect, useState } from "react";
import { ipc } from "../../state/ipc";
import { useStore } from "../../state/store";
import type { DictEntry } from "../../state/types";
import { Trash2, Plus } from "../icons";

export function DictionaryEditor() {
  const pushToast = useStore((s) => s.pushToast);
  const [entries, setEntries] = useState<DictEntry[]>([]);
  const [loading, setLoading] = useState(true);

  const [newPattern, setNewPattern] = useState("");
  const [newReplacement, setNewReplacement] = useState("");
  const [newCaseSense, setNewCaseSense] = useState(false);

  const refresh = async () => {
    try {
      const list = await ipc.voiceDictList();
      setEntries(list);
    } catch (err) {
      pushToast({ text: `voice_dict_list: ${err}`, kind: "error" });
    }
  };

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const list = await ipc.voiceDictList();
        if (!cancelled) setEntries(list);
      } catch (err) {
        if (!cancelled) pushToast({ text: `voice_dict_list: ${err}`, kind: "error" });
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [pushToast]);

  const updateField = async (
    id: string,
    patch: Partial<Pick<DictEntry, "pattern" | "replacement" | "case_sense" | "enabled">>,
  ) => {
    setEntries((prev) =>
      prev.map((e) => (e.id === id ? { ...e, ...patch } : e)),
    );
    try {
      await ipc.voiceDictUpdate({ id, ...patch });
    } catch (err) {
      pushToast({ text: `voice_dict_update: ${err}`, kind: "error" });
      refresh();
    }
  };

  const remove = async (id: string) => {
    if (!confirm("Delete this dictionary entry?")) return;
    try {
      await ipc.voiceDictDelete(id);
      setEntries((prev) => prev.filter((e) => e.id !== id));
    } catch (err) {
      pushToast({ text: `voice_dict_delete: ${err}`, kind: "error" });
    }
  };

  const add = async () => {
    const pattern = newPattern.trim();
    const replacement = newReplacement;
    if (!pattern) return;
    try {
      const created = await ipc.voiceDictAdd({
        pattern,
        replacement,
        case_sense: newCaseSense,
      });
      setEntries((prev) => [created, ...prev]);
      setNewPattern("");
      setNewReplacement("");
      setNewCaseSense(false);
    } catch (err) {
      pushToast({ text: `voice_dict_add: ${err}`, kind: "error" });
    }
  };

  const quickAdd = () => {
    const sel = window.getSelection()?.toString().trim() ?? "";
    if (!sel) {
      pushToast({
        text: "Select some text first to seed a pattern.",
        kind: "info",
      });
      return;
    }
    setNewPattern(sel);
    setNewReplacement("");
  };

  if (loading) {
    return <div className="voice-panel"><div className="voice-section">Loading…</div></div>;
  }

  return (
    <div className="voice-panel">
      <section className="voice-section voice-dict-list">
        <div className="voice-section-title">Replacements</div>
        {entries.length === 0 ? (
          <div className="empty-state" style={{ padding: 16 }}>
            No entries yet — add one below.
          </div>
        ) : (
          <div className="voice-dict-table">
            <div className="voice-dict-row voice-dict-head">
              <span className="dict-col-en">On</span>
              <span className="dict-col-pat">Pattern</span>
              <span className="dict-col-rep">Replacement</span>
              <span className="dict-col-cs">Aa</span>
              <span className="dict-col-act" />
            </div>
            {entries.map((e) => (
              <DictRow
                key={e.id}
                entry={e}
                onUpdate={(patch) => updateField(e.id, patch)}
                onDelete={() => remove(e.id)}
              />
            ))}
          </div>
        )}
      </section>

      <section className="voice-section">
        <div className="voice-section-title">Add new</div>
        <div className="voice-dict-add">
          <input
            type="text"
            placeholder="pattern"
            value={newPattern}
            onChange={(e) => setNewPattern(e.target.value)}
          />
          <input
            type="text"
            placeholder="replacement"
            value={newReplacement}
            onChange={(e) => setNewReplacement(e.target.value)}
          />
          <label className="voice-checkbox">
            <input
              type="checkbox"
              checked={newCaseSense}
              onChange={(e) => setNewCaseSense(e.target.checked)}
            />
            <span>Case-sensitive</span>
          </label>
          <div className="voice-dict-add-bar">
            <button onClick={quickAdd} title="From current selection">
              From selection
            </button>
            <button onClick={add} disabled={!newPattern.trim()}>
              <Plus size={12} /> Add
            </button>
          </div>
        </div>
      </section>
    </div>
  );
}

function DictRow({
  entry,
  onUpdate,
  onDelete,
}: {
  entry: DictEntry;
  onUpdate: (patch: Partial<Pick<DictEntry, "pattern" | "replacement" | "case_sense" | "enabled">>) => void;
  onDelete: () => void;
}) {
  const [pattern, setPattern] = useState(entry.pattern);
  const [replacement, setReplacement] = useState(entry.replacement);

  useEffect(() => {
    setPattern(entry.pattern);
  }, [entry.pattern]);
  useEffect(() => {
    setReplacement(entry.replacement);
  }, [entry.replacement]);

  const commitPattern = () => {
    if (pattern !== entry.pattern) onUpdate({ pattern });
  };
  const commitReplacement = () => {
    if (replacement !== entry.replacement) onUpdate({ replacement });
  };

  const handleKey = (
    e: React.KeyboardEvent<HTMLInputElement>,
    commit: () => void,
  ) => {
    if (e.key === "Enter") {
      e.currentTarget.blur();
    } else if (e.key === "Escape") {
      setPattern(entry.pattern);
      setReplacement(entry.replacement);
      e.currentTarget.blur();
    }
    void commit;
  };

  return (
    <div className={`voice-dict-row ${entry.enabled ? "" : "disabled"}`}>
      <span className="dict-col-en">
        <input
          type="checkbox"
          checked={entry.enabled}
          onChange={(e) => onUpdate({ enabled: e.target.checked })}
        />
      </span>
      <span className="dict-col-pat">
        <input
          type="text"
          value={pattern}
          onChange={(e) => setPattern(e.target.value)}
          onBlur={commitPattern}
          onKeyDown={(e) => handleKey(e, commitPattern)}
        />
      </span>
      <span className="dict-col-rep">
        <input
          type="text"
          value={replacement}
          onChange={(e) => setReplacement(e.target.value)}
          onBlur={commitReplacement}
          onKeyDown={(e) => handleKey(e, commitReplacement)}
        />
      </span>
      <span className="dict-col-cs">
        <input
          type="checkbox"
          checked={entry.case_sense}
          onChange={(e) => onUpdate({ case_sense: e.target.checked })}
          title="Case-sensitive"
        />
      </span>
      <span className="dict-col-act">
        <button
          onClick={onDelete}
          title="Delete"
          className="btn--icon btn--sm"
        >
          <Trash2 size={12} />
        </button>
      </span>
    </div>
  );
}
