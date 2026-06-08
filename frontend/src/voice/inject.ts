import { ipc } from "../state/ipc";
import { isAgentInLayout } from "../layout/tree";
import { useStore } from "../state/store";

/// UTF-8 string -> base64 (matches AgentTile.toB64 on the receiving side).
function toB64(s: string): string {
  const bytes = new TextEncoder().encode(s);
  let bin = "";
  for (let i = 0; i < bytes.length; i += 1) bin += String.fromCharCode(bytes[i]);
  return btoa(bin);
}

export type VoiceInjectOutcome =
  | { kind: "tile"; agentId: string }
  | { kind: "orchestrator" }
  | { kind: "orchestrator-fallback"; reason: "no-focused-tile" }
  | { kind: "noop" };

/**
 * Route a freshly-transcribed voice message according to the user's
 * `voiceTarget` preference. Resolves the focused agent from the live
 * `layout` tree and the `agents` registry so a stale `focusedLeafId`
 * (killed / respawned tile) safely falls back to the orchestrator chat.
 *
 * Behaviour:
 * - `focused-tile` + live focused agent → write to that agent's PTY
 *   (text + Enter, single line so the agent's readline sees one prompt).
 * - `focused-tile` + no live focused agent → fallback to orchestrator
 *   draft input and surface a toast.
 * - `orchestrator` → append to the draft input (legacy).
 */
export async function injectVoiceTranscript(
  text: string,
  opts: { newlines?: number } = {},
): Promise<VoiceInjectOutcome> {
  const trimmed = text.trimEnd();
  if (!trimmed) return { kind: "noop" };

  const state = useStore.getState();
  const target = state.voiceTarget;
  const trailingNewlines = Math.max(0, Math.min(5, opts.newlines ?? 1));
  const payload = trimmed + "\n".repeat(trailingNewlines);

  if (target === "orchestrator") {
    state.appendDraftInput(trimmed);
    return { kind: "orchestrator" };
  }

  // focused-tile path
  const focused = state.focusedLeafId;
  if (focused && isAgentInLayout(state.layout, focused) && state.agents[focused]) {
    try {
      await ipc.writeToAgent(focused, toB64(payload));
      return { kind: "tile", agentId: focused };
    } catch (err) {
      state.pushToast({
        text: `Voice → tile failed: ${err}; falling back to orchestrator`,
        kind: "error",
      });
      state.appendDraftInput(trimmed);
      return { kind: "orchestrator-fallback", reason: "no-focused-tile" };
    }
  }

  state.pushToast({
    text: "No focused tile — voice went to orchestrator",
    kind: "info",
  });
  state.appendDraftInput(trimmed);
  return { kind: "orchestrator-fallback", reason: "no-focused-tile" };
}
