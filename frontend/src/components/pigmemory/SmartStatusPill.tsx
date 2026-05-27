// SmartStatusPill — header indicator for the Phase 2 smart-lane worker.
// Shows live queue depth + an enable/disable toggle. Polls every 15s.

import { useEffect, useState } from "react";
import { ipc } from "../../state/ipc";

interface Status {
  enabled: boolean;
  queue_len: number;
  interval_seconds: number;
  model: string;
}

export function SmartStatusPill({ workspaceId }: { workspaceId: string | null }) {
  const [status, setStatus] = useState<Status | null>(null);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!workspaceId) {
      setStatus(null);
      return;
    }
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const tick = async () => {
      try {
        const s = await ipc.memorySmartStatus(workspaceId);
        if (!cancelled) setStatus(s);
      } catch {
        // ignore — backend may be starting up; pill stays as last value
      }
      if (!cancelled) timer = setTimeout(tick, 15000);
    };
    tick();
    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, [workspaceId]);

  if (!workspaceId || !status) return null;

  const onToggle = async () => {
    if (busy) return;
    setBusy(true);
    try {
      const next = status.enabled ? "false" : "true";
      await ipc.memorySetSmartSetting("memory.smart_ingest.enabled", next);
      setStatus({ ...status, enabled: !status.enabled });
    } finally {
      setBusy(false);
    }
  };

  const dotClass = status.enabled
    ? "pigmem-smart-pill-dot is-on"
    : "pigmem-smart-pill-dot is-off";

  return (
    <div className="pigmem-smart-pill-wrap">
      <button
        className="pigmem-smart-pill"
        onClick={() => setOpen((v) => !v)}
        title={`Smart-lane ${status.enabled ? "on" : "off"} · ${status.queue_len} queued`}
      >
        <span className={dotClass} />
        <span className="pigmem-smart-pill-label">
          {status.enabled ? "Smart on" : "Smart off"}
        </span>
        {status.queue_len > 0 ? (
          <span className="pigmem-smart-pill-badge">{status.queue_len}</span>
        ) : null}
      </button>
      {open ? (
        <div
          className="pigmem-smart-popover"
          onMouseLeave={() => setOpen(false)}
        >
          <div className="pigmem-smart-popover-row">
            <span>Smart-lane</span>
            <button
              className={`pigmem-smart-toggle ${status.enabled ? "is-on" : ""}`}
              onClick={onToggle}
              disabled={busy}
            >
              {status.enabled ? "On" : "Off"}
            </button>
          </div>
          <div className="pigmem-smart-popover-row pigmem-smart-popover-row--muted">
            <span>Interval</span>
            <span>{status.interval_seconds}s</span>
          </div>
          <div className="pigmem-smart-popover-row pigmem-smart-popover-row--muted">
            <span>Queue</span>
            <span>{status.queue_len} pending</span>
          </div>
          <div className="pigmem-smart-popover-row pigmem-smart-popover-row--muted">
            <span>Model</span>
            <span className="pigmem-smart-popover-mono">{status.model}</span>
          </div>
        </div>
      ) : null}
    </div>
  );
}
