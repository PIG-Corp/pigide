import { useEffect, useMemo, useState } from "react";
import { useStore } from "../state/store";
import { ipc } from "../state/ipc";
import type { DirEntry } from "../state/types";
import { Folder, X } from "./icons";
import { CodeEditor } from "./CodeEditor";

interface OpenTab {
  path: string;
  content: string;
  dirty: boolean;
}

/**
 * FilesPanel — file browser + multi-tab CodeMirror editor for the active
 * workspace's first path. Cmd+P opens Quick Open across the project.
 */
export function FilesPanel({ onClose }: { onClose?: () => void }) {
  const currentId = useStore((s) => s.currentId);
  const workspaces = useStore((s) => s.workspaces);
  const pushToast = useStore((s) => s.pushToast);

  const [root, setRoot] = useState<string | null>(null);
  const [stack, setStack] = useState<string[]>([]); // breadcrumb
  const [entries, setEntries] = useState<DirEntry[]>([]);
  const [tabs, setTabs] = useState<OpenTab[]>([]);
  const [activeTab, setActiveTab] = useState<string | null>(null);
  const [quickOpen, setQuickOpen] = useState(false);
  const [allFiles, setAllFiles] = useState<DirEntry[]>([]);
  const [quickQuery, setQuickQuery] = useState("");

  const ws = workspaces.find((w) => w.id === currentId);
  const wsRoot = ws?.paths?.[0] ?? null;

  // Resolve initial root.
  useEffect(() => {
    if (!wsRoot) {
      /* eslint-disable react-hooks/set-state-in-effect */
      setRoot(null);
      setEntries([]);
      setTabs([]);
      setActiveTab(null);
      /* eslint-enable react-hooks/set-state-in-effect */
      return;
    }
    setRoot(wsRoot);
    setStack([wsRoot]);
  }, [wsRoot]);

  // Load directory listing whenever the path stack changes.
  useEffect(() => {
    const top = stack[stack.length - 1];
    if (!top) return;
    ipc
      .listDir(top)
      .then(setEntries)
      .catch((err) => pushToast({ text: `list_dir: ${err}`, kind: "error" }));
  }, [stack, pushToast]);

  // Quick-open: walk on demand. Re-walk every time so freshly created files
  // show up without forcing the user to reopen the panel.
  useEffect(() => {
    if (!quickOpen || !root) return;
    ipc
      .walkFiles(root, 2000)
      .then(setAllFiles)
      .catch((err) =>
        pushToast({ text: `walk_files: ${err}`, kind: "error" }),
      );
  }, [quickOpen, root, pushToast]);

  // Cmd+P opens Quick Open. Cmd+S is handled by the editor's keymap, so we
  // don't intercept it here.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      const inField =
        target?.tagName === "INPUT" || target?.tagName === "TEXTAREA";
      // xterm renders into a <canvas>/<textarea> under .xterm; don't steal
      // its keys (Ctrl+P = previous shell command).
      const inTerminal = !!target?.closest?.(".xterm");
      if (
        (e.ctrlKey || e.metaKey) &&
        e.key.toLowerCase() === "p" &&
        !inField &&
        !inTerminal
      ) {
        e.preventDefault();
        setQuickOpen(true);
      }
      if (e.key === "Escape" && quickOpen) {
        setQuickOpen(false);
      }
    };
    document.body.addEventListener("keydown", onKey);
    return () => document.body.removeEventListener("keydown", onKey);
  }, [quickOpen]);

  const openFile = async (path: string) => {
    // If already open, just focus its tab.
    if (tabs.some((t) => t.path === path)) {
      setActiveTab(path);
      return;
    }
    try {
      const c = await ipc.readFile(path);
      setTabs((prev) => [...prev, { path, content: c, dirty: false }]);
      setActiveTab(path);
    } catch (err) {
      pushToast({ text: `read_file: ${err}`, kind: "error" });
    }
  };

  const updateTabContent = (path: string, next: string) => {
    setTabs((prev) =>
      prev.map((t) =>
        t.path === path ? { ...t, content: next, dirty: t.content !== next || t.dirty } : t,
      ),
    );
  };

  const saveTab = async (path: string) => {
    const tab = tabs.find((t) => t.path === path);
    if (!tab) return;
    try {
      await ipc.writeFile(path, tab.content);
      setTabs((prev) =>
        prev.map((t) => (t.path === path ? { ...t, dirty: false } : t)),
      );
    } catch (err) {
      pushToast({ text: `write_file: ${err}`, kind: "error" });
    }
  };

  const closeTab = (path: string) => {
    const tab = tabs.find((t) => t.path === path);
    if (tab?.dirty && !confirm(`Discard unsaved changes in ${path}?`)) return;
    setTabs((prev) => {
      const next = prev.filter((t) => t.path !== path);
      if (activeTab === path) {
        const fallback = next[next.length - 1] ?? null;
        setActiveTab(fallback ? fallback.path : null);
      }
      return next;
    });
  };

  const filtered = useMemo(() => {
    const q = quickQuery.toLowerCase().trim();
    if (!q) return allFiles.slice(0, 200);
    const scored = allFiles
      .map((f) => {
        const idx = f.path.toLowerCase().indexOf(q);
        return idx >= 0 ? { f, idx } : null;
      })
      .filter((x): x is { f: DirEntry; idx: number } => x !== null);
    scored.sort((a, b) => a.idx - b.idx || a.f.path.length - b.f.path.length);
    return scored.slice(0, 200).map((x) => x.f);
  }, [quickQuery, allFiles]);

  if (!wsRoot) {
    return (
      <div className="files-panel">
        <div className="empty-state">
          Workspace has no `paths[]` configured. Edit the workspace and add
          a project root.
        </div>
      </div>
    );
  }

  const active = activeTab ? tabs.find((t) => t.path === activeTab) : null;

  return (
    <div className="files-panel">
      <div className="files-header">
        <Folder size={12} />
        <div className="files-breadcrumb">
          {stack.map((p, i) => (
            <button
              key={`${p}-${i}`}
              onClick={() => setStack(stack.slice(0, i + 1))}
              title={p}
            >
              {i === 0 ? p.split("/").slice(-2, -1)[0] || p : p.split("/").pop()}
            </button>
          ))}
        </div>
        <span className="spacer" />
        <button onClick={() => setQuickOpen(true)} title="Quick open (Ctrl+P)">
          ⌘P
        </button>
        {onClose ? (
          <button className="btn--icon" onClick={onClose} title="Close">
            <X size={12} />
          </button>
        ) : null}
      </div>

      <div className="files-body">
        <div className="files-list">
          {entries.length === 0 ? (
            <div className="empty-state">empty</div>
          ) : null}
          {entries.map((e) => (
            <div
              key={e.path}
              className={`files-item ${
                activeTab === e.path ? "active" : ""
              } ${e.is_dir ? "dir" : "file"}`}
              onClick={() =>
                e.is_dir ? setStack([...stack, e.path]) : openFile(e.path)
              }
            >
              <span className="files-item-name">
                {e.is_dir ? "▸" : " "} {e.name}
              </span>
            </div>
          ))}
        </div>

        <div className="files-editor">
          {tabs.length > 0 && (
            <div className="files-tabbar">
              {tabs.map((t) => {
                const name = t.path.split("/").pop() ?? t.path;
                return (
                  <div
                    key={t.path}
                    className={`files-tab ${activeTab === t.path ? "active" : ""}`}
                    onClick={() => setActiveTab(t.path)}
                    title={t.path}
                  >
                    <span className="files-tab-name">
                      {t.dirty ? "•" : ""} {name}
                    </span>
                    <button
                      className="files-tab-close"
                      onClick={(e) => {
                        e.stopPropagation();
                        closeTab(t.path);
                      }}
                      title="Close tab"
                    >
                      ×
                    </button>
                  </div>
                );
              })}
            </div>
          )}

          {active ? (
            <>
              <div className="files-editor-header">
                <span className="files-editor-path" title={active.path}>
                  {active.path}
                  {active.dirty ? " •" : ""}
                </span>
                <button
                  onClick={() => saveTab(active.path)}
                  disabled={!active.dirty}
                >
                  Save (Ctrl+S)
                </button>
              </div>
              <CodeEditor
                path={active.path}
                initial={active.content}
                onChange={(next) => updateTabContent(active.path, next)}
                onSave={() => saveTab(active.path)}
              />
            </>
          ) : (
            <div className="empty-state empty-state--centered">
              Pick a file to edit. Ctrl+P for Quick Open.
            </div>
          )}
        </div>
      </div>

      {quickOpen ? (
        <div
          className="quick-open-backdrop"
          onClick={() => setQuickOpen(false)}
        >
          <div
            className="quick-open"
            onClick={(e) => e.stopPropagation()}
          >
            <input
              autoFocus
              value={quickQuery}
              onChange={(e) => setQuickQuery(e.target.value)}
              placeholder="Search files…"
              onKeyDown={(e) => {
                if (e.key === "Escape") setQuickOpen(false);
                if (e.key === "Enter" && filtered.length > 0) {
                  openFile(filtered[0].path);
                  setQuickOpen(false);
                }
              }}
            />
            <div className="quick-open-list">
              {filtered.map((f) => (
                <button
                  key={f.path}
                  className="quick-open-item"
                  onClick={() => {
                    openFile(f.path);
                    setQuickOpen(false);
                  }}
                  title={f.path}
                >
                  <span className="quick-open-name">{f.name}</span>
                  <span className="quick-open-path">{f.path}</span>
                </button>
              ))}
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
