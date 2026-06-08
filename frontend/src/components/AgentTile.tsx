import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { ImageAddon } from "@xterm/addon-image";
import "@xterm/xterm/css/xterm.css";
import { ipc, onAgentStdout, onAgentExit } from "../state/ipc";
import { useStore } from "../state/store";
import {
  signalColor,
  signalLabel,
  useArchitectStore,
} from "../state/architect";
import { closeLeaf, splitLeaf, replaceLeafId } from "../layout/tree";
import type { Agent } from "../state/types";
import { Maximize2, Minimize2, X, RowsIcon, ColumnsIcon } from "./icons";
import { useTheme } from "../themes/useTheme";
import { CommandBlockParser, type CommandBlock } from "./cmdblock/parser";
import { CommandBlocksBar } from "./cmdblock/CommandBlocksBar";
import { useAgentSummary } from "../hooks/useAgentSummary";
import type { Task, TaskStatus } from "../state/types";

interface Props {
  agent: Agent;
  isFocused: boolean;
  isMaximized: boolean;
}

// Tauri webview augments File with an absolute `path` property; fall back to name.
type TauriFile = File & { path?: string };

function quoteIfNeeded(p: string): string {
  if (/[\s'"\\$`!*?]/.test(p)) {
    // POSIX-style: wrap in single quotes, escape internal single quotes.
    return `'${p.replace(/'/g, `'\\''`)}'`;
  }
  return p;
}

/**
 * Merge a freshly-emitted batch of CommandBlock updates into the existing
 * list. The parser produces one entry per `C` (command-start) and another
 * for the same id on `D` (exit) — we coalesce by id so the timeline shows a
 * single block per command.
 */
function mergeBlocks(prev: CommandBlock[], next: CommandBlock[]): CommandBlock[] {
  if (next.length === 0) return prev;
  const map = new Map(prev.map((b) => [b.id, b]));
  for (const b of next) {
    map.set(b.id, { ...(map.get(b.id) ?? {}), ...b });
  }
  // Keep at most 200 blocks to bound memory.
  const sorted = Array.from(map.values()).sort((a, b) => a.id - b.id);
  if (sorted.length > 200) sorted.splice(0, sorted.length - 200);
  return sorted;
}

// Encode bytes/string -> base64.
function toB64(s: string): string {
  // xterm sends utf-8 strings; encode each char as utf-8 bytes first.
  const bytes = new TextEncoder().encode(s);
  let bin = "";
  for (let i = 0; i < bytes.length; i += 1) bin += String.fromCharCode(bytes[i]);
  return btoa(bin);
}

// B-10.1: strip the control characters that are dangerous to forward to
// a PTY. We keep \t, \n, \r (legitimate paste of multi-line text) and
// printable ASCII + valid UTF-8; everything else is dropped. The backend
// applies the same filter (defence in depth), but filtering at the source
// also stops garbage from spamming the IPC channel.
const CTRL_CHARS = /[\x00-\x08\x0B-\x0C\x0E-\x1F\x7F]/g;
function sanitizeForPty(s: string): string {
  return s.replace(CTRL_CHARS, "");
}

// Decode base64 -> Uint8Array.
function fromB64(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
  return out;
}

function escapeRegex(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

export function AgentTile({ agent, isFocused, isMaximized }: Props) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const bodyRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const searchRef = useRef<SearchAddon | null>(null);
  const searchInputRef = useRef<HTMLInputElement | null>(null);
  const termDisposedRef = useRef(false);
  const setFocused = useStore((s) => s.setFocusedLeaf);
  const setMaximized = useStore((s) => s.setMaximized);
  const layout = useStore((s) => s.layout);
  const currentId = useStore((s) => s.currentId);
  const setLayout = useStore((s) => s.setLayout);
  const removeAgent = useStore((s) => s.removeAgent);
  const upsertAgent = useStore((s) => s.upsertAgent);
  const pushToast = useStore((s) => s.pushToast);
  const isDead = agent.status === "exited";

  const [searchOpen, setSearchOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [menuPos, setMenuPos] = useState<{ x: number; y: number } | null>(null);
  const [dragOver, setDragOver] = useState(false);
  const [blocks, setBlocks] = useState<CommandBlock[]>([]);
  const parserRef = useRef<CommandBlockParser | null>(null);
  const { theme } = useTheme();

  // Initialise xterm exactly once.
  useEffect(() => {
    const node = containerRef.current;
    if (!node) return;
    const term = new Terminal({
      fontFamily: 'ui-monospace, "Fira Code", Menlo, Consolas, monospace',
      fontSize: 13,
      cursorBlink: true,
      theme: theme.xterm,
      allowProposedApi: true,
      convertEol: false,
      scrollback: 5000,
    });
    const fit = new FitAddon();
    const search = new SearchAddon();
    // Image addon (#22): renders inline sixel/iTerm2 image escapes — for
    // agents that emit graphs/charts directly into stdout.
    const image = new ImageAddon();
    term.loadAddon(fit);
    term.loadAddon(search);
    term.loadAddon(image);
    term.open(node);
    termRef.current = term;
    fitRef.current = fit;
    searchRef.current = search;

    // Fit then send size to backend.
    requestAnimationFrame(() => {
      try {
        fit.fit();
        const { cols, rows } = term;
        ipc.resizeAgent(agent.id, cols, rows).catch(() => undefined);
      } catch {
        /* ignore */
      }
    });

    // Send keystrokes back to PTY. xterm.js only emits bytes that
    // correspond to real user keys — every interactive key (arrows,
    // Backspace=0x7F, Ctrl+C=0x03, …) starts with ESC=0x1B or a C0
    // control byte, so we must NOT strip them here. Sanitisation is
    // only safe on paste/drop paths (defence-in-depth) and on the
    // display side. B-10.1 originally conflated the two directions.
    const onDataDisp = term.onData((data) => {
      ipc.writeToAgent(agent.id, toB64(data)).catch((err) => {
        console.error("write_to_agent failed", err);
      });
    });

    // Click to focus.
    const focusHandler = () => setFocused(agent.id);
    node.addEventListener("mousedown", focusHandler);

    // ResizeObserver -> fit -> resize_agent.
    // B-2.4: debounce the resize IPC by 50ms so a window drag doesn't
    // flood the channel with one fit+resize call per frame.
    let resizeTimer: ReturnType<typeof setTimeout> | null = null;
    const ro = new ResizeObserver(() => {
      if (termDisposedRef.current) return;
      if (resizeTimer) clearTimeout(resizeTimer);
      resizeTimer = setTimeout(() => {
        if (termDisposedRef.current) return;
        try {
          fit.fit();
          const { cols, rows } = term;
          ipc.resizeAgent(agent.id, cols, rows).catch(() => undefined);
        } catch {
          /* ignore */
        }
      }, 50);
    });
    ro.observe(node);

    // B-1.3: subscribe to live stdout FIRST so we don't lose chunks
    // emitted during the agentLogTail fetch. The replay below merges into
    // the same parser instance, so the boundary between "replay" and
    // "live" is single-pass and never drops data.
    let disposed = false;
    let unsubStdout: (() => void) | null = null;
    let unsubExit: (() => void) | null = null;

    onAgentStdout((e) => {
      if (disposed) return;
      if (e.agent_id !== agent.id) return;
      const bytes = fromB64(e.data_b64);
      const filtered = parserRef.current?.feed(bytes) ?? bytes;
      term.write(filtered);
      const taken = parserRef.current?.take() ?? [];
      if (taken.length) setBlocks((b) => mergeBlocks(b, taken));
    }).then((u) => {
      if (disposed) u();
      else unsubStdout = u;
    });

    onAgentExit((e) => {
      if (disposed) return;
      if (e.agent_id !== agent.id) return;
      term.write("\r\n\x1b[2;33m[process exited]\x1b[0m\r\n");
    }).then((u) => {
      if (disposed) u();
      else unsubExit = u;
    });

    // Replay scrollback from the persistent PTY log so a session restore
    // (or just remounting the tile) brings xterm up with the last 64 KiB of
    // terminal history. Live listener is already active above, so any
    // bytes that arrive during this fetch are not lost.
    let cancelled = false;
    parserRef.current = new CommandBlockParser();
    ipc.agentLogTail(agent.id, 64 * 1024)
      .then((b64) => {
        if (cancelled || disposed || !b64) return;
        // Defer the write one frame so the FitAddon has measured the
        // container (the rAF above is the one that calls fit.fit()).
        // Without this the scrollback paints at the default 80x24 grid
        // and the very next ResizeObserver tick re-flows it, producing
        // the visible "tile re-renders with the wrong columns" glitch
        // on every workspace switch.
        requestAnimationFrame(() => {
          if (cancelled || disposed) return;
          try {
            const parsed = parserRef.current?.feed(fromB64(b64));
            term.write(parsed ?? fromB64(b64));
            const taken = parserRef.current?.take() ?? [];
            if (taken.length) setBlocks((b) => mergeBlocks(b, taken));
          } catch {
            /* ignore */
          }
        });
      })
      .catch(() => undefined);

    return () => {
      disposed = true;
      cancelled = true;
      termDisposedRef.current = true;
      onDataDisp.dispose();
      ro.disconnect();
      if (resizeTimer) clearTimeout(resizeTimer);
      node.removeEventListener("mousedown", focusHandler);
      if (unsubStdout) unsubStdout();
      if (unsubExit) unsubExit();
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
      searchRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agent.id]);

  // Refit when maximize toggles (parent resized).
  useEffect(() => {
    requestAnimationFrame(() => {
      try {
        fitRef.current?.fit();
        if (termRef.current) {
          const { cols, rows } = termRef.current;
          ipc.resizeAgent(agent.id, cols, rows).catch(() => undefined);
        }
      } catch {
        /* ignore */
      }
    });
  }, [isMaximized, agent.id]);

  // Apply theme.xterm to the live terminal whenever the theme changes.
  useEffect(() => {
    if (termRef.current) {
      termRef.current.options.theme = theme.xterm;
    }
  }, [theme]);

  // Ctrl+F opens search; Ctrl+C / Ctrl+Shift+C copy the terminal selection
  // (same as the right-click "Copy" menu item); Escape closes it all.
  useEffect(() => {
    const node = bodyRef.current;
    if (!node) return;
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && (e.key === "f" || e.key === "F")) {
        e.preventDefault();
        e.stopPropagation();
        setSearchOpen(true);
        requestAnimationFrame(() => searchInputRef.current?.focus());
        return;
      }
      // Copy selection: Ctrl+C (or Cmd+C on macOS). Ctrl+Shift+C is also
      // bound so users with the "select-with-mouse, then chord" habit get
      // a match. Only acts when there is a non-empty xterm selection —
      // otherwise we let the event propagate so the browser/OS still get
      // their native copy behaviour.
      if (
        (e.ctrlKey || e.metaKey) &&
        !e.altKey &&
        (e.key === "c" || e.key === "C")
      ) {
        const sel = termRef.current?.getSelection() ?? "";
        if (sel) {
          e.preventDefault();
          e.stopPropagation();
          navigator.clipboard.writeText(sel).catch((err) => {
            pushToast({ text: `Copy failed: ${err}`, kind: "error" });
          });
        }
        return;
      }
      if (e.key === "Escape") {
        if (searchOpen) {
          setSearchOpen(false);
          termRef.current?.focus();
        }
        if (menuPos) setMenuPos(null);
      }
    };
    node.addEventListener("keydown", onKey, true);
    return () => node.removeEventListener("keydown", onKey, true);
  }, [searchOpen, menuPos, pushToast]);

  // Close context menu on any outside click.
  useEffect(() => {
    if (!menuPos) return;
    const onDocClick = () => setMenuPos(null);
    document.body.addEventListener("click", onDocClick);
    return () => document.body.removeEventListener("click", onDocClick);
  }, [menuPos]);

  const close = async () => {
    try {
      await ipc.killAgent(agent.id);
    } catch (err) {
      pushToast({ text: `Failed to close: ${err}`, kind: "error" });
    }
    removeAgent(agent.id);
    const next = closeLeaf(layout, agent.id);
    setLayout(next);
    if (currentId) ipc.updateLayout(currentId, next).catch(() => undefined);
  };

  const respawn = async () => {
    if (!currentId) return;
    try {
      const [a] = await ipc.spawnAgent(
        currentId,
        agent.agent_type as "kiro-cli" | "claude" | "opencode" | "devin" | "agy" | "codex",
        { autoLayout: false, cwd: agent.cwd },
      );
      // B-4.3: keep the user focused on the freshly-respawned tile.
      const wasFocused = useStore.getState().focusedLeafId === agent.id;
      removeAgent(agent.id);
      upsertAgent(a);
      const next = replaceLeafId(layout, agent.id, a.id);
      setLayout(next);
      if (wasFocused) setFocused(a.id);
      await ipc.updateLayout(currentId, next);
    } catch (err) {
      pushToast({ text: `Respawn failed: ${err}`, kind: "error" });
    }
  };

  const split = async (dir: "h" | "v") => {
    if (!currentId) return;
    try {
      const [a] = await ipc.spawnAgent(
        currentId,
        agent.agent_type as "kiro-cli" | "claude" | "opencode" | "devin" | "agy" | "codex",
        { autoLayout: false },
      );
      upsertAgent(a);
      const next = splitLeaf(layout, agent.id, dir, a.id);
      setLayout(next);
      await ipc.updateLayout(currentId, next);
    } catch (err) {
      pushToast({ text: `Spawn failed: ${err}`, kind: "error" });
    }
  };

  const findNext = () => {
    if (searchQuery) searchRef.current?.findNext(escapeRegex(searchQuery));
  };
  const findPrev = () => {
    if (searchQuery) searchRef.current?.findPrevious(escapeRegex(searchQuery));
  };

  const onSearchKey = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      if (e.shiftKey) findPrev();
      else findNext();
    } else if (e.key === "Escape") {
      e.preventDefault();
      setSearchOpen(false);
      termRef.current?.focus();
    }
  };

  const ctxCopy = async () => {
    const sel = termRef.current?.getSelection() ?? "";
    if (sel) {
      try {
        await navigator.clipboard.writeText(sel);
      } catch (err) {
        pushToast({ text: `Copy failed: ${err}`, kind: "error" });
      }
    }
    setMenuPos(null);
  };

  const ctxPaste = async () => {
    try {
      const t = await navigator.clipboard.readText();
      if (t) await ipc.writeToAgent(agent.id, toB64(sanitizeForPty(t)));
    } catch (err) {
      pushToast({ text: `Paste failed: ${err}`, kind: "error" });
    }
    setMenuPos(null);
  };

  const ctxClear = () => {
    termRef.current?.clear();
    setMenuPos(null);
  };

  const ctxSplit = (dir: "h" | "v") => {
    setMenuPos(null);
    void split(dir);
  };

  const onDragEnter = (e: React.DragEvent<HTMLDivElement>) => {
    if (!e.dataTransfer.types.includes("Files")) return;
    e.preventDefault();
    setDragOver(true);
  };

  const onDragOver = (e: React.DragEvent<HTMLDivElement>) => {
    if (!e.dataTransfer.types.includes("Files")) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
  };

  const onDragLeave = (e: React.DragEvent<HTMLDivElement>) => {
    // Only clear when we actually leave the tile (not a child element).
    if (e.currentTarget.contains(e.relatedTarget as Node)) return;
    setDragOver(false);
  };

  const onDrop = async (e: React.DragEvent<HTMLDivElement>) => {
    if (!e.dataTransfer.files || e.dataTransfer.files.length === 0) return;
    e.preventDefault();
    setDragOver(false);
    const parts: string[] = [];
    for (let i = 0; i < e.dataTransfer.files.length; i += 1) {
      const f = e.dataTransfer.files[i] as TauriFile;
      const p = f.path && f.path.length > 0 ? f.path : f.name;
      parts.push(quoteIfNeeded(p));
    }
    if (parts.length === 0) return;
    const payload = parts.join(" ") + " ";
    try {
      await ipc.writeToAgent(agent.id, toB64(sanitizeForPty(payload)));
    } catch (err) {
      pushToast({ text: `Drop failed: ${err}`, kind: "error" });
    }
  };

  return (
    <div
      className={`agent-tile ${isFocused ? "focused" : ""} ${dragOver ? "drag-over" : ""} ${isDead ? "dead" : ""}`}
      onDragEnter={onDragEnter}
      onDragOver={onDragOver}
      onDragLeave={onDragLeave}
      onDrop={onDrop}
    >
      <div className="agent-tile-header">
        <span
          className={`status-dot tile-status-dot ${agentStatusClass(agent.status)}`}
          aria-label={`status: ${agent.status}`}
          title={agent.status}
        />
        <span className="chip chip--accent tile-tag">{agent.agent_type}</span>
        {isDead && <span className="chip chip--neutral tile-tag-dead">exited</span>}
        <ArchitectBadge agentId={agent.id} />
        <AgentTitle agentId={agent.id} />
        <AgentIdChip id={agent.id} onCopy={(text) => {
          navigator.clipboard.writeText(text).catch(() => undefined);
          pushToast({ text: `Copied ${shortId(text)}`, kind: "info" });
        }} />
        <div className="tile-actions">
          <button className="btn--icon" title="Split horizontally" onClick={() => split("h")}>
            <RowsIcon size={12} />
          </button>
          <button className="btn--icon" title="Split vertically" onClick={() => split("v")}>
            <ColumnsIcon size={12} />
          </button>
          <button
            className="btn--icon"
            title={isMaximized ? "Restore" : "Maximize"}
            onClick={() => setMaximized(isMaximized ? null : agent.id)}
          >
            {isMaximized ? <Minimize2 size={12} /> : <Maximize2 size={12} />}
          </button>
          <button className="btn--icon" title="Close" onClick={close}>
            <X size={12} />
          </button>
        </div>
      </div>
      <CommandBlocksBar blocks={blocks} />
      <div
        className="agent-tile-body"
        ref={bodyRef}
        tabIndex={-1}
        onContextMenu={(e) => {
          e.preventDefault();
          setMenuPos({ x: e.clientX, y: e.clientY });
        }}
      >
        <div ref={containerRef} style={{ width: "100%", height: "100%" }} />
        {isDead && (
          <div className="agent-tile-dead-overlay">
            <div className="agent-tile-dead-msg">
              <p>Process exited</p>
              <button onClick={respawn}>Respawn</button>
              <button onClick={close}>Remove</button>
            </div>
          </div>
        )}
        {searchOpen && (
          <div className="agent-tile-search" onClick={(e) => e.stopPropagation()}>
            <input
              ref={searchInputRef}
              autoFocus
              placeholder="Find"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              onKeyDown={onSearchKey}
            />
            <button title="Previous (Shift+Enter)" onClick={findPrev}>
              {"↑"}
            </button>
            <button title="Next (Enter)" onClick={findNext}>
              {"↓"}
            </button>
            <button
              title="Close (Esc)"
              onClick={() => {
                setSearchOpen(false);
                termRef.current?.focus();
              }}
            >
              {"✕"}
            </button>
          </div>
        )}
        {menuPos && (
          <div
            className="agent-context-menu"
            style={{ left: menuPos.x, top: menuPos.y }}
            onClick={(e) => e.stopPropagation()}
            onContextMenu={(e) => e.preventDefault()}
          >
            <button onClick={ctxCopy}>Copy</button>
            <button onClick={ctxPaste}>Paste</button>
            <button onClick={ctxClear}>Clear</button>
            <button onClick={() => ctxSplit("h")}>Split horizontally</button>
            <button onClick={() => ctxSplit("v")}>Split vertically</button>
          </div>
        )}
      </div>
    </div>
  );
}

function shortId(id: string): string {
  return id.slice(0, 8);
}

// Map agent runtime status to the §2 status-dot vocabulary.
function agentStatusClass(status: string): string {
  switch (status) {
    case "running":
    case "ok":
    case "healthy":
      return "running";
    case "exited":
    case "stopped":
    case "dead":
      return "exited";
    case "error":
    case "failed":
    case "crashed":
      return "error";
    case "streaming":
    case "busy":
    case "thinking":
      return "streaming";
    default:
      return "idle";
  }
}

const TASK_PRIORITY: Record<TaskStatus, number> = {
  in_progress: 0,
  in_review: 1,
  todo: 2,
  complete: 3,
  cancelled: 4,
};

function pickActiveTask(tasks: Record<string, Task>, agentId: string): Task | null {
  let best: Task | null = null;
  for (const t of Object.values(tasks)) {
    if (t.agent_id !== agentId) continue;
    if (!best) {
      best = t;
      continue;
    }
    const a = TASK_PRIORITY[t.status] ?? 9;
    const b = TASK_PRIORITY[best.status] ?? 9;
    if (a < b || (a === b && t.updated_at > best.updated_at)) best = t;
  }
  return best;
}

function AgentTitle({ agentId }: { agentId: string }) {
  const tasks = useStore((s) => s.tasks);
  const task = pickActiveTask(tasks, agentId);
  const summary = useAgentSummary(agentId, task?.title ?? null);
  return (
    <span className="title" title={summary}>
      {summary}
    </span>
  );
}

function AgentIdChip({
  id,
  onCopy,
}: {
  id: string;
  onCopy: (id: string) => void;
}) {
  return (
    <span
      className="agent-id-chip"
      title={`${id} — click to copy`}
      onClick={(e) => {
        e.stopPropagation();
        onCopy(id);
      }}
    >
      {shortId(id)}
    </span>
  );
}

function ArchitectBadge({ agentId }: { agentId: string }) {
  const sig = useArchitectStore((s) => s.signals[agentId]);
  if (!sig || sig === "unknown") return null;
  const color = signalColor(sig);
  return (
    <span
      className="architect-badge"
      title={`Architect: ${signalLabel(sig)}`}
      style={{ color }}
    >
      {signalLabel(sig)}
    </span>
  );
}
