// Interactive COI & Causal Schematic Viewer (`elkjs` layered RTL port-aware
// layout + HiDPI pan/zoom <canvas> + deterministic SVG export):
// - Active `why_cex` causal paths & mux arms highlighted in `--st-cex`
// - Arithmetic / state-space blowup hotspots (`mul`, `div`, wide datapath) in amber
// - Live signal values from `activeTraceValues` rendered on nets as you scrub time
// - Progressive `+1 stage` cone expansion and 3-way cross-probing into Source & Trace

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import ELK, { type ElkExtendedEdge, type ElkNode } from "elkjs/lib/elk.bundled.js";
import type { SchematicEdgeView, SchematicGraphView, SchematicNodeView } from "../backend";
import { useWorkbenchStore } from "../store/workbenchStore";
import { resolveSvgPalette } from "../svgTheme";

interface PositionedPort {
  id: string;
  label: string;
  dir: "in" | "out";
  x: number;
  y: number;
}

interface PositionedNode extends SchematicNodeView {
  x: number;
  y: number;
  w: number;
  h: number;
  laidPorts: PositionedPort[];
}

interface PositionedEdge extends SchematicEdgeView {
  points: { x: number; y: number }[];
}

interface LaidOutSchematic {
  width: number;
  height: number;
  nodes: PositionedNode[];
  edges: PositionedEdge[];
}

const elk = new ELK();

function nodeDimensions(n: SchematicNodeView): { w: number; h: number } {
  const inCount = n.ports.filter((p) => p.dir === "in").length;
  const baseH = Math.max(40, inCount * 18 + 16);
  if (n.kind === "mux") return { w: 68, h: Math.max(58, baseH) };
  if (n.kind === "register") return { w: 108, h: Math.max(52, baseH) };
  if (n.kind === "input" || n.kind === "target" || n.kind === "const") return { w: 112, h: 36 };
  return { w: Math.max(84, Math.min(148, n.label.length * 7.2 + 26)), h: baseH };
}

/** Instant deterministic layered fallback while `elk.layout` computes asynchronously. */
function fallbackLayout(graph: SchematicGraphView): LaidOutSchematic {
  const byDist = new Map<number, SchematicNodeView[]>();
  let maxDist = 0;
  for (const n of graph.nodes) {
    const col = n.kind === "target" ? -1 : n.distance;
    maxDist = Math.max(maxDist, col);
    const list = byDist.get(col) ?? [];
    list.push(n);
    byDist.set(col, list);
  }
  const cols = Array.from(byDist.keys()).sort((a, b) => b - a);
  const posByNode = new Map<string, PositionedNode>();
  const portCoords = new Map<string, { x: number; y: number }>();

  let curX = 36;
  let maxH = 240;
  for (const c of cols) {
    const list = byDist.get(c) ?? [];
    let curY = 36;
    let colW = 100;
    for (const n of list) {
      const { w, h } = nodeDimensions(n);
      colW = Math.max(colW, w);
      const ins = n.ports.filter((p) => p.dir === "in");
      const outs = n.ports.filter((p) => p.dir === "out");
      const laidPorts: PositionedPort[] = [];
      ins.forEach((p, idx) => {
        const py = (h * (idx + 1)) / (ins.length + 1);
        laidPorts.push({ ...p, x: 0, y: py });
        portCoords.set(p.id, { x: curX, y: curY + py });
      });
      outs.forEach((p, idx) => {
        const py = (h * (idx + 1)) / (outs.length + 1);
        laidPorts.push({ ...p, x: w, y: py });
        portCoords.set(p.id, { x: curX + w, y: curY + py });
      });
      posByNode.set(n.id, { ...n, x: curX, y: curY, w, h, laidPorts });
      curY += h + 46;
    }
    maxH = Math.max(maxH, curY + 36);
    curX += colW + 84;
  }

  const edges: PositionedEdge[] = graph.edges.map((e) => {
    const p0 = portCoords.get(e.source_port) ?? { x: 40, y: 40 };
    const p1 = portCoords.get(e.target_port) ?? { x: 140, y: 40 };
    const midX = (p0.x + p1.x) / 2;
    return {
      ...e,
      points: [p0, { x: midX, y: p0.y }, { x: midX, y: p1.y }, p1],
    };
  });

  return {
    width: Math.max(420, curX + 24),
    height: maxH,
    nodes: Array.from(posByNode.values()),
    edges,
  };
}

export async function computeElkLayout(graph: SchematicGraphView): Promise<LaidOutSchematic> {
  if (!graph.nodes.length) return { width: 400, height: 240, nodes: [], edges: [] };
  const elkGraph: ElkNode = {
    id: "root",
    layoutOptions: {
      "elk.algorithm": "layered",
      "elk.direction": "RIGHT",
      "elk.layered.spacing.nodeNodeBetweenLayers": "68",
      "elk.spacing.nodeNode": "38",
      "elk.spacing.edgeNode": "32",
      "elk.layered.spacing.edgeNodeBetweenLayers": "32",
      "elk.spacing.edgeEdge": "16",
      "elk.edgeRouting": "ORTHOGONAL",
      "elk.padding": "[top=32,left=32,bottom=48,right=32]",
    },
    children: graph.nodes.map((n) => {
      const { w, h } = nodeDimensions(n);
      const ins = n.ports.filter((p) => p.dir === "in");
      const outs = n.ports.filter((p) => p.dir === "out");
      return {
        id: n.id,
        width: w,
        height: h,
        // Reserve 30px beneath each node so bottom feedback wires clear the live value label (+12px) by >= 16px.
        layoutOptions: {
          "elk.portConstraints": "FIXED_ORDER",
          "elk.margins": "[top=4,left=4,bottom=30,right=4]",
        },
        ports: [
          ...ins.map((p, idx) => ({
            id: p.id,
            x: 0,
            y: Math.round((h * (idx + 1)) / (ins.length + 1)),
            layoutOptions: { "elk.port.side": "WEST", "elk.port.index": String(idx) },
          })),
          ...outs.map((p, idx) => ({
            id: p.id,
            x: w,
            y: Math.round((h * (idx + 1)) / (outs.length + 1)),
            layoutOptions: { "elk.port.side": "EAST", "elk.port.index": String(idx) },
          })),
        ],
      };
    }),
    edges: graph.edges.map((e) => ({
      id: e.id,
      sources: [e.source_port],
      targets: [e.target_port],
    })),
  };

  const res = await elk.layout(elkGraph);
  const byId = new Map(graph.nodes.map((n) => [n.id, n]));
  const portAbs = new Map<string, { x: number; y: number }>();

  const nodes: PositionedNode[] = (res.children ?? []).map((c) => {
    const orig = byId.get(c.id)!;
    const x = c.x ?? 0;
    const y = c.y ?? 0;
    const w = c.width ?? 90;
    const h = c.height ?? 44;
    const laidPorts: PositionedPort[] = (c.ports ?? []).map((ep) => {
      const origPort = orig.ports.find((p) => p.id === ep.id) ?? { id: ep.id, label: "", dir: "in" as const };
      const px = ep.x ?? (origPort.dir === "in" ? 0 : w);
      const py = ep.y ?? h / 2;
      portAbs.set(ep.id, { x: x + px, y: y + py });
      return { ...origPort, x: px, y: py };
    });
    return { ...orig, x, y, w, h, laidPorts };
  });

  const edgeById = new Map(graph.edges.map((e) => [e.id, e]));
  const edges: PositionedEdge[] = ((res.edges ?? []) as ElkExtendedEdge[]).map((ee) => {
    const orig = edgeById.get(ee.id)!;
    const sec = ee.sections?.[0];
    if (sec) {
      return {
        ...orig,
        points: [sec.startPoint, ...(sec.bendPoints ?? []), sec.endPoint],
      };
    }
    const p0 = portAbs.get(orig.source_port) ?? { x: 0, y: 0 };
    const p1 = portAbs.get(orig.target_port) ?? { x: 100, y: 0 };
    return { ...orig, points: [p0, p1] };
  });

  const maxNodeBottom = nodes.reduce((acc, n) => Math.max(acc, n.y + n.h + 36), 0);
  const maxEdgeBottom = edges.reduce(
    (acc, e) => e.points.reduce((a, p) => Math.max(a, p.y + 16), acc),
    0,
  );
  return {
    width: Math.max(420, (res.width ?? 600) + 32),
    height: Math.max(260, (res.height ?? 320) + 48, maxNodeBottom, maxEdgeBottom),
    nodes,
    edges,
  };
}

function escapeXml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

export function renderSchematicSvg(
  laid: LaidOutSchematic,
  values: Record<string, string>,
  dark?: boolean,
): string {
  const pal = resolveSvgPalette(dark);
  const out: string[] = [];
  out.push(`<svg xmlns="http://www.w3.org/2000/svg" width="${laid.width}" height="${laid.height}" viewBox="0 0 ${laid.width} ${laid.height}" font-family="IBM Plex Mono, monospace" font-size="11">`);
  out.push(`<rect width="${laid.width}" height="${laid.height}" fill="${pal.surface}" />`);
  for (const e of laid.edges) {
    if (e.points.length < 2) continue;
    const d = e.points.map((p, i) => `${i === 0 ? "M" : "L"}${p.x},${p.y}`).join(" ");
    const stroke = e.causal ? pal.stCex : pal.wireMuted;
    const sw = e.causal ? 2 : e.width > 1 ? 1.7 : 1.2;
    out.push(`<path d="${d}" fill="none" stroke="${stroke}" stroke-width="${sw}" />`);
  }
  for (const n of laid.nodes) {
    const stroke = n.hotspot ? pal.stUndetermined : n.causal ? pal.stCex : pal.text2;
    const fill = n.hotspot ? pal.hotspotFill : n.causal ? pal.causalFill : pal.nodeFill;
    out.push(`<rect x="${n.x}" y="${n.y}" width="${n.w}" height="${n.h}" rx="5" fill="${fill}" stroke="${stroke}" stroke-width="${n.causal || n.hotspot ? 1.8 : 1.2}" />`);
    out.push(`<text x="${n.x + n.w / 2}" y="${n.y + n.h / 2 + 4}" text-anchor="middle" fill="${pal.text}" font-weight="600">${escapeXml(n.label)}</text>`);
    if (n.power_domain) {
      const bw = Math.max(32, n.power_domain.length * 5.6 + 8);
      const bx = n.x + n.w - bw;
      const by = n.y - 12;
      out.push(`<rect x="${bx}" y="${by}" width="${bw}" height="12" rx="3" fill="${pal.accent}" />`);
      out.push(`<text x="${bx + bw / 2}" y="${by + 9}" text-anchor="middle" fill="#ffffff" font-size="8.5" font-weight="600">${escapeXml(n.power_domain)}</text>`);
    }
    const v = n.signal ? values[n.signal] ?? values[n.signal.split(".").pop() ?? ""] : undefined;
    if (v) {
      out.push(`<text x="${n.x + n.w / 2}" y="${n.y + n.h + 12}" text-anchor="middle" fill="${pal.accent}" font-size="10">${escapeXml(v)}</text>`);
    }
  }
  out.push(`</svg>`);
  return out.join("\n");
}

export function SchematicView({
  tabs,
  graph,
  loading,
  error,
  maxDepth,
  dark: darkProp,
  onChangeDepth,
  onSelectSignalSource,
  onAddSignalToTrace,
  onPinContext,
  onPopOut,
  onCaptureSvgRef,
}: {
  tabs?: React.ReactNode;
  graph: SchematicGraphView | null;
  loading?: boolean;
  error?: string | null;
  maxDepth: number;
  dark?: boolean;
  onChangeDepth: (nextDepth: number) => void;
  onSelectSignalSource?: (signal: string) => void;
  onAddSignalToTrace?: (signal: string) => void;
  onPinContext?: (label: string, detail: string) => void;
  onPopOut?: () => void;
  onCaptureSvgRef?: React.MutableRefObject<(() => string | null) | null>;
}) {
  const fallback = useMemo(() => (graph ? fallbackLayout(graph) : null), [graph]);
  const [laid, setLaid] = useState<LaidOutSchematic | null>(fallback);
  const [scale, setScale] = useState(1);
  const [pan, setPan] = useState({ x: 16, y: 16 });
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);
  const dragging = useRef<{ x0: number; y0: number; px: number; py: number } | null>(null);
  const hostRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);

  const activeTraceCycle = useWorkbenchStore((st) => st.activeTraceCycle);
  const activeTraceValues = useWorkbenchStore((st) => st.activeTraceValues);

  useEffect(() => {
    if (!graph) {
      setLaid(null);
      return;
    }
    setLaid(fallbackLayout(graph));
    let cancelled = false;
    computeElkLayout(graph)
      .then((res) => {
        if (!cancelled) setLaid(res);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [graph]);

  useEffect(() => {
    if (!onCaptureSvgRef) return;
    onCaptureSvgRef.current = () => (laid ? renderSchematicSvg(laid, activeTraceValues, darkProp) : null);
    return () => {
      onCaptureSvgRef.current = null;
    };
  }, [onCaptureSvgRef, laid, activeTraceValues, darkProp]);

  const selectedNode = useMemo(
    () => laid?.nodes.find((n) => n.id === selectedNodeId) ?? null,
    [laid, selectedNodeId],
  );

  const fitToView = useCallback(() => {
    const el = hostRef.current;
    if (!el || !laid) return;
    const vw = Math.max(300, el.clientWidth - 32);
    const vh = Math.max(200, el.clientHeight - 32);
    const nextScale = Math.max(0.35, Math.min(1.6, Math.min(vw / laid.width, vh / laid.height)));
    setScale(nextScale);
    setPan({ x: 16, y: 16 });
  }, [laid]);

  // Paint HiDPI <canvas> schematic
  useLayoutEffect(() => {
    const cvs = canvasRef.current;
    const host = hostRef.current;
    if (!cvs || !host || !laid) return;
    let ctx: CanvasRenderingContext2D | null = null;
    try {
      ctx = cvs.getContext("2d");
    } catch {
      ctx = null;
    }
    if (!ctx) return;

    const dpr = typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1;
    const vw = Math.max(320, host.clientWidth || 800);
    const vh = Math.max(220, host.clientHeight || 460);
    if (cvs.width !== Math.round(vw * dpr) || cvs.height !== Math.round(vh * dpr)) {
      cvs.width = Math.round(vw * dpr);
      cvs.height = Math.round(vh * dpr);
    }
    cvs.style.width = `${vw}px`;
    cvs.style.height = `${vh}px`;

    const pal = resolveSvgPalette(darkProp);
    const dark = pal.dark;

    const bg = pal.surface;
    const gridDot = dark ? "#2f2e2a" : "#e5e2d9";
    const wireMuted = pal.wireMuted;
    const wireCausal = pal.stCex;
    const hotspotStroke = dark ? "#f59e0b" : "#b45309";
    const hotspotFill = dark ? "rgba(245, 158, 11, 0.16)" : "#fef3c7";
    const causalFill = dark ? "rgba(240, 138, 93, 0.14)" : "#fff7ed";
    const nodeFill = pal.nodeFill;
    const nodeStroke = dark ? "#5b584f" : "#6a665d";
    const textCol = pal.text;
    const valCol = pal.accent;

    ctx.save();
    ctx.scale(dpr, dpr);
    ctx.fillStyle = bg;
    ctx.fillRect(0, 0, vw, vh);

    // Subtle schematic dot grid
    ctx.fillStyle = gridDot;
    for (let gx = (pan.x % 24 + 24) % 24; gx < vw; gx += 24) {
      for (let gy = (pan.y % 24 + 24) % 24; gy < vh; gy += 24) {
        ctx.fillRect(gx, gy, 1.2, 1.2);
      }
    }

    ctx.translate(pan.x, pan.y);
    ctx.scale(scale, scale);

    // Draw orthogonal nets
    for (const e of laid.edges) {
      if (e.points.length < 2) continue;
      const hi = selectedNodeId !== null && (e.source === selectedNodeId || e.target === selectedNodeId);
      ctx.strokeStyle = hi ? valCol : e.causal ? wireCausal : wireMuted;
      ctx.lineWidth = e.causal || hi ? 2.1 : e.width > 1 ? 1.7 : 1.15;
      ctx.beginPath();
      e.points.forEach((pt, idx) => {
        if (idx === 0) ctx!.moveTo(pt.x, pt.y);
        else ctx!.lineTo(pt.x, pt.y);
      });
      ctx.stroke();

      if (e.inverted) {
        const last = e.points[e.points.length - 1];
        ctx.beginPath();
        ctx.arc(last.x - 4, last.y, 3.2, 0, Math.PI * 2);
        ctx.fillStyle = bg;
        ctx.fill();
        ctx.stroke();
      }
    }

    // Draw RTL symbol nodes
    ctx.font = '600 11px "IBM Plex Mono", ui-monospace, monospace';
    for (const n of laid.nodes) {
      const isSel = n.id === selectedNodeId;
      ctx.fillStyle = n.hotspot ? hotspotFill : n.causal ? causalFill : nodeFill;
      ctx.strokeStyle = isSel ? valCol : n.hotspot ? hotspotStroke : n.causal ? wireCausal : nodeStroke;
      ctx.lineWidth = isSel ? 2.4 : n.hotspot || n.causal ? 1.8 : 1.2;

      ctx.beginPath();
      if (n.kind === "mux") {
        // Trapezoid multiplexer symbol
        ctx.moveTo(n.x, n.y);
        ctx.lineTo(n.x + n.w, n.y + 10);
        ctx.lineTo(n.x + n.w, n.y + n.h - 10);
        ctx.lineTo(n.x, n.y + n.h);
        ctx.closePath();
      } else if (n.kind === "input") {
        // Input flag pointing right
        ctx.moveTo(n.x, n.y + 4);
        ctx.lineTo(n.x + n.w - 10, n.y + 4);
        ctx.lineTo(n.x + n.w, n.y + n.h / 2);
        ctx.lineTo(n.x + n.w - 10, n.y + n.h - 4);
        ctx.lineTo(n.x, n.y + n.h - 4);
        ctx.closePath();
      } else if (n.kind === "target") {
        // Target check flag
        ctx.roundRect(n.x, n.y + 2, n.w, n.h - 4, 6);
      } else {
        ctx.roundRect(n.x, n.y, n.w, n.h, n.kind === "register" ? 3 : 8);
      }
      ctx.fill();
      ctx.stroke();

      // D-FF clock wedge on sequential registers
      if (n.kind === "register") {
        ctx.beginPath();
        ctx.moveTo(n.x, n.y + n.h - 14);
        ctx.lineTo(n.x + 8, n.y + n.h - 9);
        ctx.lineTo(n.x, n.y + n.h - 4);
        ctx.stroke();
      }

      // Port pin labels
      ctx.font = '9.5px "IBM Plex Mono", ui-monospace, monospace';
      ctx.fillStyle = wireMuted;
      for (const p of n.laidPorts) {
        ctx.textAlign = p.dir === "in" ? "left" : "right";
        ctx.fillText(p.label, n.x + (p.dir === "in" ? 5 : n.w - 5), n.y + p.y + 3);
      }

      // Node main label
      ctx.font = '600 11px "IBM Plex Mono", ui-monospace, monospace';
      ctx.fillStyle = textCol;
      ctx.textAlign = "center";
      ctx.fillText(n.label, n.x + n.w / 2, n.y + n.h / 2 + 4);

      // Expandable cone badge (`+`)
      if (n.expandable) {
        ctx.fillStyle = valCol;
        ctx.fillText("⊕", n.x - 8, n.y + n.h / 2 + 4);
      }

      // UPF power domain tag badge
      if (n.power_domain) {
        ctx.font = '600 8.5px "IBM Plex Mono", ui-monospace, monospace';
        const bw = Math.max(32, n.power_domain.length * 5.6 + 8);
        const bx = n.x + n.w - bw;
        const by = n.y - 12;
        ctx.fillStyle = valCol;
        ctx.beginPath();
        ctx.roundRect(bx, by, bw, 12, 3);
        ctx.fill();
        ctx.fillStyle = "#ffffff";
        ctx.textAlign = "center";
        ctx.fillText(n.power_domain, bx + bw / 2, by + 9);
      }

      // Live trace value at cursor cycle
      const liveVal = n.signal
        ? activeTraceValues[n.signal] ?? activeTraceValues[n.signal.split(".").pop() ?? ""]
        : undefined;
      if (liveVal) {
        ctx.font = '10px "IBM Plex Mono", ui-monospace, monospace';
        ctx.fillStyle = valCol;
        ctx.fillText(liveVal, n.x + n.w / 2, n.y + n.h + 12);
      }
    }

    ctx.restore();
  }, [laid, scale, pan, selectedNodeId, activeTraceValues, darkProp]);

  const handleCanvasClick = (e: React.MouseEvent<HTMLCanvasElement>) => {
    if (!laid || !canvasRef.current) return;
    const r = canvasRef.current.getBoundingClientRect();
    const gx = (e.clientX - r.left - pan.x) / scale;
    const gy = (e.clientY - r.top - pan.y) / scale;
    const hit = laid.nodes.find((n) => gx >= n.x - 12 && gx <= n.x + n.w && gy >= n.y && gy <= n.y + n.h);
    if (hit) {
      setSelectedNodeId(hit.id);
      if (hit.expandable && gx < hit.x + 4) {
        onChangeDepth(maxDepth + 1);
      }
    } else {
      setSelectedNodeId(null);
    }
  };

  return (
    <div className="schematic-pane">
      {tabs && <div className="strip">{tabs}</div>}
      <div className="trace-bar schematic-bar">
        <span className="strip-summary">
          COI Schematic{graph ? `: ${graph.property} · ${graph.nodes.length} nodes · ${maxDepth} stage${maxDepth === 1 ? "" : "s"}` : ""}
        </span>
        {activeTraceCycle !== null && (
          <span className="source-live-badge mono">● Values @C{activeTraceCycle}</span>
        )}
        <span className="spacer" />
        <span className="schematic-legend small">
          <span className="leg-dot leg-causal" /> Causal path
          <span className="leg-dot leg-hotspot" /> Arith hotspot
        </span>
        <span className="zoom">
          <button
            className="tool"
            disabled={maxDepth <= 0 || loading}
            onClick={() => onChangeDepth(Math.max(0, maxDepth - 1))}
            aria-label="Fewer cone stages"
            title="Contract cone depth (-1 register stage)"
          >
            −
          </button>
          <span className="zoom-text mono" title="Sequential register stages in cone">
            d={maxDepth}
          </span>
          <button
            className="tool"
            disabled={maxDepth >= 6 || loading}
            onClick={() => onChangeDepth(maxDepth + 1)}
            aria-label="More cone stages"
            title="Expand cone depth (+1 register stage)"
          >
            +
          </button>
        </span>
        <button className="btn small" onClick={fitToView} disabled={!laid}>
          Fit
        </button>
        {onPopOut && (
          <button className="btn small" onClick={onPopOut} title="Pop schematic out into a secondary window">
            ↗
          </button>
        )}
      </div>

      {selectedNode && (
        <div className="schematic-inspector" role="region" aria-label="Selected schematic node">
          <span className="mono strong">{selectedNode.signal ?? selectedNode.label}</span>
          <span className="muted small">
            {selectedNode.kind} · {selectedNode.width}b · stage {selectedNode.distance}
            {selectedNode.power_domain ? ` · domain ${selectedNode.power_domain}` : ""}
            {selectedNode.causal ? " · Why-CEX active" : ""}
            {selectedNode.hotspot ? " · Complexity hotspot" : ""}
          </span>
          <span className="spacer" />
          {selectedNode.expandable && (
            <button className="btn small" onClick={() => onChangeDepth(maxDepth + 1)}>
              +1 Stage Fan-in
            </button>
          )}
          {selectedNode.signal && onSelectSignalSource && (
            <button className="btn small" onClick={() => onSelectSignalSource(selectedNode.signal!)}>
              Jump to Source
            </button>
          )}
          {selectedNode.signal && onAddSignalToTrace && (
            <button className="btn small" onClick={() => onAddSignalToTrace(selectedNode.signal!)}>
              Add to Trace
            </button>
          )}
          {selectedNode.signal && onPinContext && (
            <button
              className="btn small"
              onClick={() =>
                onPinContext(
                  `@${selectedNode.signal!.split(".").pop()}`,
                  `Schematic node ${selectedNode.signal} (${selectedNode.kind}, ${selectedNode.width}b)`,
                )
              }
            >
              ✦ Pin
            </button>
          )}
        </div>
      )}

      {loading && !laid ? (
        <p className="empty">Extracting RTL cone schematic…</p>
      ) : error ? (
        <p className="empty">Schematic unavailable: {error}</p>
      ) : !laid || !laid.nodes.length ? (
        <p className="empty">Select an assertion or cover property to inspect its cone schematic.</p>
      ) : (
        <div
          className="schematic-canvas-wrap"
          ref={hostRef}
          onWheel={(e) => {
            e.preventDefault();
            if (e.ctrlKey || e.metaKey) {
              setScale((s) => Math.max(0.25, Math.min(2.5, s * (e.deltaY < 0 ? 1.15 : 0.87))));
            } else {
              setPan((p) => ({ x: p.x - e.deltaX, y: p.y - e.deltaY }));
            }
          }}
        >
          <canvas
            ref={canvasRef}
            role="img"
            aria-label={`RTL cone schematic for ${graph?.property ?? "property"}, ${laid.nodes.length} nodes`}
            onClick={handleCanvasClick}
            onPointerDown={(e) => {
              dragging.current = { x0: e.clientX, y0: e.clientY, px: pan.x, py: pan.y };
            }}
            onPointerMove={(e) => {
              if (!dragging.current) return;
              const dx = e.clientX - dragging.current.x0;
              const dy = e.clientY - dragging.current.y0;
              if (Math.abs(dx) > 3 || Math.abs(dy) > 3) {
                setPan({ x: dragging.current.px + dx, y: dragging.current.py + dy });
              }
            }}
            onPointerUp={() => {
              dragging.current = null;
            }}
          />
          {/* Accessible DOM node summary list for keyboard & screen-reader users */}
          <ul className="sr-only" aria-label="Schematic nodes">
            {laid.nodes.map((n) => (
              <li key={n.id}>
                <button type="button" onClick={() => setSelectedNodeId(n.id)}>
                  {n.signal ?? n.label} ({n.kind}, {n.width} bits{n.power_domain ? `, domain ${n.power_domain}` : ""}{n.causal ? ", active causal path" : ""}{n.hotspot ? ", complexity hotspot" : ""})
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}
