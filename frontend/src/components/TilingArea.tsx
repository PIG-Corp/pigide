import { Allotment } from "allotment";
import "allotment/dist/style.css";
import { useStore } from "../state/store";
import { AgentTile } from "./AgentTile";
import { KanbanBoard } from "./KanbanBoard";
import { ipc } from "../state/ipc";
import { setRatioAt } from "../layout/tree";
import type { LayoutNode, RoomTemplate } from "../state/types";
import { useEffect, useRef, useState, type ReactElement } from "react";

export function TilingArea() {
  const layout = useStore((s) => s.layout);
  const agents = useStore((s) => s.agents);
  const focusedLeafId = useStore((s) => s.focusedLeafId);
  const maximizedLeafId = useStore((s) => s.maximizedLeafId);
  const setLayout = useStore((s) => s.setLayout);
  const currentId = useStore((s) => s.currentId);
  const upsertAgent = useStore((s) => s.upsertAgent);
  const pushToast = useStore((s) => s.pushToast);
  const showKanban = useStore((s) => s.showKanban);
  const setShowKanban = useStore((s) => s.setShowKanban);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [rooms, setRooms] = useState<RoomTemplate[]>([]);
  const [roomMenuOpen, setRoomMenuOpen] = useState(false);

  useEffect(() => {
    ipc.listRoomTemplates().then(setRooms).catch(() => undefined);
  }, []);

  const applyRoom = async (templateId: string) => {
    if (!currentId) return;
    setRoomMenuOpen(false);
    try {
      await ipc.applyRoomTemplate(currentId, templateId);
      const ws = await ipc.getWorkspace(currentId);
      setLayout(ws.layout);
      const list = await ipc.listAgents(currentId);
      list.forEach(upsertAgent);
    } catch (err) {
      pushToast({ text: `apply room: ${err}`, kind: "error" });
    }
  };

  const persistLayout = (next: LayoutNode) => {
    if (!currentId) return;
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => {
      ipc.updateLayout(currentId, next).catch(() => undefined);
    }, 200);
  };

  const setRatio = (path: ("a" | "b")[], ratio: number) => {
    const next = setRatioAt(layout, path, ratio);
    setLayout(next);
    persistLayout(next);
  };

  const renderNode = (node: LayoutNode, path: ("a" | "b")[] = []): ReactElement => {
    if (node.type === "empty") {
      return (
        <div className="tiling-area-empty">
          <div>Workspace is empty.</div>
          <div className="actions">
            <button onClick={() => spawn("kiro-cli")}>+ kiro-cli</button>
            <button onClick={() => spawn("claude")}>+ claude</button>
            <button onClick={() => spawn("aider")}>+ aider</button>
            <button onClick={() => spawn("goose")}>+ goose</button>
            <button onClick={() => spawn("opencode")}>+ opencode</button>
            <button onClick={() => spawn("devin")}>+ devin</button>
          </div>
        </div>
      );
    }
    if (node.type === "leaf") {
      const agent = agents[node.agentId];
      if (!agent) {
        return (
          <div className="tiling-area-empty">
            <div>Loading agent {node.agentId.slice(0, 8)}…</div>
          </div>
        );
      }
      return (
        <AgentTile
          agent={agent}
          isFocused={focusedLeafId === agent.id}
          isMaximized={maximizedLeafId === agent.id}
        />
      );
    }
    // split
    return (
      <Allotment
        vertical={node.direction === "h"}
        defaultSizes={[node.ratio * 100, (1 - node.ratio) * 100]}
        onChange={(sizes) => {
          const total = sizes[0] + sizes[1];
          if (total <= 0) return;
          const r = sizes[0] / total;
          // Avoid a re-render storm: only update if delta significant.
          if (Math.abs(r - node.ratio) > 0.005) setRatio(path, r);
        }}
        proportionalLayout
      >
        <Allotment.Pane minSize={120}>{renderNode(node.a, [...path, "a"])}</Allotment.Pane>
        <Allotment.Pane minSize={120}>{renderNode(node.b, [...path, "b"])}</Allotment.Pane>
      </Allotment>
    );
  };

  const spawn = async (kind: "kiro-cli" | "claude" | "aider" | "goose" | "opencode" | "devin") => {
    if (!currentId) return;
    try {
      const list = await ipc.spawnAgent(currentId, kind);
      list.forEach(upsertAgent);
      const ws = await ipc.getWorkspace(currentId);
      setLayout(ws.layout);
    } catch (err) {
      pushToast({ text: `Spawn failed: ${err}`, kind: "error" });
    }
  };

  // If we have a maximized leaf, short-circuit to render only it.
  if (maximizedLeafId && agents[maximizedLeafId]) {
    return (
      <div className="tiling-area">
        <div className="tiling-area-toolbar">
          <button onClick={() => spawn("kiro-cli")}>+ kiro-cli</button>
          <button onClick={() => spawn("claude")}>+ claude</button>
          <button onClick={() => spawn("aider")}>+ aider</button>
          <button onClick={() => spawn("goose")}>+ goose</button>
          <button onClick={() => spawn("opencode")}>+ opencode</button>
          <button onClick={() => spawn("devin")}>+ devin</button>
          <span className="spacer" />
          <span className="tiling-area-meta">
            maximized: {maximizedLeafId.slice(0, 8)}
          </span>
        </div>
        <div className="tiling-area-canvas">
          <AgentTile
            agent={agents[maximizedLeafId]}
            isFocused
            isMaximized
          />
        </div>
      </div>
    );
  }

  return (
    <div className="tiling-area">
      <div className="tiling-area-toolbar">
        <button onClick={() => spawn("kiro-cli")}>+ kiro-cli</button>
        <button onClick={() => spawn("claude")}>+ claude</button>
        <button onClick={() => spawn("aider")}>+ aider</button>
        <button onClick={() => spawn("goose")}>+ goose</button>
        <button onClick={() => spawn("opencode")}>+ opencode</button>
        <button onClick={() => spawn("devin")}>+ devin</button>
        <span className="spacer" />
        <div className="room-menu-wrap">
          <button
            onClick={() => setRoomMenuOpen((v) => !v)}
            disabled={!currentId || rooms.length === 0}
            title="Apply a room template"
          >
            Rooms ▾
          </button>
          {roomMenuOpen ? (
            <div className="room-menu">
              {rooms.map((r) => (
                <button
                  key={r.id}
                  className="room-menu-item"
                  onClick={() => applyRoom(r.id)}
                  title={r.description}
                >
                  <div className="room-menu-name">{r.name}</div>
                  <div className="room-menu-desc">{r.description}</div>
                </button>
              ))}
            </div>
          ) : null}
        </div>
        <button
          className={`kanban-toggle ${showKanban ? "active" : ""}`}
          onClick={() => setShowKanban(!showKanban)}
          title="Toggle Kanban board"
        >
          Kanban
        </button>
        <span className="tiling-area-meta">
          tiles: {Object.keys(agents).length}
        </span>
      </div>
      <div className="tiling-area-canvas">
        {showKanban ? (
          <Allotment vertical defaultSizes={[60, 40]}>
            <Allotment.Pane minSize={120}>{renderNode(layout)}</Allotment.Pane>
            <Allotment.Pane minSize={160} preferredSize={300}>
              <KanbanBoard />
            </Allotment.Pane>
          </Allotment>
        ) : (
          renderNode(layout)
        )}
      </div>
    </div>
  );
}
