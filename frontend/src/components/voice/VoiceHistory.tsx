import { useEffect, useState } from "react";
import { ipc } from "../../state/ipc";
import { useStore } from "../../state/store";
import type { Transcript } from "../../state/types";
import { Trash2 } from "../icons";

export function VoiceHistory() {
  const pushToast = useStore((s) => s.pushToast);
  const [items, setItems] = useState<Transcript[]>([]);
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(true);

  const loadDefault = async () => {
    try {
      const list = await ipc.voiceHistoryList(100);
      setItems(list);
    } catch (err) {
      pushToast({ text: `voice_history_list: ${err}`, kind: "error" });
    }
  };

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const list = await ipc.voiceHistoryList(100);
        if (!cancelled) setItems(list);
      } catch (err) {
        if (!cancelled) {
          pushToast({ text: `voice_history_list: ${err}`, kind: "error" });
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [pushToast]);

  const search = async () => {
    const q = query.trim();
    if (!q) {
      loadDefault();
      return;
    }
    try {
      const list = await ipc.voiceHistorySearch(q, 100);
      setItems(list);
    } catch (err) {
      pushToast({ text: `voice_history_search: ${err}`, kind: "error" });
    }
  };

  const clear = async () => {
    setQuery("");
    loadDefault();
  };

  const remove = async (id: string) => {
    try {
      await ipc.voiceHistoryDelete(id);
      setItems((prev) => prev.filter((t) => t.id !== id));
    } catch (err) {
      pushToast({ text: `voice_history_delete: ${err}`, kind: "error" });
    }
  };

  const exportJsonl = () => {
    if (items.length === 0) return;
    const lines = items.map((t) => JSON.stringify(t)).join("\n");
    const blob = new Blob([lines], { type: "application/x-ndjson" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `voice-transcripts-${new Date().toISOString().slice(0, 10)}.jsonl`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  };

  if (loading) {
    return <div className="voice-panel"><div className="voice-section">Loading…</div></div>;
  }

  return (
    <div className="voice-panel">
      <section className="voice-section">
        <div className="voice-section-title">Search</div>
        <div className="voice-history-search">
          <input
            type="text"
            placeholder="Search transcripts…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") search();
              if (e.key === "Escape") clear();
            }}
          />
          <button onClick={search}>Search</button>
          {query.length > 0 ? <button onClick={clear}>Clear</button> : null}
          <button onClick={exportJsonl} title="Export JSONL">
            Export
          </button>
        </div>
      </section>

      <section className="voice-section voice-history-list">
        <div className="voice-section-title">
          Transcripts ({items.length})
        </div>
        {items.length === 0 ? (
          <div className="empty-state" style={{ padding: 16 }}>
            No transcripts yet.
          </div>
        ) : (
          <div className="voice-history-items">
            {items.map((t) => (
              <TranscriptItem key={t.id} t={t} onDelete={() => remove(t.id)} />
            ))}
          </div>
        )}
      </section>
    </div>
  );
}

function TranscriptItem({
  t,
  onDelete,
}: {
  t: Transcript;
  onDelete: () => void;
}) {
  return (
    <div className="voice-history-item">
      <div className="voice-history-meta">
        <span className="badge">{shortModelId(t.model_id)}</span>
        {t.language ? <span className="badge">{t.language}</span> : null}
        <span className="meta-dim">{formatDuration(t.duration_ms)}</span>
        <span className="meta-dim">{t.word_count} words</span>
        <span className="meta-dim">{formatDate(t.created_at)}</span>
        <span style={{ flex: 1 }} />
        <button
          onClick={onDelete}
          title="Delete"
          className="btn--icon btn--sm"
        >
          <Trash2 size={12} />
        </button>
      </div>
      <div className="voice-history-text">{t.text}</div>
    </div>
  );
}

function shortModelId(id: string): string {
  const i = id.lastIndexOf("-");
  return i >= 0 ? id.slice(i + 1) : id;
}

function formatDuration(ms: number): string {
  const s = Math.floor(ms / 1000);
  const m = Math.floor(s / 60);
  const rs = s % 60;
  return m > 0 ? `${m}m ${rs}s` : `${rs}s`;
}

function formatDate(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleString();
}
