// B-3.3: shared workspace-switch helper.
//
// `WorkspaceSidebar.switchTo` and `HotkeyBindings.switchWorkspace` (Ctrl+1..9)
// used to be two separate code paths that diverged — the sidebar forgot
// `setTasks`, which meant tasks from the previous workspace briefly leaked
// into `OrchestratorPanel.workspaceTasks` after a switch. Centralising the
// reload here keeps both callers in lock-step.

import { useCallback } from "react";
import { useStore } from "./store";
import { ipc } from "./ipc";

export function useSwitchWorkspace() {
  const clearWorkspaceState = useStore((s) => s.clearWorkspaceState);
  const setCurrent = useStore((s) => s.setCurrent);
  const setLayout = useStore((s) => s.setLayout);
  const setAgents = useStore((s) => s.setAgents);
  const setTasks = useStore((s) => s.setTasks);
  const pushToast = useStore((s) => s.pushToast);

  return useCallback(
    async (id: string | null): Promise<void> => {
      if (!id) return;
      try {
        clearWorkspaceState();
        await ipc.setCurrentWorkspace(id);
        setCurrent(id);
        const ws = await ipc.getWorkspace(id);
        setLayout(ws.layout);
        const agents = await ipc.listAgents(id);
        setAgents(agents);
        // Always reload tasks alongside agents — keeps OrchestratorPanel's
        // TasksCard from showing stale rows from the previous workspace.
        try {
          const tasks = await ipc.listTasks({ workspace_id: id });
          setTasks(tasks);
        } catch {
          // If tasks fail, just clear the local cache so we don't leak.
          setTasks([]);
        }
      } catch (err) {
        pushToast({ text: `Switch failed: ${err}`, kind: "error" });
      }
    },
    [clearWorkspaceState, setCurrent, setLayout, setAgents, setTasks, pushToast],
  );
}
