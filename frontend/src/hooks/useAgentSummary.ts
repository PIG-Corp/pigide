import { useEffect, useRef, useState } from "react";
import { onAgentStdout } from "../state/ipc";

/* eslint-disable no-control-regex */
const ANSI_RE =
  /\x1B\][\s\S]*?(?:\x07|\x1B\\)|\x1B\[[0-?]*[ -/]*[@-~]|\x1B[ -/]*[@-~]|\x1B./g;
const CTRL_RE = /[\x00-\x08\x0B-\x0C\x0E-\x1F\x7F]/g;
/* eslint-enable no-control-regex */
const SPINNER_RE = /^[\s|/\\\-•·.…⠁-⣿◐◓◑◒◴◵◶◷◰◱◲◳▖▘▝▗◇◈◆]+$/;
const SUMMARY_MAX = 40;
const IDLE_AFTER_MS = 5000;
const THROTTLE_MS = 500;
const BUFFER_LIMIT = 4096;

function stripControl(s: string): string {
  return s.replace(ANSI_RE, "").replace(CTRL_RE, "");
}

function lastMeaningfulLine(buf: string): string | null {
  const lines = buf.split(/\r?\n/);
  for (let i = lines.length - 1; i >= 0; i -= 1) {
    let line = lines[i];
    const cr = line.lastIndexOf("\r");
    if (cr >= 0) line = line.slice(cr + 1);
    line = stripControl(line).trim();
    if (!line || line.length < 2) continue;
    if (SPINNER_RE.test(line)) continue;
    return line;
  }
  return null;
}

function truncate(s: string, max: number): string {
  if (s.length <= max) return s;
  return s.slice(0, max - 1).trimEnd() + "…";
}

function fromB64(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
  return out;
}

/**
 * Live one-line summary of what an agent is doing right now.
 *
 * Priority chain:
 *   1) latest meaningful stdout line (ANSI/control/spinner-stripped)
 *   2) task title, if a task is bound to this agent
 *   3) "idle" when stdout is silent ≥ 5s and no task is bound
 *
 * Updates are throttled to ~2 Hz so a noisy stream does not thrash React.
 */
export function useAgentSummary(
  agentId: string,
  taskTitle: string | null,
): string {
  const [summary, setSummary] = useState<string>(() =>
    taskTitle ? truncate(taskTitle, SUMMARY_MAX) : "idle",
  );

  const taskTitleRef = useRef<string | null>(taskTitle);
  useEffect(() => {
    taskTitleRef.current = taskTitle;
  }, [taskTitle]);

  useEffect(() => {
    let disposed = false;
    let unsub: (() => void) | null = null;
    const decoder = new TextDecoder("utf-8", { fatal: false });
    let buf = "";
    let lastLine: string | null = null;
    let lastStdoutAt = 0;
    let pending: number | null = null;

    const recompute = () => {
      pending = null;
      const silent = Date.now() - lastStdoutAt > IDLE_AFTER_MS;
      let next: string;
      if (lastLine && !silent) next = truncate(lastLine, SUMMARY_MAX);
      else if (taskTitleRef.current) next = truncate(taskTitleRef.current, SUMMARY_MAX);
      else next = "idle";
      setSummary((prev) => (prev === next ? prev : next));
    };

    const schedule = () => {
      if (pending != null) return;
      pending = window.setTimeout(recompute, THROTTLE_MS);
    };

    // B-1.4: if the IPC subscription resolves AFTER unmount, drop the
    // listener immediately so we don't double-fire and so the cleanup
    // closure doesn't capture stale state.
    onAgentStdout((e) => {
      if (disposed) return;
      if (e.agent_id !== agentId) return;
      const text = decoder.decode(fromB64(e.data_b64), { stream: true });
      const merged = buf + text;
      buf = merged.length > BUFFER_LIMIT ? merged.slice(-BUFFER_LIMIT) : merged;
      const line = lastMeaningfulLine(buf);
      if (line) {
        lastLine = line;
        lastStdoutAt = Date.now();
      }
      schedule();
    }).then((u) => {
      if (disposed) u();
      else unsub = u;
    });

    const tickId = window.setInterval(schedule, 1000);
    schedule();

    return () => {
      disposed = true;
      if (unsub) unsub();
      if (pending != null) clearTimeout(pending);
      window.clearInterval(tickId);
    };
  }, [agentId]);

  return summary;
}
