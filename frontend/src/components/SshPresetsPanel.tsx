import { useEffect, useState } from "react";
import { useStore } from "../state/store";
import { ipc } from "../state/ipc";
import type { SshPreset } from "../state/types";
import { Plus, Trash2 } from "./icons";

/**
 * SshPresetsPanel — manage SSH connection profiles (BridgeSpace gap #14).
 *
 * Each preset launches a PTY agent of type `ssh` with argv built from
 * (host, user, port, identity, args). The host shell's `ssh` does the
 * crypto — no extra Rust crate.
 */
export function SshPresetsPanel() {
  const currentId = useStore((s) => s.currentId);
  const upsertAgent = useStore((s) => s.upsertAgent);
  const pushToast = useStore((s) => s.pushToast);

  const [presets, setPresets] = useState<SshPreset[]>([]);
  const [creating, setCreating] = useState(false);
  const [draft, setDraft] = useState({
    name: "",
    host: "",
    user: "",
    port: "",
    identity: "",
    args: "",
    cwd: "",
  });

  const reload = async () => {
    try {
      const list = await ipc.listSshPresets();
      setPresets(list);
    } catch (err) {
      pushToast({ text: `list_ssh_presets: ${err}`, kind: "error" });
    }
  };

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const startCreate = () => {
    setDraft({ name: "", host: "", user: "", port: "", identity: "", args: "", cwd: "" });
    setCreating(true);
  };

  const save = async () => {
    if (!draft.name.trim() || !draft.host.trim()) {
      pushToast({ text: "Name and host required", kind: "error" });
      return;
    }
    const port = draft.port.trim() ? Number(draft.port) : null;
    if (port !== null && (!Number.isInteger(port) || port < 1 || port > 65535)) {
      pushToast({ text: "Port must be 1-65535", kind: "error" });
      return;
    }
    const args = draft.args
      .split(/\s+/)
      .map((s) => s.trim())
      .filter(Boolean);
    try {
      await ipc.createSshPreset({
        name: draft.name.trim(),
        host: draft.host.trim(),
        user: draft.user.trim() || null,
        port,
        identity: draft.identity.trim() || null,
        args,
        cwd: draft.cwd.trim() || null,
      });
      setCreating(false);
      await reload();
    } catch (err) {
      pushToast({ text: `create_ssh_preset: ${err}`, kind: "error" });
    }
  };

  const remove = async (id: string) => {
    try {
      await ipc.deleteSshPreset(id);
      await reload();
    } catch (err) {
      pushToast({ text: `delete_ssh_preset: ${err}`, kind: "error" });
    }
  };

  const connect = async (p: SshPreset) => {
    if (!currentId) {
      pushToast({ text: "Select a workspace first", kind: "error" });
      return;
    }
    try {
      const agent = await ipc.spawnSsh(currentId, p.id);
      upsertAgent(agent);
      pushToast({ text: `Connecting to ${p.name}…`, kind: "info" });
    } catch (err) {
      pushToast({ text: `spawn_ssh: ${err}`, kind: "error" });
    }
  };

  return (
    <div className="ssh-panel">
      <div className="ssh-header">
        <span>SSH presets</span>
        <button onClick={startCreate} title="New preset">
          <Plus size={12} />
        </button>
      </div>

      {creating && (
        <div className="ssh-editor">
          <input
            placeholder="Name"
            value={draft.name}
            onChange={(e) => setDraft({ ...draft, name: e.target.value })}
          />
          <input
            placeholder="Host"
            value={draft.host}
            onChange={(e) => setDraft({ ...draft, host: e.target.value })}
          />
          <div className="ssh-editor-row">
            <input
              placeholder="User"
              value={draft.user}
              onChange={(e) => setDraft({ ...draft, user: e.target.value })}
            />
            <input
              placeholder="Port"
              value={draft.port}
              onChange={(e) => setDraft({ ...draft, port: e.target.value })}
              style={{ width: 80 }}
            />
          </div>
          <input
            placeholder="Identity file (~/.ssh/...)"
            value={draft.identity}
            onChange={(e) => setDraft({ ...draft, identity: e.target.value })}
          />
          <input
            placeholder="Extra ssh args (e.g. -L 8080:localhost:80)"
            value={draft.args}
            onChange={(e) => setDraft({ ...draft, args: e.target.value })}
          />
          <input
            placeholder="cwd (optional)"
            value={draft.cwd}
            onChange={(e) => setDraft({ ...draft, cwd: e.target.value })}
          />
          <div className="ssh-editor-actions">
            <button onClick={save}>Save</button>
            <button onClick={() => setCreating(false)}>Cancel</button>
          </div>
        </div>
      )}

      <div className="ssh-list">
        {presets.length === 0 ? (
          <div className="ssh-empty">No SSH presets yet</div>
        ) : (
          presets.map((p) => (
            <div key={p.id} className="ssh-item">
              <div className="ssh-item-head">
                <span className="ssh-item-name">{p.name}</span>
                <span className="ssh-item-target">
                  {p.user ? `${p.user}@${p.host}` : p.host}
                  {p.port ? `:${p.port}` : ""}
                </span>
              </div>
              {p.args.length > 0 && (
                <div className="ssh-item-args">{p.args.join(" ")}</div>
              )}
              <div className="ssh-item-actions">
                <button onClick={() => connect(p)}>Connect</button>
                <button onClick={() => remove(p.id)} title="Delete">
                  <Trash2 size={11} />
                </button>
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
