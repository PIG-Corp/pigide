import { useEffect, useRef, useState } from "react";
import ForceGraph2D from "react-force-graph-2d";
import { ipc } from "../state/ipc";
import { useStore } from "../state/store";
import type { GraphData, GraphNode } from "../state/types";

interface RFNode extends GraphNode {
  // ForceGraph mutates these.
  x?: number;
  y?: number;
  vx?: number;
  vy?: number;
}
interface RFLink {
  source: string;
  target: string;
  ambiguous: boolean;
}

const UNRESOLVED = "__unresolved__";

export function MemoryGraph({ onSelect }: { onSelect?: (id: string) => void }) {
  const currentId = useStore((s) => s.currentId);
  const pushToast = useStore((s) => s.pushToast);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [size, setSize] = useState<{ w: number; h: number }>({ w: 0, h: 0 });
  const [data, setData] = useState<{ nodes: RFNode[]; links: RFLink[] } | null>(null);

  // Track container size for the canvas.
  useEffect(() => {
    if (!containerRef.current) return;
    const el = containerRef.current;
    const ro = new ResizeObserver(() => {
      const r = el.getBoundingClientRect();
      setSize({ w: Math.floor(r.width), h: Math.floor(r.height) });
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  useEffect(() => {
    if (!currentId) {
      setData(null);
      return;
    }
    ipc
      .memoryGraph(currentId)
      .then((g: GraphData) => setData(transform(g)))
      .catch((err) => pushToast({ text: `memory_graph: ${err}`, kind: "error" }));
  }, [currentId, pushToast]);

  if (!currentId) {
    return <div className="empty-state">No workspace selected</div>;
  }
  if (!data || data.nodes.length === 0) {
    return <div className="empty-state">Empty graph — create a memory first.</div>;
  }

  // Resolve theme colours once per render. The canvas API needs concrete
  // strings, so we read computed CSS variables off <html> — this keeps the
  // graph in sync with the active palette.
  const styles =
    typeof document !== "undefined"
      ? getComputedStyle(document.documentElement)
      : null;
  const colorBg = (styles?.getPropertyValue("--bg") ?? "#0a0b0e").trim() || "#0a0b0e";
  const colorAccent = (styles?.getPropertyValue("--accent") ?? "#5d7ff5").trim() || "#5d7ff5";
  const colorMuted = (styles?.getPropertyValue("--fg-muted") ?? "#8a8f99").trim() || "#8a8f99";
  const colorSubtle = (styles?.getPropertyValue("--fg-subtle") ?? "#6b7280").trim() || "#6b7280";
  const colorBorder = (styles?.getPropertyValue("--border-strong") ?? "#2a2d34").trim() || "#2a2d34";
  const colorWarn = (styles?.getPropertyValue("--warn") ?? "#f59e0b").trim() || "#f59e0b";
  const colorFg = (styles?.getPropertyValue("--fg") ?? "#d6d8dd").trim() || "#d6d8dd";

  return (
    <div className="memory-graph" ref={containerRef}>
      {size.w > 0 ? (
        <ForceGraph2D
          width={size.w}
          height={size.h}
          graphData={data}
          nodeRelSize={4}
          nodeLabel={(n: any) => (n as RFNode).title || (n as RFNode).id}
          nodeColor={(n: any) =>
            (n as RFNode).id === UNRESOLVED ? colorSubtle : colorAccent
          }
          linkColor={(l: any) => ((l as RFLink).ambiguous ? colorWarn : colorBorder)}
          linkWidth={1}
          backgroundColor={colorBg}
          cooldownTicks={50}
          onNodeClick={(n: any) => {
            const id = (n as RFNode).id;
            if (id !== UNRESOLVED && onSelect) onSelect(id);
          }}
          nodeCanvasObject={(n: any, ctx, scale) => {
            const node = n as RFNode;
            const r = 4;
            ctx.fillStyle =
              node.id === UNRESOLVED ? colorMuted : colorAccent;
            ctx.beginPath();
            ctx.arc(node.x ?? 0, node.y ?? 0, r, 0, 2 * Math.PI);
            ctx.fill();
            if (scale > 1.4) {
              ctx.fillStyle = colorFg;
              ctx.font = `${10 / scale}px sans-serif`;
              ctx.textAlign = "center";
              ctx.textBaseline = "top";
              ctx.fillText(
                node.title || node.id.slice(0, 8),
                node.x ?? 0,
                (node.y ?? 0) + r + 1,
              );
            }
          }}
        />
      ) : null}
    </div>
  );
}

function transform(g: GraphData): { nodes: RFNode[]; links: RFLink[] } {
  // ForceGraph wants source/target as either string ids or node objects.
  // We keep dangling links to a synthetic UNRESOLVED node so user sees orphans.
  const nodes: RFNode[] = g.nodes.map((n) => ({ ...n }));
  const ids = new Set(nodes.map((n) => n.id));
  const links: RFLink[] = [];
  let needUnresolved = false;
  for (const e of g.links) {
    if (e.target && ids.has(e.target)) {
      links.push({
        source: e.source,
        target: e.target,
        ambiguous: e.ambiguous,
      });
    } else {
      needUnresolved = true;
      links.push({
        source: e.source,
        target: UNRESOLVED,
        ambiguous: e.ambiguous,
      });
    }
  }
  if (needUnresolved) {
    nodes.push({
      id: UNRESOLVED,
      slug: "(unresolved)",
      title: "(unresolved)",
      tags: [],
    });
  }
  return { nodes, links };
}
