import { useMemo } from "react";
import { useHotkeys, type HotkeyMap } from "../hooks/useHotkeys";
import { useStore } from "../state/store";
import { ipc } from "../state/ipc";
import { closeLeaf, splitLeaf } from "../layout/tree";
import type { SplitDir } from "../state/types";
import { useSwitchWorkspace } from "../state/useSwitchWorkspace";

export function HotkeyBindings(): null {
  // B-3.3: switch uses the shared hook so it can't drift from the sidebar
  // path. (Inline `useStore.getState().setTasks(...)` here used to be
  // missing from the sidebar — see FRONTEND_BUGS.md 3.3.)
  const switchWorkspace = useSwitchWorkspace();
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

    const switchWorkspaceByIdx = (idx: number) => () => {
      const state = useStore.getState();
      const ws = state.workspaces[idx];
      if (!ws) return;
      if (ws.id === state.currentId) return;
      void switchWorkspace(ws.id);
    };

    const toggleKanban = () => {
      const state = useStore.getState();
      state.setShowTaskBoard(!state.showTaskBoard);
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
      bindings[`ctrl+${i}`] = switchWorkspaceByIdx(i - 1);
    }
    return bindings;
  }, [switchWorkspace]);

  useHotkeys(map);
  return null;
}
