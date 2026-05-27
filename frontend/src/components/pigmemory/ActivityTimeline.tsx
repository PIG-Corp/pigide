// ActivityTimeline — bottom strip in PigMemory's full-canvas graph view.
//
// Plots the last 4 hours of memory://note.created events as colour-coded
// dots on a horizontal time axis. Hover for title, click to focus the
// node in the graph. Buffer is in-memory + scoped to the current
// workspace; events are pushed in by PigMemoryWorkbench's listener.

import { useMemo, useState } from "react";
import type { NoteKind } from "../../state/types";

export interface ActivityEvent {
  id: string;
  slug: string;
  title: string;
  kind: NoteKind;
  source_kind: string;
  at: number; // epoch ms
}

const WINDOW_MS = 4 * 60 * 60 * 1000; // 4h
const ROW_HEIGHT = 80;

export function ActivityTimeline({
  events,
  onFocus,
}: {
  events: ActivityEvent[];
  onFocus?: (id: string) => void;
}) {
  const [hoverId, setHoverId] = useState<string | null>(null);
  const now = Date.now();

  const inWindow = useMemo(
    () => events.filter((e) => now - e.at <= WINDOW_MS).slice(-200),
    [events, now],
  );

  const counts = useMemo(() => {
    const today = inWindow.length;
    return { today };
  }, [inWindow]);

  if (inWindow.length === 0) {
    return (
      <div className="pigmem-activity pigmem-activity--empty">
        <span>Activity timeline — events appear as work happens.</span>
      </div>
    );
  }

  // Ticks every 30 min for the 4h window.
  const ticks = [4, 3.5, 3, 2.5, 2, 1.5, 1, 0.5, 0];

  return (
    <div className="pigmem-activity" style={{ height: ROW_HEIGHT }}>
      <div className="pigmem-activity-track">
        {ticks.map((h) => (
          <div
            key={h}
            className="pigmem-activity-tick"
            style={{ left: `${(1 - h / 4) * 100}%` }}
          >
            <span className="pigmem-activity-tick-label">
              {h === 0 ? "now" : `-${h}h`}
            </span>
          </div>
        ))}
        {inWindow.map((evt) => {
          const ageMs = now - evt.at;
          const x = (1 - ageMs / WINDOW_MS) * 100;
          const isHover = hoverId === evt.id;
          return (
            <button
              key={`${evt.id}-${evt.at}`}
              className={`pigmem-activity-dot pigmem-activity-dot--${evt.kind} ${
                isHover ? "is-hover" : ""
              }`}
              style={{ left: `${x}%` }}
              onMouseEnter={() => setHoverId(evt.id)}
              onMouseLeave={() => setHoverId(null)}
              onClick={() => onFocus?.(evt.id)}
              title={`${evt.kind} · ${evt.title}`}
            />
          );
        })}
        {hoverId
          ? (() => {
              const evt = inWindow.find((e) => e.id === hoverId);
              if (!evt) return null;
              const x = (1 - (now - evt.at) / WINDOW_MS) * 100;
              return (
                <div
                  className="pigmem-activity-tooltip"
                  style={{ left: `${x}%` }}
                >
                  <span className="pigmem-activity-tooltip-kind">
                    {evt.kind}
                  </span>
                  <span className="pigmem-activity-tooltip-title">
                    {evt.title}
                  </span>
                  <span className="pigmem-activity-tooltip-slug">
                    {evt.slug}
                  </span>
                </div>
              );
            })()
          : null}
      </div>
      <div className="pigmem-activity-counts">
        {counts.today} in last 4h
      </div>
    </div>
  );
}
