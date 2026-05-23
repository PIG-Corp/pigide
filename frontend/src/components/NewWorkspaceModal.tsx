import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { ipc } from "../state/ipc";
import type { DirEntry } from "../state/types";
import { useStore } from "../state/store";
import { ChevronRight, Folder, X } from "./icons";

const RECENT_KEY = "workspace.recent_folders";
const RECENT_CAP = 10;
const AGENT_PRESETS = [1, 2, 4, 6, 8, 10, 12] as const;
type AgentCount = (typeof AGENT_PRESETS)[number];

interface RecentEntry {
  path: string;
  name: string;
  count: AgentCount;
  used_at: string;
}

function isAgentCount(n: number): n is AgentCount {
  return (AGENT_PRESETS as readonly number[]).includes(n);
}

function safeParseRecent(raw: string | null): RecentEntry[] {
  if (!raw || !raw.trim()) return [];
  try {
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    const out: RecentEntry[] = [];
    for (const x of parsed) {
      if (
        x &&
        typeof x === "object" &&
        typeof x.path === "string" &&
        x.path.trim()
      ) {
        const n =
          typeof x.count === "number" && isAgentCount(x.count) ? x.count : 1;
        out.push({
          path: x.path,
          name: typeof x.name === "string" ? x.name : x.path.split("/").pop() ?? x.path,
          count: n,
          used_at: typeof x.used_at === "string" ? x.used_at : "",
        });
      }
    }
    const seen = new Set<string>();
    return out
      .filter((e) => (seen.has(e.path) ? false : (seen.add(e.path), true)))
      .slice(0, RECENT_CAP);
  } catch {
    return [];
  }
}

async function loadRecents(): Promise<RecentEntry[]> {
  try {
    const raw = await ipc.getSetting(RECENT_KEY);
    return safeParseRecent(raw);
  } catch {
    return [];
  }
}

async function pushRecent(entry: RecentEntry): Promise<void> {
  const list = await loadRecents();
  const filtered = list.filter((e) => e.path !== entry.path);
  filtered.unshift(entry);
  const capped = filtered.slice(0, RECENT_CAP);
  try {
    await ipc.setSetting(RECENT_KEY, JSON.stringify(capped));
  } catch {
    // best-effort; swallow — modal still completes the workspace creation
  }
}

interface FolderBrowserProps {
  cwd: string;
  onCwdChange: (next: string) => void;
  onPick: (path: string) => void;
  picked: string | null;
}

function basename(p: string): string {
  if (!p) return "";
  const s = p.replace(/\/+$/, "");
  const i = s.lastIndexOf("/");
  return i < 0 ? s : s.slice(i + 1) || "/";
}

function parentPath(p: string): string {
  if (!p || p === "/") return "/";
  const s = p.replace(/\/+$/, "");
  const i = s.lastIndexOf("/");
  if (i <= 0) return "/";
  return s.slice(0, i);
}

function FolderBrowser({ cwd, onCwdChange, onPick, picked }: FolderBrowserProps) {
  const pushToast = useStore((s) => s.pushToast);
  const [entries, setEntries] = useState<DirEntry[]>([]);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    ipc
      .browseDir(cwd)
      .then((items) => {
        if (cancelled) return;
        setEntries(items.filter((e) => e.is_dir));
      })
      .catch((err) => {
        if (cancelled) return;
        pushToast({ text: `list_dir: ${err}`, kind: "error" });
        setEntries([]);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [cwd, pushToast]);

  return (
    <div className="nws-browser">
      <div className="nws-browser-bar">
        <button
          type="button"
          className="nws-browser-up"
          onClick={() => onCwdChange(parentPath(cwd))}
          disabled={cwd === "/"}
          aria-label="Go up one directory"
        >
          ↑
        </button>
        <input
          className="nws-browser-path"
          value={cwd}
          onChange={(e) => onCwdChange(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              onCwdChange((e.target as HTMLInputElement).value);
            }
          }}
          spellCheck={false}
          aria-label="Current path"
        />
        <button
          type="button"
          className="nws-browser-pick"
          onClick={() => onPick(cwd)}
          aria-label="Select current directory"
        >
          Use this folder
        </button>
      </div>
      <div className="nws-browser-list" role="listbox" aria-label="Subdirectories">
        {loading ? (
          <div className="nws-browser-empty">Loading…</div>
        ) : entries.length === 0 ? (
          <div className="nws-browser-empty">No subdirectories</div>
        ) : (
          entries.map((e) => (
            <button
              key={e.path}
              type="button"
              role="option"
              aria-selected={picked === e.path}
              className={`nws-browser-row ${picked === e.path ? "picked" : ""}`}
              onClick={() => onPick(e.path)}
              onDoubleClick={() => onCwdChange(e.path)}
            >
              <Folder size={12} />
              <span className="nws-browser-row-name">{e.name}</span>
              <ChevronRight
                size={12}
                onClick={(ev: React.MouseEvent) => {
                  ev.stopPropagation();
                  onCwdChange(e.path);
                }}
                aria-label={`Open ${e.name}`}
              />
            </button>
          ))
        )}
      </div>
    </div>
  );
}

interface AgentLayoutPickerProps {
  value: AgentCount;
  onChange: (n: AgentCount) => void;
}

function gridShape(n: AgentCount): string {
  switch (n) {
    case 1: return "1×1";
    case 2: return "1×2";
    case 4: return "2×2";
    case 6: return "2×3";
    case 8: return "2×4";
    case 10: return "2×5";
    case 12: return "3×4";
  }
}

function AgentLayoutPicker({ value, onChange }: AgentLayoutPickerProps) {
  return (
    <div
      className="nws-presets"
      role="radiogroup"
      aria-label="Agent count and grid layout"
    >
      {AGENT_PRESETS.map((n) => (
        <button
          key={n}
          type="button"
          role="radio"
          aria-checked={value === n}
          className={`nws-preset ${value === n ? "active" : ""}`}
          onClick={() => onChange(n)}
        >
          <span className="nws-preset-num">{n}</span>
          <span className="nws-preset-shape">{gridShape(n)}</span>
        </button>
      ))}
    </div>
  );
}

interface NewWorkspaceModalProps {
  onClose: () => void;
  onCreated: (workspaceId: string) => void;
}

export function NewWorkspaceModal({
  onClose,
  onCreated,
}: NewWorkspaceModalProps) {
  const pushToast = useStore((s) => s.pushToast);
  const workspaces = useStore((s) => s.workspaces);

  const [tab, setTab] = useState<"new" | "recent">("new");
  const [cwd, setCwd] = useState<string>("/");
  const [folder, setFolder] = useState<string | null>(null);
  const [name, setName] = useState<string>("");
  const [count, setCount] = useState<AgentCount>(1);
  const [recents, setRecents] = useState<RecentEntry[]>([]);
  const [submitting, setSubmitting] = useState(false);

  const firstFieldRef = useRef<HTMLDivElement | null>(null);
  const returnFocusRef = useRef<Element | null>(null);

  // Bootstrap: home dir + recents.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const home = await ipc.homeDir();
        if (!cancelled) setCwd(home || "/");
      } catch {
        if (!cancelled) setCwd("/");
      }
      const list = await loadRecents();
      if (!cancelled) setRecents(list);
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // Auto-derive name from picked folder unless user has typed one.
  const userTouchedName = useRef(false);
  useEffect(() => {
    if (!folder) return;
    if (!userTouchedName.current) {
      setName(basename(folder));
    }
  }, [folder]);

  // Esc to close.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  // Capture return-focus target and focus first element on open.
  useEffect(() => {
    returnFocusRef.current = document.activeElement;
    const root = firstFieldRef.current;
    if (!root) return;
    const focusable = root.querySelector<HTMLElement>(
      'button, [role="tab"], input, [tabindex]:not([tabindex="-1"])',
    );
    focusable?.focus();
    return () => {
      (returnFocusRef.current as HTMLElement | null)?.focus();
    };
  }, []);

  const trimmedName = name.trim();
  const nameClash = useMemo(
    () =>
      workspaces.some(
        (w) => w.name.toLowerCase() === trimmedName.toLowerCase(),
      ),
    [workspaces, trimmedName],
  );
  const canSubmit =
    !!folder && !!trimmedName && !nameClash && !submitting;

  const submit = useCallback(async () => {
    if (!folder || !trimmedName || submitting) return;
    setSubmitting(true);
    try {
      const ws = await ipc.createWorkspace(trimmedName, [folder]);
      if (count > 0) {
        try {
          await ipc.spawnAgent(ws.id, "claude", {
            cwd: folder,
            count,
            autoLayout: true,
          });
        } catch (err) {
          pushToast({
            text: `Workspace created, but agent spawn failed: ${err}`,
            kind: "error",
          });
        }
      }
      await pushRecent({
        path: folder,
        name: trimmedName,
        count,
        used_at: new Date().toISOString(),
      });
      onCreated(ws.id);
      onClose();
    } catch (err) {
      pushToast({ text: `Create failed: ${err}`, kind: "error" });
      setSubmitting(false);
    }
  }, [folder, trimmedName, count, submitting, pushToast, onCreated, onClose]);

  const repopulate = (entry: RecentEntry) => {
    setFolder(entry.path);
    setCwd(parentPath(entry.path) || "/");
    userTouchedName.current = true;
    setName(entry.name);
    setCount(entry.count);
    setTab("new");
  };

  return (
    <div
      className="nws-backdrop"
      onClick={onClose}
      role="presentation"
    >
      <div
        ref={firstFieldRef}
        className="nws-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="nws-title"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            submit();
          } else if (e.key === "Tab") {
            const root = firstFieldRef.current;
            if (!root) return;
            const focusable = Array.from(
              root.querySelectorAll<HTMLElement>(
                'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
              )
            ).filter((n) => !n.hasAttribute("disabled"));
            if (focusable.length === 0) return;
            const first = focusable[0];
            const last = focusable[focusable.length - 1];
            if (e.shiftKey) {
              if (document.activeElement === first) {
                e.preventDefault();
                last.focus();
              }
            } else {
              if (document.activeElement === last) {
                e.preventDefault();
                first.focus();
              }
            }
          }
        }}
      >
        <div className="nws-head">
          <span className="nws-title" id="nws-title">
            New workspace
          </span>
          <button
            type="button"
            className="nws-close"
            onClick={onClose}
            aria-label="Close"
          >
            <X size={14} />
          </button>
        </div>

        <div className="nws-tabs" role="tablist">
          <button
            type="button"
            role="tab"
            aria-selected={tab === "new"}
            tabIndex={tab === "new" ? 0 : -1}
            className={`nws-tab ${tab === "new" ? "active" : ""}`}
            onClick={() => setTab("new")}
          >
            New
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={tab === "recent"}
            tabIndex={tab === "recent" ? 0 : -1}
            className={`nws-tab ${tab === "recent" ? "active" : ""}`}
            onClick={() => setTab("recent")}
          >
            Recent
            {recents.length > 0 ? (
              <span className="nws-tab-badge">{recents.length}</span>
            ) : null}
          </button>
        </div>

        {tab === "new" ? (
          <div className="nws-body" role="tabpanel" aria-label="New workspace">
            <label className="nws-field">
              <span className="nws-label">
                Working folder <span className="nws-req">*</span>
              </span>
              <FolderBrowser
                cwd={cwd}
                onCwdChange={setCwd}
                onPick={setFolder}
                picked={folder}
              />
              {folder ? (
                <span className="nws-hint nws-hint-ok" title={folder}>
                  Selected: <code>{folder}</code>
                </span>
              ) : (
                <span className="nws-hint">
                  Choose a directory or click “Use this folder”.
                </span>
              )}
            </label>

            <label className="nws-field">
              <span className="nws-label">Name</span>
              <input
                value={name}
                onChange={(e) => {
                  userTouchedName.current = true;
                  setName(e.target.value);
                }}
                placeholder="my-workspace"
                spellCheck={false}
                aria-invalid={!trimmedName || nameClash}
              />
              {nameClash ? (
                <span className="nws-hint nws-hint-err">
                  A workspace with this name already exists.
                </span>
              ) : null}
            </label>

            <div className="nws-field">
              <span className="nws-label">Agent layout</span>
              <AgentLayoutPicker value={count} onChange={setCount} />
              <span className="nws-hint">
                {count} agent{count === 1 ? "" : "s"} arranged as {gridShape(count)}.
              </span>
            </div>
          </div>
        ) : (
          <div
            className="nws-body nws-body-recent"
            role="tabpanel"
            aria-label="Recent folders"
          >
            {recents.length === 0 ? (
              <div className="nws-empty">No recent folders yet.</div>
            ) : (
              <ul className="nws-recents">
                {recents.map((r) => (
                  <li key={r.path}>
                    <button
                      type="button"
                      className="nws-recent-row"
                      onClick={() => repopulate(r)}
                    >
                      <Folder size={14} />
                      <span className="nws-recent-main">
                        <span className="nws-recent-name">{r.name}</span>
                        <span className="nws-recent-path">{r.path}</span>
                      </span>
                      <span className="nws-recent-meta">
                        <span className="nws-recent-count">
                          {r.count} · {gridShape(r.count)}
                        </span>
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}

        <div className="nws-foot">
          <button type="button" className="btn--ghost" onClick={onClose}>
            Cancel
          </button>
          <button
            type="button"
            className="btn--primary"
            disabled={!canSubmit}
            onClick={submit}
          >
            {submitting ? "Creating…" : "Create workspace"}
          </button>
        </div>
      </div>
    </div>
  );
}
