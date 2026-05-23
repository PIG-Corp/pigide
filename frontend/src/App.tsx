import { useEffect, useRef } from "react";
import { Allotment } from "allotment";
import "allotment/dist/style.css";
import "./styles/tokens.css";
import "./styles.css";
import { WorkspaceSidebar } from "./components/WorkspaceSidebar";
import { TilingArea } from "./components/TilingArea";
import { OrchestratorPanel } from "./components/OrchestratorPanel";
import { VoicePill } from "./components/voice/VoicePill";
import { HotkeyBindings } from "./components/HotkeyBindings";
import { SettingsButton } from "./components/SettingsButton";
import { useStore } from "./state/store";
import { useThemeBootstrap } from "./themes/useTheme";
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
} from "./state/ipc";
import { closeLeaf } from "./layout/tree";

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
  const pushToast = useStore((s) => s.pushToast);
  const toasts = useStore((s) => s.toasts);
  const dismissToast = useStore((s) => s.dismissToast);

  useThemeBootstrap();

  // U-89 / H-30: cancellation ref so concurrent workspace switches don't
  // corrupt state — each switch increments the id; stale continuations bail out.
  const switchIdRef = useRef(0);

  // Helper used by the workspace://changed handler when the orchestrator
  // creates/switches a workspace under us. Note: chat is GLOBAL, not reloaded.
  async function reloadAfterSwitch(newId: string) {
    const switchId = ++switchIdRef.current;
    try {
      const list = await ipc.listWorkspaces();
      if (switchIdRef.current !== switchId) return;
      setWorkspaces(list);
      setCurrent(newId);
      const ws = await ipc.getWorkspace(newId);
      if (switchIdRef.current !== switchId) return;
      setLayout(ws.layout);
      const agents = await ipc.listAgents(newId);
      if (switchIdRef.current !== switchId) return;
      setAgents(agents);
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
        // Orchestrator chat is global — load it independently of workspace.
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
  }, [setAgents, setChat, setCurrent, setLayout, setQueue, setWorkspaces]);

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
      appendChatChunk(e.id, e.delta);
    }));
    track(onChatStatus((e) => {
      setOrchestratorStatus(e.state);
    }));
    track(onChatQueue((e) => {
      setQueue(e.items, e.pending);
    }));
    track(onVoiceState((e) => {
      setVoiceState(e.state);
    }));
    track(onVoiceTranscript((e) => {
      if (e.text) appendDraftInput(e.text);
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
              | "kiro-cli" | "claude" | "opencode" | "devin" | "agy";
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
          <WorkspaceSidebar />
        </Allotment.Pane>
        <Allotment.Pane minSize={300}>
          <TilingArea />
        </Allotment.Pane>
        <Allotment.Pane minSize={260} preferredSize={320}>
          <OrchestratorPanel />
        </Allotment.Pane>
      </Allotment>

      <SettingsButton />

      <div className="toast-wrap" role="status" aria-live="polite">
        {toasts.map((t) => (
          <div
            key={t.id}
            className={`toast ${t.kind}`}
            onClick={() => dismissToast(t.id)}
            role="alert"
          >
            {t.text}
          </div>
        ))}
      </div>

      <VoicePill />
    </div>
  );
}
