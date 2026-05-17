import { useEffect, useState } from "react";
import { Allotment } from "allotment";
import "allotment/dist/style.css";
import "./styles/tokens.css";
import "./styles.css";
import { WorkspaceSidebar } from "./components/WorkspaceSidebar";
import { TilingArea } from "./components/TilingArea";
import { OrchestratorPanel } from "./components/OrchestratorPanel";
import { MemoryPanel } from "./components/MemoryPanel";
import { SkillsPanel } from "./components/SkillsPanel";
import { VoicePanel } from "./components/voice/VoicePanel";
import { VoicePill } from "./components/voice/VoicePill";
import { BrowserPanel } from "./components/BrowserPanel";
import { FilesPanel } from "./components/FilesPanel";
import { PromptsPanel } from "./components/PromptsPanel";
import { AgentConfigPanel } from "./components/AgentConfigPanel";
import { SshPresetsPanel } from "./components/SshPresetsPanel";
import { ArchitectPanel } from "./components/ArchitectPanel";
import { HotkeyBindings } from "./components/HotkeyBindings";
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

export default function App() {
  const setWorkspaces = useStore((s) => s.setWorkspaces);
  const setCurrent = useStore((s) => s.setCurrent);
  const setLayout = useStore((s) => s.setLayout);
  const setAgents = useStore((s) => s.setAgents);
  const setChat = useStore((s) => s.setChat);
  const removeAgent = useStore((s) => s.removeAgent);
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

  // Initial load.
  useEffect(() => {
    (async () => {
      try {
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

  // Helper used by the workspace://changed handler when the orchestrator
  // creates/switches a workspace under us. Note: chat is GLOBAL, not reloaded.
  async function reloadAfterSwitch(newId: string) {
    try {
      const list = await ipc.listWorkspaces();
      setWorkspaces(list);
      setCurrent(newId);
      const ws = await ipc.getWorkspace(newId);
      setLayout(ws.layout);
      const agents = await ipc.listAgents(newId);
      setAgents(agents);
    } catch (err) {
      console.error("reloadAfterSwitch", err);
    }
  }

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
      removeAgent(e.agent_id);
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
              | "kiro-cli" | "claude" | "aider" | "goose" | "opencode" | "devin";
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
      <Allotment defaultSizes={[220, 800, 360]}>
        <Allotment.Pane minSize={160} preferredSize={220}>
          <WorkspaceSidebar />
        </Allotment.Pane>
        <Allotment.Pane minSize={400}>
          <TilingArea />
        </Allotment.Pane>
        <Allotment.Pane minSize={260} preferredSize={360}>
          <RightPane />
        </Allotment.Pane>
      </Allotment>

      <div className="toast-wrap">
        {toasts.map((t) => (
          <div
            key={t.id}
            className={`toast ${t.kind}`}
            onClick={() => dismissToast(t.id)}
          >
            {t.text}
          </div>
        ))}
      </div>

      <VoicePill />
    </div>
  );
}

type RightTab =
  | "chat"
  | "architect"
  | "memory"
  | "skills"
  | "voice"
  | "browser"
  | "files"
  | "prompts"
  | "agents"
  | "ssh";

function RightPane() {
  const [tab, setTab] = useState<RightTab>("chat");
  return (
    <div className="right-pane">
      <div className="right-pane-tabs">
        <button
          className={`right-pane-tab ${tab === "chat" ? "active" : ""}`}
          onClick={() => setTab("chat")}
        >
          Chat
        </button>
        <button
          className={`right-pane-tab ${tab === "architect" ? "active" : ""}`}
          onClick={() => setTab("architect")}
          title="Always-On Architect"
        >
          Architect
        </button>
        <button
          className={`right-pane-tab ${tab === "memory" ? "active" : ""}`}
          onClick={() => setTab("memory")}
        >
          Memory
        </button>
        <button
          className={`right-pane-tab ${tab === "skills" ? "active" : ""}`}
          onClick={() => setTab("skills")}
          title="Architect prompt-skills"
        >
          Skills
        </button>
        <button
          className={`right-pane-tab ${tab === "voice" ? "active" : ""}`}
          onClick={() => setTab("voice")}
        >
          Voice
        </button>
        <button
          className={`right-pane-tab ${tab === "browser" ? "active" : ""}`}
          onClick={() => setTab("browser")}
        >
          Web
        </button>
        <button
          className={`right-pane-tab ${tab === "files" ? "active" : ""}`}
          onClick={() => setTab("files")}
        >
          Files
        </button>
        <button
          className={`right-pane-tab ${tab === "prompts" ? "active" : ""}`}
          onClick={() => setTab("prompts")}
        >
          Prompts
        </button>
        <button
          className={`right-pane-tab ${tab === "agents" ? "active" : ""}`}
          onClick={() => setTab("agents")}
          title="Per-role system prompt overrides"
        >
          Agents
        </button>
        <button
          className={`right-pane-tab ${tab === "ssh" ? "active" : ""}`}
          onClick={() => setTab("ssh")}
          title="SSH connection presets"
        >
          SSH
        </button>
      </div>
      <div className="right-pane-body">
        {tab === "chat" ? <OrchestratorPanel /> : null}
        {tab === "architect" ? <ArchitectPanel /> : null}
        {tab === "memory" ? <MemoryPanel /> : null}
        {tab === "skills" ? <SkillsPanel /> : null}
        {tab === "voice" ? <VoicePanel /> : null}
        {tab === "browser" ? <BrowserPanel /> : null}
        {tab === "files" ? <FilesPanel /> : null}
        {tab === "prompts" ? <PromptsPanel /> : null}
        {tab === "agents" ? <AgentConfigPanel /> : null}
        {tab === "ssh" ? <SshPresetsPanel /> : null}
      </div>
    </div>
  );
}
