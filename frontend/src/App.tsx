import { useEffect, useRef } from "react";
import { Allotment } from "allotment";
import "allotment/dist/style.css";
import "./styles/tokens.css";
import "./styles.css";
import "./styles/pigmemory.css";
import { WorkspaceSidebar } from "./components/WorkspaceSidebar";
import { TilingArea } from "./components/TilingArea";
import { OrchestratorPanel } from "./components/OrchestratorPanel";
import { VoicePill } from "./components/voice/VoicePill";
import { HotkeyBindings } from "./components/HotkeyBindings";
import { SettingsButton } from "./components/SettingsButton";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { useStore } from "./state/store";
import { useThemeBootstrap } from "./themes/useTheme";
import type { Task } from "./state/types";
import {
  ipc,
  onAgentExit,
  onAgentSpawned,
  onChatChunk,
  onChatMessage,
  onChatQueue,
  onChatStatus,
  onDeepLink,
  onLayoutChanged,
  onVoiceDownload,
  onVoiceState,
  onVoiceTranscript,
  onWorkspaceChanged,
  onChatScopeChanged,
} from "./state/ipc";
import { closeLeaf } from "./layout/tree";
import { injectVoiceTranscript } from "./voice/inject";

export default function App() {
  const setWorkspaces = useStore((s) => s.setWorkspaces);
  const setCurrent = useStore((s) => s.setCurrent);
  const setLayout = useStore((s) => s.setLayout);
  const setAgents = useStore((s) => s.setAgents);
  const setChat = useStore((s) => s.setChat);
  const upsertAgent = useStore((s) => s.upsertAgent);
  const setOrchestratorStatus = useStore((s) => s.setOrchestratorStatus);
  const upsertChat = useStore((s) => s.upsertChatMessage);
  const appendChatChunk = useStore((s) => s.appendChatChunk);
  const setVoiceState = useStore((s) => s.setVoiceState);
  const setVoiceDownload = useStore((s) => s.setVoiceDownload);
  const appendDraftInput = useStore((s) => s.appendDraftInput);
  const setQueue = useStore((s) => s.setQueue);
  const setChatScope = useStore((s) => s.setChatScope);
  const pushToast = useStore((s) => s.pushToast);
  const toasts = useStore((s) => s.toasts);
  const dismissToast = useStore((s) => s.dismissToast);

  useThemeBootstrap();

  // U-89 / H-30: cancellation ref so concurrent workspace switches don't
  // corrupt state — each switch increments the id; stale continuations bail out.
  const switchIdRef = useRef(0);

  // B-9.1: chat-chunk coalescer. The streaming model emits ~60 chunks/sec;
  // we buffer deltas per message id and flush on a 50 ms timer so the store
  // receives ~20 setState calls instead of ~60, and each call only walks
  // the chat array once for a 1-element update.
  const chunkBufRef = useRef<Map<string, string>>(new Map());
  const chunkTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const scheduleChunk = (id: string, delta: string) => {
    chunkBufRef.current.set(id, (chunkBufRef.current.get(id) ?? "") + delta);
    if (chunkTimerRef.current) return;
    chunkTimerRef.current = setTimeout(() => {
      chunkTimerRef.current = null;
      const buf = chunkBufRef.current;
      chunkBufRef.current = new Map();
      for (const [mid, d] of buf) {
        appendChatChunk(mid, d);
      }
    }, 50);
  };
  const clearWorkspaceState = useStore((s) => s.clearWorkspaceState);

  // Helper used by the workspace://changed handler when the orchestrator
  // creates/switches a workspace under us. Note: chat is GLOBAL, not reloaded.
  //
  // Important ordering:
  //   1. Bump switchId so any in-flight reload bails out.
  //   2. Wipe workspace-scoped state (agents/layout/leaf ids) so the canvas
  //      does not keep rendering nodes pointing at agents of the OLD ws while
  //      we await the backend.
  //   3. Fetch the new workspace + agents in order, checking switchId between
  //      every await so a faster switch cancels this one cleanly.
  async function reloadAfterSwitch(newId: string) {
    const switchId = ++switchIdRef.current;
    try {
      const list = await ipc.listWorkspaces();
      if (switchIdRef.current !== switchId) return;
      setWorkspaces(list);
      // Drop everything from the previous workspace BEFORE swapping the id so
      // maximized/focused leaves don't briefly resolve to agents of the wrong
      // workspace (or leak across switches). We don't touch chat / voice /
      // tasks-state here — only canvas/leaf ids.
      clearWorkspaceState();
      setCurrent(newId);
      const ws = await ipc.getWorkspace(newId);
      if (switchIdRef.current !== switchId) return;
      setLayout(ws.layout);
      const agents = await ipc.listAgents(newId);
      if (switchIdRef.current !== switchId) return;
      setAgents(agents);
      // B-3.3: also reload tasks so the TasksCard in OrchestratorPanel does
      // not briefly display tasks from the previous workspace.
      const tasks = await ipc.listTasks({ workspace_id: newId }).catch(() => [] as Task[]);
      if (switchIdRef.current !== switchId) return;
      useStore.getState().setTasks(tasks);
    } catch (err) {
      console.error("reloadAfterSwitch", err);
    }
  }

  // Initial load.
  useEffect(() => {
    (async () => {
      try {
        await ipc.restoreSession().catch((err) => console.error("restore session", err));
        const list = await ipc.listWorkspaces();
        setWorkspaces(list);
        let cur = await ipc.getCurrentWorkspace();
        if (!cur && list.length > 0) {
          cur = list[0].id;
          await ipc.setCurrentWorkspace(cur);
        }
        if (cur) {
          setCurrent(cur);
          const ws = await ipc.getWorkspace(cur);
          setLayout(ws.layout);
          const agents = await ipc.listAgents(cur);
          setAgents(agents);
        }
        // Orchestrator chat is scope-resolved — load initial session scope first.
        try {
          const sess = await ipc.getCurrentSession();
          setChatScope(sess.scope);
        } catch (err) {
          console.error("init session", err);
        }
        const chat = await ipc.listChat();
        setChat(chat);
        // Initial queue snapshot — backend will keep us in sync via events.
        try {
          const items = await ipc.listChatQueue();
          const pending = items.filter((i) => i.status === "queued").length;
          setQueue(items, pending);
        } catch (err) {
          console.error("init queue", err);
        }
      } catch (err) {
        console.error("init", err);
      }
    })();
  }, [setAgents, setChat, setCurrent, setLayout, setQueue, setWorkspaces, setChatScope]);

  // Subscribe to backend events.
  useEffect(() => {
    let disposed = false;
    const unsubs: (() => void)[] = [];
    const track = (p: Promise<() => void>) => {
      p.then((u) => {
        if (disposed) u();
        else unsubs.push(u);
      });
    };
    track(onAgentExit((e) => {
      const cur = useStore.getState();
      const existing = cur.agents[e.agent_id];
      if (existing) {
        upsertAgent({ ...existing, status: "exited" });
      }
      // U-46 / H-31: remove dead agent's tile from the layout tree.
      const currentLayout = useStore.getState().layout;
      const currentWsId = useStore.getState().currentId;
      const nextLayout = closeLeaf(currentLayout, e.agent_id);
      setLayout(nextLayout);
      if (currentWsId) ipc.updateLayout(currentWsId, nextLayout).catch(() => undefined);
      ipc.listWorkspaces().then(setWorkspaces).catch(() => undefined);
    }));
    track(onAgentSpawned((a) => {
      // Only adopt if it belongs to OUR current workspace.
      const cur = useStore.getState().currentId;
      if (cur && a.workspace_id === cur) {
        upsertAgent(a);
      } else {
        // Workspace counts in the sidebar should still update.
        ipc.listWorkspaces().then(setWorkspaces).catch(() => undefined);
      }
    }));
    track(onLayoutChanged((e) => {
      const cur = useStore.getState().currentId;
      if (cur && e.workspace_id === cur) {
        setLayout(e.layout);
      }
    }));
    track(onWorkspaceChanged((e) => {
      if (e.current_workspace_id) {
        reloadAfterSwitch(e.current_workspace_id);
      } else if (e.deleted) {
        // If the deleted workspace was the current one, clear stale state
        // so the UI doesn't keep rendering ghost agents/layout.
        const cur = useStore.getState().currentId;
        if (cur && e.deleted === cur) {
          setCurrent(null);
          setLayout({ type: "empty" });
          setAgents([]);
        }
        ipc.listWorkspaces().then(setWorkspaces).catch(() => undefined);
      }
    }));
    track(onChatMessage((m) => {
      upsertChat(m);
    }));
    track(onChatChunk((e) => {
      // B-9.1: coalesce streaming chunks within a 50 ms window so a 60 fps
      // stream doesn't cause 60 setState calls (each O(n) on chat.length).
      scheduleChunk(e.id, e.delta);
    }));
    track(onChatStatus((e) => {
      setOrchestratorStatus(e.state);
    }));
    track(onChatScopeChanged(async (e) => {
      setChatScope(e.scope);
      try {
        const chat = await ipc.listChat();
        setChat(chat);
        const items = await ipc.listChatQueue();
        const pending = items.filter((i) => i.status === "queued").length;
        setQueue(items, pending);
      } catch (err) {
        console.error("reload chat on scope change", err);
      }
    }));
    track(onChatQueue((e) => {
      setQueue(e.items, e.pending);
    }));
    track(onVoiceState((e) => {
      setVoiceState(e.state);
    }));
    track(onVoiceTranscript((e) => {
      if (e.text) {
        // Route per `voiceTarget` (focused-tile by default). injectVoiceTranscript
        // handles stale focusedLeafId / dead agents by falling back to the
        // orchestrator draft and surfacing a toast.
        void injectVoiceTranscript(e.text);
      }
    }));
    track(onVoiceDownload((e) => {
      setVoiceDownload({ bytes: e.bytes, total: e.total });
    }));
    track(onDeepLink(async (e) => {
      try {
        switch (e.route.kind) {
          case "workspace": {
            await ipc.setCurrentWorkspace(e.route.id);
            await reloadAfterSwitch(e.route.id);
            break;
          }
          case "agent_spawn": {
            const ws = e.route.workspace_id ?? useStore.getState().currentId;
            if (!ws) {
              pushToast({ kind: "error", text: "deep link: no workspace" });
              break;
            }
            const at = e.route.agent_type as
              | "kiro-cli" | "claude" | "opencode" | "devin" | "agy" | "codex";
            await ipc.spawnAgent(ws, at, { cwd: e.route.cwd ?? undefined });
            break;
          }
          case "chat": {
            if (e.route.text) appendDraftInput(e.route.text);
            break;
          }
          default:
            pushToast({ kind: "info", text: `Deep link: ${e.url}` });
        }
      } catch (err) {
        pushToast({ kind: "error", text: `deep_link: ${err}` });
      }
    }));
    return () => {
      disposed = true;
      unsubs.forEach((u) => u());
      // B-9.1: drain + cancel the chunk coalescer timer so we don't fire a
      // setState after unmount.
      if (chunkTimerRef.current) {
        clearTimeout(chunkTimerRef.current);
        chunkTimerRef.current = null;
      }
      const buf = chunkBufRef.current;
      chunkBufRef.current = new Map();
      for (const [mid, d] of buf) appendChatChunk(mid, d);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Auto-dismiss toasts after 4s.
  useEffect(() => {
    if (toasts.length === 0) return;
    const t = setTimeout(() => {
      dismissToast(toasts[0].id);
    }, 4000);
    return () => clearTimeout(t);
  }, [dismissToast, toasts]);

  return (
    <div className="app-shell">
      <HotkeyBindings />
      <Allotment defaultSizes={[220, 600, 320]}>
        <Allotment.Pane minSize={160} preferredSize={220}>
          <ErrorBoundary label="Sidebar">
            <WorkspaceSidebar />
          </ErrorBoundary>
        </Allotment.Pane>
        <Allotment.Pane minSize={300}>
          <ErrorBoundary label="Tiling Area">
            <TilingArea />
          </ErrorBoundary>
        </Allotment.Pane>
        <Allotment.Pane minSize={260} preferredSize={320}>
          <ErrorBoundary label="Orchestrator">
            <OrchestratorPanel />
          </ErrorBoundary>
        </Allotment.Pane>
      </Allotment>

      <SettingsButton />

      <div className="toast-wrap" role="status" aria-live="polite">
        {toasts.map((t) => (
          // B-8.1: each toast declares its own live region. `aria-live` on
          // the parent is "polite", so info toasts won't interrupt
          // speech; we override with `assertive` on error toasts.
          <div
            key={t.id}
            className={`toast ${t.kind}`}
            onClick={() => dismissToast(t.id)}
            role={t.kind === "error" ? "alert" : "status"}
            aria-live={t.kind === "error" ? "assertive" : "polite"}
          >
            {t.text}
          </div>
        ))}
      </div>

      <VoicePill />
    </div>
  );
}
