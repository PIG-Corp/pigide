import { useEffect, useState } from "react";
import { useStore } from "../state/store";
import { ipc } from "../state/ipc";
import type { RolePromptOverride, SwarmRole } from "../state/types";
import { Plus, Trash2 } from "./icons";

const ROLES: SwarmRole[] = ["coordinator", "builder", "reviewer", "scout"];
const AGENT_TYPES: { value: string; label: string }[] = [
  { value: "", label: "Any agent" },
  { value: "claude", label: "Claude" },
  { value: "kiro-cli", label: "Kiro CLI" },
  { value: "opencode", label: "OpenCode" },
  { value: "devin", label: "Devin" },
  { value: "agy", label: "Antigravity CLI" },
  { value: "codex", label: "OpenAI Codex" },
];

/**
 * AgentConfigPanel — per-workspace, per-role (and optionally per-agent-type)
 * system-prompt overrides (BridgeSpace gap #19).
 *
 * Persists via the `role_prompts` table; the orchestrator and PigSwarm look
 * up overrides via `swarm::prompts::resolve` before falling back to
 * `Role::default_prompt()`.
 */
export function AgentConfigPanel() {
  const currentId = useStore((s) => s.currentId);
  const pushToast = useStore((s) => s.pushToast);

  const [items, setItems] = useState<RolePromptOverride[]>([]);
  const [draftRole, setDraftRole] = useState<SwarmRole>("builder");
  const [draftType, setDraftType] = useState("");
  const [draftPrompt, setDraftPrompt] = useState("");
  const [defaultPreview, setDefaultPreview] = useState("");

  const reload = async () => {
    if (!currentId) {
      setItems([]);
      return;
    }
    try {
      const list = await ipc.listRolePrompts(currentId);
      setItems(list);
    } catch (err) {
      pushToast({ text: `list_role_prompts: ${err}`, kind: "error" });
    }
  };

  useEffect(() => {
    // B-11.2: drop draft state on workspace switch — otherwise editing
    // an override in workspace A, switching to B, would leave the
    // draft from A bound to (role, type) that may not exist in B.
    setDraftRole("builder");
    setDraftType("");
    setDraftPrompt("");
    setDefaultPreview("");
    // eslint-disable-next-line react-hooks/set-state-in-effect
    reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentId]);

  // Show the resolved (effective) prompt for the chosen role+type so the user
  // sees what an override would replace.
  useEffect(() => {
    if (!currentId) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setDefaultPreview("");
      return;
    }
    let cancelled = false;
    ipc
      .resolveRolePrompt({
        workspace_id: currentId,
        agent_type: draftType,
        role: draftRole,
      })
      .then((s) => {
        if (!cancelled) setDefaultPreview(s);
      })
      .catch(() => {
        if (!cancelled) setDefaultPreview("");
      });
    return () => {
      cancelled = true;
    };
  }, [currentId, draftRole, draftType]);

  const save = async () => {
    if (!currentId) return;
    if (!draftPrompt.trim()) {
      pushToast({ text: "Prompt body required", kind: "error" });
      return;
    }
    try {
      await ipc.upsertRolePrompt({
        workspace_id: currentId,
        agent_type: draftType,
        role: draftRole,
        prompt: draftPrompt,
      });
      setDraftPrompt("");
      await reload();
    } catch (err) {
      pushToast({ text: `upsert_role_prompt: ${err}`, kind: "error" });
    }
  };

  const remove = async (o: RolePromptOverride) => {
    if (!currentId) return;
    try {
      await ipc.deleteRolePrompt({
        workspace_id: currentId,
        agent_type: o.agent_type,
        role: o.role,
      });
      await reload();
    } catch (err) {
      pushToast({ text: `delete_role_prompt: ${err}`, kind: "error" });
    }
  };

  const startEdit = (o: RolePromptOverride) => {
    setDraftRole(o.role);
    setDraftType(o.agent_type);
    setDraftPrompt(o.prompt);
  };

  if (!currentId) {
    return (
      <div className="agent-config-panel">
        <div className="agent-config-empty">Open a workspace first.</div>
      </div>
    );
  }

  return (
    <div className="agent-config-panel">
      <div className="agent-config-editor">
        <div className="agent-config-row">
          <label>
            Role
            <select
              value={draftRole}
              onChange={(e) => setDraftRole(e.target.value as SwarmRole)}
            >
              {ROLES.map((r) => (
                <option key={r} value={r}>
                  {r}
                </option>
              ))}
            </select>
          </label>
          <label>
            Agent type
            <select
              value={draftType}
              onChange={(e) => setDraftType(e.target.value)}
            >
              {AGENT_TYPES.map((t) => (
                <option key={t.value} value={t.value}>
                  {t.label}
                </option>
              ))}
            </select>
          </label>
        </div>
        <div className="agent-config-preview-label">Effective prompt now:</div>
        <pre className="agent-config-preview">{defaultPreview}</pre>
        <textarea
          className="agent-config-textarea"
          placeholder="Override system prompt for this role/type combo…"
          value={draftPrompt}
          onChange={(e) => setDraftPrompt(e.target.value)}
        />
        <div className="agent-config-actions">
          <button onClick={save}>
            <Plus size={12} /> Save override
          </button>
        </div>
      </div>

      <div className="agent-config-list">
        <div className="agent-config-list-title">Saved overrides</div>
        {items.length === 0 ? (
          <div className="agent-config-empty">
            No overrides yet — defaults apply.
          </div>
        ) : (
          items.map((o) => (
            <div
              key={`${o.agent_type}:${o.role}`}
              className="agent-config-item"
            >
              <div className="agent-config-item-head">
                <span className="agent-config-role">{o.role}</span>
                <span className="agent-config-type">
                  {o.agent_type ? o.agent_type : "any"}
                </span>
                <span className="agent-config-spacer" />
                <button onClick={() => startEdit(o)}>Edit</button>
                <button onClick={() => remove(o)} title="Delete">
                  <Trash2 size={12} />
                </button>
              </div>
              <pre className="agent-config-item-body">{o.prompt}</pre>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
