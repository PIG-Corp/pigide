import { useEffect, useState } from "react";
import type { CommandBlock } from "./parser";

/**
 * CommandBlocksBar — slim timeline above an AgentTile that lists the most
 * recent OSC 133 command blocks. Clicking a block toggles its expanded view
 * (command + exit code + duration). Renders nothing until the agent emits
 * its first OSC 133 envelope, so users with shells that lack shell
 * integration just see the regular xterm.
 */
export function CommandBlocksBar({ blocks }: { blocks: CommandBlock[] }) {
  const [open, setOpen] = useState<number | null>(null);

  // Auto-collapse when the user starts a new command.
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setOpen(null);
  }, [blocks.length]);

  if (blocks.length === 0) return null;
  const last = blocks.slice(-12);
  return (
    <div className="cmdblock-bar">
      {last.map((b) => {
        const expanded = open === b.id;
        const status =
          b.exitCode === undefined
            ? "running"
            : b.exitCode === 0
              ? "ok"
              : "fail";
        const duration =
          b.endedAt && b.startedAt
            ? `${Math.max(0, b.endedAt - b.startedAt)}ms`
            : "…";
        return (
          <button
            key={b.id}
            className={`cmdblock cmdblock-${status} ${expanded ? "expanded" : ""}`}
            onClick={() => setOpen(expanded ? null : b.id)}
            title={b.command}
          >
            <span className="cmdblock-dot" />
            {expanded ? (
              <span className="cmdblock-detail">
                <span className="cmdblock-cmd">{b.command || "(no command)"}</span>
                <span className="cmdblock-meta">
                  {status === "running"
                    ? "…"
                    : `exit ${b.exitCode}, ${duration}`}
                </span>
              </span>
            ) : (
              <span className="cmdblock-summary">
                {b.command ? truncate(b.command, 28) : "(prompt)"}
                {b.exitCode !== undefined && b.exitCode !== 0
                  ? ` ✗${b.exitCode}`
                  : ""}
              </span>
            )}
          </button>
        );
      })}
    </div>
  );
}

function truncate(s: string, n: number): string {
  if (s.length <= n) return s;
  return s.slice(0, n - 1) + "…";
}
