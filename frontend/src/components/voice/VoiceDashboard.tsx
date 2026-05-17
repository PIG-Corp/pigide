import { useEffect, useState } from "react";
import { ipc } from "../../state/ipc";
import { useStore } from "../../state/store";
import type { VoiceStats, VoiceStatsRange } from "../../state/types";

const RANGES: { id: VoiceStatsRange; label: string }[] = [
  { id: "day", label: "Day" },
  { id: "week", label: "Week" },
  { id: "month", label: "Month" },
  { id: "all", label: "All" },
];

export function VoiceDashboard() {
  const pushToast = useStore((s) => s.pushToast);
  const [range, setRange] = useState<VoiceStatsRange>("week");
  const [stats, setStats] = useState<VoiceStats | null>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    (async () => {
      try {
        const s = await ipc.voiceStats(range);
        if (!cancelled) setStats(s);
      } catch (err) {
        if (!cancelled) pushToast({ text: `voice_stats: ${err}`, kind: "error" });
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [range, pushToast]);

  return (
    <div className="voice-panel">
      <section className="voice-section">
        <div className="voice-section-title">Range</div>
        <div className="voice-range-tabs">
          {RANGES.map((r) => (
            <button
              key={r.id}
              className={`voice-range-tab ${range === r.id ? "active" : ""}`}
              onClick={() => setRange(r.id)}
            >
              {r.label}
            </button>
          ))}
        </div>
      </section>

      <section className="voice-section">
        <div className="voice-section-title">Statistics</div>
        <div className="voice-stats-grid">
          <StatCard label="Sessions" value={stats ? stats.sessions.toString() : "—"} loading={loading} />
          <StatCard label="Total words" value={stats ? stats.total_words.toLocaleString() : "—"} loading={loading} />
          <StatCard label="Talk time" value={stats ? formatTalkTime(stats.talk_seconds) : "—"} loading={loading} />
          <StatCard label="Avg WPM" value={stats ? stats.avg_wpm.toFixed(1) : "—"} loading={loading} />
        </div>
      </section>
    </div>
  );
}

function StatCard({
  label,
  value,
  loading,
}: {
  label: string;
  value: string;
  loading: boolean;
}) {
  return (
    <div className="voice-stat-card">
      <div className="voice-stat-value">{loading ? "…" : value}</div>
      <div className="voice-stat-label">{label}</div>
    </div>
  );
}

function formatTalkTime(seconds: number): string {
  const total = Math.floor(seconds);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m.toString().padStart(2, "0")}:${s.toString().padStart(2, "0")}`;
}
