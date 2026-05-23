// Always-On Architect UI panel.
// - Top toggle: enable/disable supervisor.
// - Live decision log (last ~50 entries).
// Read-only otherwise: tuning lives in `architect.*` settings (DB).

import { useEffect, useMemo, useState } from "react";
import {
  architectIpc,
  onArchitectDecision,
  onArchitectSignal,
  signalColor,
  signalLabel,
  useArchitectStore,
  type ArchitectDecision,
} from "../state/architect";
import { ipc } from "../state/ipc";

const ARCHITECT_MODELS = [
  "kr/claude-opus-4.7",
  "kr/claude-opus-4.6",
  "kr/claude-sonnet-4.6",
  "kr/claude-sonnet-4.5",
  "kr/claude-haiku-4.5",
];

const SETTING_KEY = "architect.model";

export function ArchitectPanel() {
  const enabled = useArchitectStore((s) => s.enabled);
  const setEnabled = useArchitectStore((s) => s.setEnabled);
  const decisions = useArchitectStore((s) => s.decisions);
  const setDecisions = useArchitectStore((s) => s.setDecisions);
  const pushDecision = useArchitectStore((s) => s.pushDecision);
  const setSignals = useArchitectStore((s) => s.setSignals);
  const [busy, setBusy] = useState(false);
  const [model, setModel] = useState(ARCHITECT_MODELS[0]);

  useEffect(() => {
    ipc.getSetting(SETTING_KEY).then((v) => {
      if (v && ARCHITECT_MODELS.includes(v)) setModel(v);
    });
  }, []);

  const onModelChange = async (value: string) => {
    setModel(value);
    try {
      await ipc.setSetting(SETTING_KEY, value);
    } catch (err) {
      console.warn("architect model save", err);
    }
  };

  // Initial config + log + event subscriptions.
  useEffect(() => {
    let dead = false;
    void (async () => {
      try {
        const cfg = await architectIpc.getConfig();
        if (!dead) setEnabled(cfg.enabled);
        const log = await architectIpc.decisions(100);
        if (!dead) setDecisions(log);
      } catch (e) {
        console.warn("architect init", e);
      }
    })();
    const unsubs: Array<Promise<() => void>> = [
      onArchitectDecision((d) => pushDecision(d)),
      onArchitectSignal((e) => {
        const m: Record<string, (typeof e.signals)[number]["signal"]> = {};
        for (const s of e.signals) m[s.agent_id] = s.signal;
        setSignals(m);
      }),
    ];
    return () => {
      dead = true;
      unsubs.forEach((p) => void p.then((u) => u()));
    };
  }, [pushDecision, setDecisions, setEnabled, setSignals]);

  const toggle = async () => {
    setBusy(true);
    try {
      const next = !enabled;
      await architectIpc.setEnabled(next);
      setEnabled(next);
    } finally {
      setBusy(false);
    }
  };

  const recent = useMemo(() => decisions.slice(-50).reverse(), [decisions]);

  return (
    <div className="architect-panel">
      <div className="architect-panel-header">
        <strong>Always-On Architect</strong>
        <label className="architect-toggle">
          <input
            type="checkbox"
            disabled={busy}
            checked={enabled}
            onChange={toggle}
          />
          <span>{enabled ? "ON" : "OFF"}</span>
        </label>
      </div>
      <div className="architect-model-section">
        <label className="architect-model-label" htmlFor="architect-model-select">
          Model
        </label>
        <select
          id="architect-model-select"
          className="architect-model-select"
          value={model}
          onChange={(e) => onModelChange(e.target.value)}
        >
          {ARCHITECT_MODELS.map((m) => (
            <option key={m} value={m}>{m}</option>
          ))}
        </select>
      </div>
      <div className="architect-log">
        {recent.length === 0 ? (
          <div className="architect-empty">No decisions yet.</div>
        ) : (
          recent.map((d, i) => <DecisionRow key={`${d.at}-${i}`} d={d} />)
        )}
      </div>
    </div>
  );
}

function DecisionRow({ d }: { d: ArchitectDecision }) {
  const time = new Date(d.at).toLocaleTimeString();
  const color = signalColor(d.signal);
  return (
    <div className="architect-row" data-kind={d.kind}>
      <div className="architect-row-head">
        <span className="architect-time">{time}</span>
        <span
          className="architect-signal"
          style={{ borderColor: color, color }}
        >
          {signalLabel(d.signal)}
        </span>
        <span className="architect-kind">{d.kind.replace(/_/g, " ")}</span>
        {d.auto_executed ? (
          <span className="architect-tag architect-auto">auto</span>
        ) : d.kind === "escalate" ? (
          <span className="architect-tag architect-esc">escalated</span>
        ) : null}
        <span className="architect-agent">{d.agent_id.slice(0, 8)}</span>
      </div>
      <div className="architect-reason">{d.reason}</div>
      {d.quote ? <pre className="architect-quote">{d.quote}</pre> : null}
    </div>
  );
}
