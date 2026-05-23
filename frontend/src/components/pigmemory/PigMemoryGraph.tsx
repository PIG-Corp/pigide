// Force-directed graph for the PigMemory workbench. Larger / fancier than
// the side-panel MemoryGraph: search-with-pulse, ego-mode (shift-hover dims
// everything except the focused node + its 2-hop neighborhood), node sizing
// by degree, tag-aware coloring.

import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
} from "react";
import ForceGraph2D from "react-force-graph-2d";
import type { GraphData, GraphNode } from "../../state/types";

interface RFNode extends GraphNode {
  x?: number;
  y?: number;
  vx?: number;
  vy?: number;
  degree?: number;
  __order?: number;
}
interface RFLink {
  source: string;
  target: string;
  ambiguous: boolean;
}

interface ForceGraphHandle {
  centerAt(x: number, y: number, ms: number): void;
  zoom(scale: number, ms: number): void;
}

const UNRESOLVED = "__unresolved__";

export interface PigMemoryGraphHandle {
  focusNode: (id: string) => void;
}

export const PigMemoryGraph = forwardRef<
  PigMemoryGraphHandle,
  {
    data: GraphData | null;
    activeId: string | null;
    onSelect: (id: string) => void;
    searchTerm?: string;
  }
>(function PigMemoryGraph(
  { data, activeId, onSelect, searchTerm },
  ref,
) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const fgRef = useRef<ForceGraphHandle | null>(null);
  const [size, setSize] = useState<{ w: number; h: number }>({ w: 0, h: 0 });
  const [hovered, setHovered] = useState<string | null>(null);
  const [egoMode, setEgoMode] = useState(false);

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      const r = el.getBoundingClientRect();
      setSize({ w: Math.floor(r.width), h: Math.floor(r.height) });
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Shift") setEgoMode(true);
    };
    const offKey = (e: KeyboardEvent) => {
      if (e.key === "Shift") setEgoMode(false);
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("keyup", offKey);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("keyup", offKey);
    };
  }, []);

  const transformed = useMemo(() => {
    if (!data) return null;
    const nodes: RFNode[] = data.nodes.map((n, i) => ({ ...n, __order: i, degree: 0 }));
    const ids = new Set(nodes.map((n) => n.id));
    const links: RFLink[] = [];
    let needUnresolved = false;
    const degMap = new Map<string, number>();
    for (const e of data.links) {
      const t = e.target && ids.has(e.target) ? e.target : UNRESOLVED;
      if (t === UNRESOLVED) needUnresolved = true;
      links.push({ source: e.source, target: t, ambiguous: e.ambiguous });
      degMap.set(e.source, (degMap.get(e.source) ?? 0) + 1);
      degMap.set(t, (degMap.get(t) ?? 0) + 1);
    }
    if (needUnresolved) {
      nodes.push({
        id: UNRESOLVED,
        slug: "(unresolved)",
        title: "(unresolved)",
        tags: [],
        __order: nodes.length,
        degree: degMap.get(UNRESOLVED) ?? 0,
      });
    }
    for (const n of nodes) n.degree = degMap.get(n.id) ?? 0;
    return { nodes, links };
  }, [data]);

  // Two-hop neighborhood for ego mode + active highlighting.
  const neighborhood = useMemo(() => {
    const focus = hovered ?? activeId;
    if (!focus || !transformed) return null;
    const adj = new Map<string, Set<string>>();
    for (const l of transformed.links) {
      if (!adj.has(l.source)) adj.set(l.source, new Set());
      if (!adj.has(l.target)) adj.set(l.target, new Set());
      adj.get(l.source)!.add(l.target);
      adj.get(l.target)!.add(l.source);
    }
    const out = new Set<string>([focus]);
    const first = adj.get(focus);
    if (first) {
      for (const f of first) {
        out.add(f);
        const second = adj.get(f);
        if (second) for (const s of second) out.add(s);
      }
    }
    return out;
  }, [hovered, activeId, transformed]);

  useImperativeHandle(ref, () => ({
    focusNode(id: string) {
      const fg = fgRef.current;
      if (!fg || !transformed) return;
      const n = transformed.nodes.find((x) => x.id === id);
      if (!n || n.x == null || n.y == null) return;
      fg.centerAt(n.x, n.y, 600);
      fg.zoom(2.4, 600);
    },
  }));

  if (!data) {
    return (
      <div ref={containerRef} className="pigmem-graph">
        <div className="pigmem-empty">Loading graph…</div>
      </div>
    );
  }
  if (transformed && transformed.nodes.length === 0) {
    return (
      <div ref={containerRef} className="pigmem-graph">
        <div className="pigmem-empty">
          The graph is empty. Create a note with `[[wikilinks]]` to see
          connections appear here.
        </div>
      </div>
    );
  }

  const styles =
    typeof document !== "undefined"
      ? getComputedStyle(document.documentElement)
      : null;
  const colorBg = (styles?.getPropertyValue("--bg") ?? "#0B0C0F").trim() || "#0B0C0F";
  const colorAccent = (styles?.getPropertyValue("--accent") ?? "#E89A4A").trim() || "#E89A4A";
  const colorMuted = (styles?.getPropertyValue("--fg-muted") ?? "#8A8F99").trim() || "#8A8F99";
  const colorSubtle = (styles?.getPropertyValue("--fg-subtle") ?? "#5A5F69").trim() || "#5A5F69";
  const colorBorder = (styles?.getPropertyValue("--border-strong") ?? "#2A2F38").trim() || "#2A2F38";
  const colorWarn = (styles?.getPropertyValue("--warn") ?? "#E89A4A").trim() || "#E89A4A";
  const colorFg = (styles?.getPropertyValue("--fg") ?? "#E4E6EA").trim() || "#E4E6EA";
  const colorInfo = (styles?.getPropertyValue("--info") ?? "#60A5FA").trim() || "#60A5FA";

  const search = (searchTerm ?? "").trim().toLowerCase();

  return (
    <div ref={containerRef} className="pigmem-graph">
      {size.w > 0 && transformed ? (
        <ForceGraph2D
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          ref={fgRef as unknown as React.MutableRefObject<any>}
          width={size.w}
          height={size.h}
          graphData={transformed}
          backgroundColor={colorBg}
          nodeRelSize={5}
          cooldownTicks={80}
          d3VelocityDecay={0.32}
          linkColor={(l: unknown) =>
            (l as RFLink).ambiguous ? colorWarn : colorBorder
          }
          linkWidth={(l: unknown) => {
            if (!neighborhood) return 1.1;
            const link = l as RFLink;
            return neighborhood.has(link.source) && neighborhood.has(link.target)
              ? 1.8
              : 0.6;
          }}
          linkDirectionalParticles={(l: unknown) => {
            if (!neighborhood) return 0;
            const link = l as RFLink;
            return neighborhood.has(link.source) && neighborhood.has(link.target)
              ? 1
              : 0;
          }}
          linkDirectionalParticleSpeed={0.005}
          linkDirectionalParticleColor={() => colorAccent}
          onNodeClick={(n: unknown) => {
            const id = (n as RFNode).id;
            if (id !== UNRESOLVED) onSelect(id);
          }}
          onNodeHover={(n: unknown) => {
            setHovered(n ? (n as RFNode).id : null);
          }}
          nodeCanvasObject={(n: unknown, ctx: CanvasRenderingContext2D, scale: number) => {
            const node = n as RFNode;
            const baseR = 4 + Math.min(8, Math.sqrt(node.degree ?? 0) * 1.4);
            const isActive = node.id === activeId;
            const inEgo =
              egoMode && neighborhood ? neighborhood.has(node.id) : true;
            const matchesSearch =
              search.length > 0 &&
              (node.title.toLowerCase().includes(search) ||
                node.slug.toLowerCase().includes(search));
            const dim = (egoMode && !inEgo) || (search.length > 0 && !matchesSearch);
            const r = isActive ? baseR + 2 : baseR;
            const cx = node.x ?? 0;
            const cy = node.y ?? 0;

            // Halo for active / matched nodes.
            if (isActive || matchesSearch) {
              ctx.beginPath();
              ctx.arc(cx, cy, r + 5, 0, 2 * Math.PI);
              ctx.fillStyle = isActive ? colorAccent + "33" : colorInfo + "33";
              ctx.fill();
            }

            ctx.beginPath();
            ctx.arc(cx, cy, r, 0, 2 * Math.PI);
            ctx.globalAlpha = dim ? 0.18 : 1;
            if (node.id === UNRESOLVED) {
              ctx.fillStyle = colorSubtle;
            } else if (isActive) {
              ctx.fillStyle = colorAccent;
            } else if (matchesSearch) {
              ctx.fillStyle = colorInfo;
            } else {
              ctx.fillStyle = colorMuted;
            }
            ctx.fill();

            if (scale > 1.2 || isActive || matchesSearch) {
              ctx.fillStyle = colorFg;
              ctx.font = `${Math.min(13, 11 / scale + 6)}px var(--font-sans, sans-serif)`;
              ctx.textAlign = "center";
              ctx.textBaseline = "top";
              const label = node.title || node.id.slice(0, 8);
              const truncated =
                label.length > 24 ? label.slice(0, 22) + "…" : label;
              ctx.fillText(truncated, cx, cy + r + 2);
            }
            ctx.globalAlpha = 1;
          }}
          nodePointerAreaPaint={(n: unknown, color: string, ctx: CanvasRenderingContext2D) => {
            const node = n as RFNode;
            const r = 4 + Math.min(8, Math.sqrt(node.degree ?? 0) * 1.4);
            ctx.fillStyle = color;
            ctx.beginPath();
            ctx.arc(node.x ?? 0, node.y ?? 0, r + 4, 0, 2 * Math.PI);
            ctx.fill();
          }}
        />
      ) : null}
      <div className="pigmem-graph-legend">
        <span className="pigmem-graph-legend-row">
          <span className="dot" style={{ background: colorAccent }} />
          active
        </span>
        <span className="pigmem-graph-legend-row">
          <span className="dot" style={{ background: colorInfo }} />
          search match
        </span>
        <span className="pigmem-graph-legend-row">
          <span className="dot" style={{ background: colorSubtle }} />
          unresolved
        </span>
        <span className="pigmem-graph-legend-row pigmem-graph-legend-hint">
          shift = ego
        </span>
      </div>
    </div>
  );
});
