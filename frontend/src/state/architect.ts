// Always-On Architect — frontend bindings.
// Backend lives in src-tauri/src/architect/. This module owns:
//   * IPC commands (ipc.architect*)
//   * Event listeners (onArchitectDecision, onArchitectSignal)
//   * Zustand slice for the per-agent signal map and decision log

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { create } from "zustand";

export type ArchitectSignal =
  | "working"
  | "idle_done"
  | "awaiting_input"
  | "error"
  | "stuck"
  | "unknown";

export interface ArchitectConfig {
  enabled: boolean;
  poll_interval_ms: number;
  idle_done_after_secs: number;
  stuck_after_secs: number;
  auto_confirm: boolean;
  auto_assign_next: boolean;
}

export type ArchitectDecisionKind =
  | "observe"
  | "auto_confirm"
  | "auto_choose"
  | "assign_next"
  | "ping_stuck"
  | "auto_retry_error"
  | "escalate";

export interface ArchitectDecision {
  at: string;
  agent_id: string;
  workspace_id: string;
  signal: ArchitectSignal;
  kind: ArchitectDecisionKind;
  reason: string;
  quote: string | null;
  auto_executed: boolean;
}

interface SignalEvent {
  workspace_id: string;
  signals: Array<{ agent_id: string; signal: ArchitectSignal }>;
}

export const architectIpc = {
  getConfig: () => invoke<ArchitectConfig>("architect_get_config"),
  setEnabled: (enabled: boolean) =>
    invoke<void>("architect_set_enabled", { args: { enabled } }),
  pause: () => invoke<void>("architect_pause"),
  resume: () => invoke<void>("architect_resume"),
  decisions: (limit?: number) =>
    invoke<ArchitectDecision[]>("architect_decisions", {
      limit: limit ?? null,
    }),
};

export function onArchitectDecision(
  cb: (d: ArchitectDecision) => void,
): Promise<UnlistenFn> {
  return listen<ArchitectDecision>("architect://decision", (e) =>
    cb(e.payload),
  );
}

export function onArchitectSignal(
  cb: (e: SignalEvent) => void,
): Promise<UnlistenFn> {
  return listen<SignalEvent>("architect://signal", (e) => cb(e.payload));
}

interface ArchitectStore {
  enabled: boolean;
  signals: Record<string, ArchitectSignal>;
  decisions: ArchitectDecision[];
  setEnabled: (v: boolean) => void;
  setSignals: (m: Record<string, ArchitectSignal>) => void;
  pushDecision: (d: ArchitectDecision) => void;
  setDecisions: (list: ArchitectDecision[]) => void;
}

export const useArchitectStore = create<ArchitectStore>((set) => ({
  enabled: false,
  signals: {},
  decisions: [],
  setEnabled: (v) => set({ enabled: v }),
  setSignals: (m) => set({ signals: m }),
  pushDecision: (d) =>
    set((s) => ({
      // B-9.7: bumped cap from 199 → 999 so a long-running session
      // doesn't silently drop earlier Architect decisions from the log.
      decisions: [...s.decisions.slice(-999), d],
    })),
  setDecisions: (list) => set({ decisions: list }),
}));

export function signalLabel(sig: ArchitectSignal): string {
  switch (sig) {
    case "working":
      return "working";
    case "idle_done":
      return "idle";
    case "awaiting_input":
      return "awaiting";
    case "error":
      return "error";
    case "stuck":
      return "stuck";
    default:
      return "—";
  }
}

export function signalColor(sig: ArchitectSignal): string {
  switch (sig) {
    case "working":
      return "#3b82f6";
    case "idle_done":
      return "#10b981";
    case "awaiting_input":
      return "#f59e0b";
    case "error":
      return "#ef4444";
    case "stuck":
      return "#a855f7";
    default:
      return "#6b7280";
  }
}
