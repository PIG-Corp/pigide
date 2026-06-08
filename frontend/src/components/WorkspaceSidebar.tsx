import { useState } from "react";
import { useStore } from "../state/store";
import { ipc } from "../state/ipc";
import { Pencil, Settings, Trash2 } from "./icons";
import { ThemePicker } from "./ThemePicker";
import { NewWorkspaceModal } from "./NewWorkspaceModal";
import { useTheme } from "../themes/useTheme";
import { useSwitchWorkspace } from "../state/useSwitchWorkspace";

const STATUS_COLORS = ["#4ADE80", "#60A5FA", "#E89A4A", "#EF4444", "#C084FC"];

function getStatusColor(index: number): string {
  return STATUS_COLORS[index % STATUS_COLORS.length];
}

export function WorkspaceSidebar() {
  const workspaces = useStore((s) => s.workspaces);
  const currentId = useStore((s) => s.currentId);
  const setWorkspaces = useStore((s) => s.setWorkspaces);
  const setCurrent = useStore((s) => s.setCurrent);
  const setLayout = useStore((s) => s.setLayout);
  const setAgents = useStore((s) => s.setAgents);
  const pushToast = useStore((s) => s.pushToast);
  const newOpen = useStore((s) => s.newWorkspaceModalOpen);
  const setNewOpen = useStore((s) => s.setNewWorkspaceModalOpen);
  const switchWorkspace = useSwitchWorkspace();
  const [pickerOpen, setPickerOpen] = useState(false);
  const [tooltip, setTooltip] = useState<{ text: string; x: number; y: number } | null>(null);
  const { theme } = useTheme();

  const reload = async () => {
    try {
      const list = await ipc.listWorkspaces();
      setWorkspaces(list);
    } catch (err) {
      pushToast({ text: `Reload failed: ${err}`, kind: "error" });
    }
  };

  const switchTo = (id: string) => {
    void switchWorkspace(id);
  };

  const create = () => setNewOpen(true);

  const onWorkspaceCreated = async (id: string) => {
    try {
      const list = await ipc.listWorkspaces();
      setWorkspaces(list);
      await switchWorkspace(id);
    } catch (err) {
      pushToast({ text: `Reload failed: ${err}`, kind: "error" });
    }
  };

  const rename = async (id: string, current: string) => {
    const name = prompt("New name", current);
    if (!name || name === current) return;
    try {
      await ipc.renameWorkspace(id, name);
      await reload();
    } catch (err) {
      pushToast({ text: `Rename failed: ${err}`, kind: "error" });
    }
  };

  const remove = async (id: string) => {
    if (!confirm("Delete this workspace and kill all its agents?")) return;
    try {
      await ipc.deleteWorkspace(id);
      await reload();
      const remaining = (await ipc.listWorkspaces())[0];
      if (remaining) await switchWorkspace(remaining.id);
      else {
        setCurrent(null);
        setLayout({ type: "empty" });
        setAgents([]);
      }
    } catch (err) {
      pushToast({ text: `Delete failed: ${err}`, kind: "error" });
    }
  };

  const showTooltip = (e: React.MouseEvent, paths: string[] | undefined) => {
    if (!paths || paths.length === 0) return;
    // B-12.42: collapse the user's $HOME prefix to `~` so a screencast or
    // shoulder-surf doesn't leak the full path. Anything that doesn't
    // start with `/home/` is left as-is (e.g. `/var`, `/opt`).
    const HOME_PREFIX = "/home/";
    const homeUser = paths.find((p) => p.startsWith(HOME_PREFIX));
    const homeUserSegment = homeUser
      ? HOME_PREFIX + (homeUser.split("/")[2] ?? "") + "/"
      : null;
    const safePaths = paths.map((p) => {
      if (homeUserSegment && p.startsWith(homeUserSegment)) {
        return "~/" + p.slice(homeUserSegment.length);
      }
      return p;
    });
    // B-12.37: clamp the tooltip position to the viewport so it doesn't
    // bleed off the right/bottom edge.
    const margin = 8;
    const tipW = 320; // rough max width; CSS will truncate
    const tipH = 60;
    const x = Math.max(margin, Math.min(window.innerWidth - tipW - margin, e.clientX + 12));
    const y = Math.max(margin, Math.min(window.innerHeight - tipH - margin, e.clientY - 4));
    setTooltip({ text: safePaths.join(", "), x, y });
  };

  const hideTooltip = () => setTooltip(null);

  return (
    <div className="workspace-sidebar">
      <div className="workspace-sidebar-header">
        <span>Workspaces</span>
        <button
          title={`Theme: ${theme.name}`}
          onClick={() => setPickerOpen(true)}
          style={{ marginRight: 4 }}
        >
          <Settings size={11} />
        </button>
        <button title="New workspace" onClick={create}>+</button>
      </div>
      <div className="workspace-list">
        {workspaces.length === 0 ? (
          <div className="empty-state">
            No workspaces yet.<br />
            <button style={{ marginTop: 10 }} onClick={create}>Create one</button>
          </div>
        ) : null}
        {workspaces.map((ws, i) => (
          <div
            key={ws.id}
            className={`workspace-item ${currentId === ws.id ? "active" : ""}`}
            onClick={() => switchTo(ws.id)}
            onMouseMove={(e) => showTooltip(e, ws.paths)}
            onMouseLeave={hideTooltip}
          >
            <span
              className="ws-status-dot"
              style={{ background: getStatusColor(i) }}
            />
            <span className="name">{ws.name}</span>
            <span className="ws-pill-badge">{ws.agent_count}</span>
            <span className="workspace-actions">
              <button
                title="Rename"
                onClick={(e) => {
                  e.stopPropagation();
                  rename(ws.id, ws.name);
                }}
              >
                <Pencil size={11} />
              </button>
              <button
                title="Delete"
                onClick={(e) => {
                  e.stopPropagation();
                  remove(ws.id);
                }}
              >
                <Trash2 size={11} />
              </button>
            </span>
          </div>
        ))}
      </div>

      {tooltip && (
        <div
          className="ws-tooltip"
          style={{ left: tooltip.x, top: tooltip.y }}
        >
          {tooltip.text}
        </div>
      )}

      {pickerOpen && <ThemePicker onClose={() => setPickerOpen(false)} />}
      {newOpen && (
        <NewWorkspaceModal
          onClose={() => setNewOpen(false)}
          onCreated={onWorkspaceCreated}
        />
      )}
    </div>
  );
}
