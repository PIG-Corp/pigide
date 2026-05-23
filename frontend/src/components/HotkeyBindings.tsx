import { useMemo } from "react";
import { useHotkeys, type HotkeyMap } from "../hooks/useHotkeys";
import { useStore } from "../state/store";
import { ipc } from "../state/ipc";
import { closeLeaf, splitLeaf } from "../layout/tree";
import type { SplitDir } from "../state/types";

export function HotkeyBindings(): null {
  const map = useMemo<HotkeyMap>(() => {
    const newWorkspace = () => {
      useStore.getState().setNewWorkspaceModalOpen(true);
    };

    const closeFocusedTile = async () => {
      const state = useStore.getState();
      const focused = state.focusedLeafId;
      const currentId = state.currentId;
      if (!focused) return;
      try {
        await ipc.killAgent(focused);
      } catch (err) {
        state.pushToast({ text: `Failed to close: ${err}`, kind: "error" });
        return;
      }
      // Local layout update — backend agent://exit handler removes the
      // agent record but doesn't touch the tree.
      const next = closeLeaf(useStore.getState().layout, focused);
      useStore.getState().setLayout(next);
      useStore.getState().removeAgent(focused);
      if (currentId) {
        ipc.updateLayout(currentId, next).catch(() => undefined);
      }
    };

    const splitFocused = async (dir: SplitDir) => {
      const state = useStore.getState();
      const focused = state.focusedLeafId;
      const currentId = state.currentId;
      if (!focused || !currentId) return;
      const focusedAgent = state.agents[focused];
      if (!focusedAgent) return;
      try {
        const [a] = await ipc.spawnAgent(
          currentId,
          focusedAgent.agent_type as "kiro-cli" | "claude",
          { autoLayout: false },
        );
        useStore.getState().upsertAgent(a);
        const next = splitLeaf(useStore.getState().layout, focused, dir, a.id);
        useStore.getState().setLayout(next);
        await ipc.updateLayout(currentId, next);
      } catch (err) {
        useStore.getState().pushToast({
          text: `Spawn failed: ${err}`,
          kind: "error",
        });
      }
    };

    const switchWorkspace = (idx: number) => async () => {
      const state = useStore.getState();
      const ws = state.workspaces[idx];
      if (!ws) return;
      if (ws.id === state.currentId) return;
      try {
        state.clearWorkspaceState();
        await ipc.setCurrentWorkspace(ws.id);
        // Backend doesn't emit workspace://changed for set_current; pull
        // the new state ourselves.
        state.setCurrent(ws.id);
        const fresh = await ipc.getWorkspace(ws.id);
        state.setLayout(fresh.layout);
        const agents = await ipc.listAgents(ws.id);
        state.setAgents(agents);
        const tasks = await ipc.listTasks({ workspace_id: ws.id });
        state.setTasks(tasks);
      } catch (err) {
        state.pushToast({
          text: `Failed to switch workspace: ${err}`,
          kind: "error",
        });
      }
    };

    const toggleKanban = () => {
      const state = useStore.getState();
      state.setShowKanban(!state.showKanban);
    };

    const restoreFromMaximize = () => {
      const state = useStore.getState();
      if (state.maximizedLeafId !== null) {
        state.setMaximized(null);
      }
    };

    const bindings: HotkeyMap = {
      // ctrl+t (new browser tab) and ctrl+w (close tab) are intentionally
      // omitted — they hijack standard browser/OS shortcuts (U-92 / H-35).
      "ctrl+shift+n": newWorkspace,
      "ctrl+shift+w": () => { void closeFocusedTile(); },
      "ctrl+shift+d": () => splitFocused("h"),
      "ctrl+shift+s": () => splitFocused("v"),
      "ctrl+k": toggleKanban,
      escape: restoreFromMaximize,
    };
    for (let i = 1; i <= 9; i += 1) {
      bindings[`ctrl+${i}`] = switchWorkspace(i - 1);
    }
    return bindings;
  }, []);

  useHotkeys(map);
  return null;
}
