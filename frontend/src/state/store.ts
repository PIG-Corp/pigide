import { create } from "zustand";
import type {
  Agent,
  ChatMessage,
  LayoutNode,
  OrchestratorStatus,
  QueueItem,
  Task,
  VoiceState,
  Workspace,
} from "./types";

interface ToastEntry {
  id: string;
  text: string;
  kind: "info" | "error";
}

interface AppStateShape {
  // workspaces
  workspaces: Workspace[];
  currentId: string | null;
  layout: LayoutNode;
  agents: Record<string, Agent>;
  focusedLeafId: string | null;
  maximizedLeafId: string | null;

  // chat
  chat: ChatMessage[];
  orchestratorStatus: OrchestratorStatus;
  draftInput: string;
  queueItems: QueueItem[];
  queuePending: number;

  // voice
  voiceState: VoiceState;
  voiceModelDownload: { bytes: number; total: number } | null;

  // tasks (filtered to current workspace)
  tasks: Record<string, Task>;
  showKanban: boolean;
  newWorkspaceModalOpen: boolean;

  // toasts
  toasts: ToastEntry[];

  // setters
  setWorkspaces: (ws: Workspace[]) => void;
  setCurrent: (id: string | null) => void;
  setLayout: (layout: LayoutNode) => void;
  setAgents: (agents: Agent[]) => void;
  upsertAgent: (a: Agent) => void;
  removeAgent: (id: string) => void;

  setFocusedLeaf: (id: string | null) => void;
  setMaximized: (id: string | null) => void;

  setChat: (msgs: ChatMessage[]) => void;
  upsertChatMessage: (m: ChatMessage) => void;
  appendChatChunk: (id: string, delta: string) => void;
  setOrchestratorStatus: (s: OrchestratorStatus) => void;
  setDraftInput: (s: string) => void;
  appendDraftInput: (s: string) => void;
  setQueue: (items: QueueItem[], pending: number) => void;

  setVoiceState: (s: VoiceState) => void;
  setVoiceDownload: (d: { bytes: number; total: number } | null) => void;

  setTasks: (list: Task[]) => void;
  upsertTask: (t: Task) => void;
  removeTask: (id: string) => void;
  setShowKanban: (v: boolean) => void;
  setNewWorkspaceModalOpen: (v: boolean) => void;

  pushToast: (t: Omit<ToastEntry, "id">) => void;
  dismissToast: (id: string) => void;
}

export const useStore = create<AppStateShape>((set) => ({
  workspaces: [],
  currentId: null,
  layout: { type: "empty" },
  agents: {},
  focusedLeafId: null,
  maximizedLeafId: null,

  chat: [],
  orchestratorStatus: "idle",
  draftInput: "",
  queueItems: [],
  queuePending: 0,

  voiceState: "idle",
  voiceModelDownload: null,

  tasks: {},
  showKanban: false,
  newWorkspaceModalOpen: false,

  toasts: [],

  setWorkspaces: (ws) => set({ workspaces: ws }),
  setCurrent: (id) => set({ currentId: id }),
  setLayout: (layout) => set({ layout }),
  setAgents: (list) =>
    set(() => ({ agents: Object.fromEntries(list.map((a) => [a.id, a])) })),
  upsertAgent: (a) =>
    set((s) => ({ agents: { ...s.agents, [a.id]: a } })),
  removeAgent: (id) =>
    set((s) => {
      const next = { ...s.agents };
      delete next[id];
      return { agents: next };
    }),

  setFocusedLeaf: (id) => set({ focusedLeafId: id }),
  setMaximized: (id) => set({ maximizedLeafId: id }),

  setChat: (msgs) => set({ chat: msgs }),
  upsertChatMessage: (m) =>
    set((s) => {
      const idx = s.chat.findIndex((x) => x.id === m.id);
      if (idx >= 0) {
        const next = s.chat.slice();
        next[idx] = m;
        return { chat: next };
      }
      return { chat: [...s.chat, m] };
    }),
  appendChatChunk: (id, delta) =>
    set((s) => {
      const idx = s.chat.findIndex((x) => x.id === id);
      if (idx < 0) {
        const stub: ChatMessage = {
          id,
          role: "assistant",
          content: delta,
          created_at: new Date().toISOString(),
        };
        return { chat: [...s.chat, stub] };
      }
      const next = s.chat.slice();
      next[idx] = { ...next[idx], content: next[idx].content + delta };
      return { chat: next };
    }),
  setOrchestratorStatus: (status) => set({ orchestratorStatus: status }),
  setDraftInput: (s) => set({ draftInput: s }),
  appendDraftInput: (text) =>
    set((s) => ({
      draftInput: s.draftInput
        ? s.draftInput.endsWith(" ") || !text
          ? s.draftInput + text
          : s.draftInput + " " + text
        : text,
    })),
  setQueue: (items, pending) => set({ queueItems: items, queuePending: pending }),

  setVoiceState: (state) => set({ voiceState: state }),
  setVoiceDownload: (d) => set({ voiceModelDownload: d }),

  setTasks: (list) =>
    set(() => ({ tasks: Object.fromEntries(list.map((t) => [t.id, t])) })),
  upsertTask: (t) =>
    set((s) => ({ tasks: { ...s.tasks, [t.id]: t } })),
  removeTask: (id) =>
    set((s) => {
      const next = { ...s.tasks };
      delete next[id];
      return { tasks: next };
    }),
  setShowKanban: (v) => set({ showKanban: v }),
  setNewWorkspaceModalOpen: (v) => set({ newWorkspaceModalOpen: v }),

  pushToast: (t) =>
    set((s) => ({
      toasts: [
        ...s.toasts,
        { id: Math.random().toString(36).slice(2), ...t },
      ],
    })),
  dismissToast: (id) =>
    set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
}));
