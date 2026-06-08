import { Allotment } from "allotment";
import "allotment/dist/style.css";
import { useStore } from "../state/store";
import { AgentTile } from "./AgentTile";
import { KanbanBoard } from "./KanbanBoard";
import { PigMemoryWorkbench } from "./pigmemory/PigMemoryWorkbench";
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
  const showTaskBoard = useStore((s) => s.showTaskBoard);
  const setShowTaskBoard = useStore((s) => s.setShowTaskBoard);
  const showPigMemory = useStore((s) => s.showPigMemory);
  const setShowPigMemory = useStore((s) => s.setShowPigMemory);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // B-1.1 / H-31: capture the workspaceId the layout belongs to, so a
  // debounced flush that fires after the user switched workspaces doesn't
  // overwrite the new workspace's layout with the old one's.
  const pendingLayoutRef = useRef<{ workspaceId: string; layout: LayoutNode } | null>(null);
  const [rooms, setRooms] = useState<RoomTemplate[]>([]);
  const [roomMenuOpen, setRoomMenuOpen] = useState(false);

  useEffect(() => {
    ipc.listRoomTemplates().then(setRooms).catch(() => undefined);
  }, []);

  // B-1.1: when the user switches workspaces, drop any pending debounced
  // write — that layout belonged to the old workspace, not the new one.
  useEffect(() => {
    return () => {
      if (debounceRef.current) {
        clearTimeout(debounceRef.current);
        debounceRef.current = null;
        pendingLayoutRef.current = null;
      }
    };
  }, [currentId]);

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
    // B-1.1: snapshot the workspaceId alongside the layout so the deferred
    // IPC write can't accidentally hit a different workspace.
    pendingLayoutRef.current = { workspaceId: currentId, layout: next };
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => {
      const pending = pendingLayoutRef.current;
      debounceRef.current = null;
      pendingLayoutRef.current = null;
      if (!pending) return;
      ipc.updateLayout(pending.workspaceId, pending.layout).catch(() => undefined);
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
            <button onClick={() => spawn("opencode")}>+ opencode</button>
            <button onClick={() => spawn("devin")}>+ devin</button>
            <button onClick={() => spawn("agy")}>+ agy</button>
            <button onClick={() => spawn("codex")}>+ codex</button>
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
      // Force a fresh mount per (workspace, agent). Without this React will
      // happily reuse the AgentTile instance when the layout tree changes
      // (e.g. workspace switch reuses an agent_id or just remounts with a
      // new agent object): xterm.js never re-runs its mount effect, the
      // scrollback replay is skipped, and the terminal stays sized to the
      // old ratio. The key makes the old tile unmount → new tile mount
      // → agentLogTail + FitAddon.fit run cleanly.
      return (
        <AgentTile
          key={`${currentId ?? "_"}:${agent.id}`}
          agent={agent}
          isFocused={focusedLeafId === agent.id}
          isMaximized={maximizedLeafId === agent.id}
        />
      );
    }
    // split
    return (
      <Allotment
        // B-9.3: stable key per split-path so the Allotment instance
        // survives a parent re-render unless the tree-shape actually
        // changed. Without this, every layout setLayout() re-mounts the
        // Allotment and resets the user's drag.
        key={`split:${path.join("")}`}
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

  const spawn = async (kind: "kiro-cli" | "claude" | "opencode" | "devin" | "agy" | "codex") => {
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

  // PigMemory takes over the whole canvas when active.
  if (showPigMemory) {
    return (
      <div className="tiling-area">
        <div className="tiling-area-toolbar">
          <button
            className="task-board-toggle active"
            onClick={() => setShowPigMemory(false)}
            title="Close PigMemory"
          >
            ← Back
          </button>
          <span className="spacer" />
          <span className="tiling-area-meta">PigMemory</span>
        </div>
        <div className="tiling-area-canvas">
          <PigMemoryWorkbench />
        </div>
      </div>
    );
  }

  // If we have a maximized leaf, short-circuit to render only it.
  if (maximizedLeafId && agents[maximizedLeafId]) {
    return (
      <div className="tiling-area">
        <div className="tiling-area-toolbar">
          <button onClick={() => spawn("kiro-cli")}>+ kiro-cli</button>
          <button onClick={() => spawn("claude")}>+ claude</button>
          <button onClick={() => spawn("opencode")}>+ opencode</button>
          <button onClick={() => spawn("devin")}>+ devin</button>
          <button onClick={() => spawn("agy")}>+ agy</button>
          <button onClick={() => spawn("codex")}>+ codex</button>
          <span className="spacer" />
          <span className="tiling-area-meta">
            maximized: {maximizedLeafId.slice(0, 8)}
          </span>
        </div>
        {/*
          key forces the maximized AgentTile to remount on workspace switch
          (xterm.js re-runs its mount effect, scrollback is replayed, and
          the ResizeObserver is reattached at the new dimensions).
        */}
        <div className="tiling-area-canvas" key={currentId ?? "_"}>
          <AgentTile
            key={`${currentId ?? "_"}:${agents[maximizedLeafId].id}`}
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
        <button onClick={() => spawn("opencode")}>+ opencode</button>
        <button onClick={() => spawn("devin")}>+ devin</button>
        <button onClick={() => spawn("agy")}>+ agy</button>
        <button onClick={() => spawn("codex")}>+ codex</button>
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
          className={`task-board-toggle ${showTaskBoard ? "active" : ""}`}
          onClick={() => setShowTaskBoard(!showTaskBoard)}
          title="Toggle Task Board"
        >
          Task Board
        </button>
        <button
          className={`task-board-toggle ${showPigMemory ? "active" : ""}`}
          onClick={() => setShowPigMemory(true)}
          title="Open PigMemory"
        >
          PigMemory
        </button>
        <span className="tiling-area-meta">
          tiles: {Object.keys(agents).length}
        </span>
      </div>
      {/*
        key on the canvas forces the Allotment tree to remount on workspace
        switch. Allotment only honours defaultSizes on first mount, so
        without this key the new split ratios from ws.layout never take
        effect and the new AgentTiles keep the previous sizing. The
        canvas key is also the cheapest way to remount every leaf-level
        AgentTile (it owns the renderNode tree), which re-runs their
        xterm mount effect, scrollback replay, and ResizeObserver.
      */}
      <div className="tiling-area-canvas" key={currentId ?? "_"}>
        {showTaskBoard ? (
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
