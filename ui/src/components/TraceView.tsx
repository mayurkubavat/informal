// The trace tab: a CEX or cover witness as an interactive HiDPI <canvas>
// waveform backed by binary `IFTR` IPC (`Uint8Array`) or VCD text, with
// virtualized signal rows for 500+ signal traces, min/max zoom decimation for
// 5,000+ cycle traces, `why_cex` causal slice overlays, AI callout badges,
// next/prev transition jump (`N` / `P`), `@context` Ask-AI pin, and
// deterministic SVG capture (`gui_capture_view`).

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CausalEvent, WhyCexReport } from "../backend";
import { STATUS_LABEL, type ViewProp } from "../model";
import { useWorkbenchStore } from "../store/workbenchStore";
import { decodeIftr, renderTraceSvg, type TraceAnnotation } from "../traceBin";
import { resolveSvgPalette } from "../svgTheme";
import { cycleTimes, formatValue, parseVcd, type Radix, type Signal, type Vcd } from "../vcd";

export const ZOOM = [40, 60, 80, 120, 160];
export const ZOOM_DEFAULT = 60;
const LANE = 26;
const HEAD = 22;
const NAME_W = 200;
const VIRT_THRESHOLD = 64;
const SEMANTIC_CELL_CAP = 2000;

const helper = (s: Signal) => s.path === "informal" || s.path.startsWith("informal.");

function matchesCausalPath(vcdPath: string, causalSig: string): boolean {
  if (vcdPath === causalSig) return true;
  const vTail = vcdPath.split(".").slice(1).join(".");
  const cTail = causalSig.split(".").slice(1).join(".");
  if (vTail && (vTail === causalSig || vcdPath === cTail)) return true;
  return vcdPath.endsWith(`.${causalSig}`) || causalSig.endsWith(`.${vcdPath}`);
}

export function TraceView({
  tabs,
  prop,
  path,
  text,
  binary,
  error,
  dark: darkProp,
  addRequest,
  onAddResult,
  cursorRequest,
  onView,
  actions,
  clock,
  whyCex,
  whyCexOpen,
  whyCexLoading,
  onToggleWhyCex,
  annotations,
  controlledSliceOnly,
  onSliceOnlyChange,
  onPinContext,
  onPopOut,
  onCaptureSvgRef,
}: {
  tabs?: React.ReactNode;
  prop: ViewProp;
  path: string;
  text: string | null;
  binary?: Uint8Array | null;
  error?: string | null;
  dark?: boolean;
  addRequest?: { path: string; n: number } | null;
  onAddResult?: (path: string, result: "added" | "shown" | "missing") => void;
  cursorRequest?: { cycle: number; n: number } | null;
  onView?: (view: { cursor: number; signals: string[]; sliceOnly?: boolean }) => void;
  actions?: React.ReactNode;
  clock?: { signal: string; edge?: string } | null;
  whyCex?: WhyCexReport | null;
  whyCexOpen?: boolean;
  whyCexLoading?: boolean;
  onToggleWhyCex?: () => void;
  annotations?: TraceAnnotation[];
  controlledSliceOnly?: boolean | null;
  onSliceOnlyChange?: (v: boolean) => void;
  onPinContext?: (label: string, detail: string) => void;
  onPopOut?: () => void;
  onCaptureSvgRef?: React.MutableRefObject<(() => string | null) | null>;
}) {
  const vcd = useMemo<Vcd | null>(() => {
    if (binary && binary.byteLength >= 14) {
      try {
        return decodeIftr(binary);
      } catch {
        // fall back to text VCD if present
      }
    }
    return text ? parseVcd(text) : null;
  }, [binary, text]);

  const clockSignal = clock?.signal;
  const clockEdge = clock?.edge;
  const cycles = useMemo(
    () => (vcd ? cycleTimes(vcd, clockSignal ? { signal: clockSignal, edge: clockEdge } : null) : []),
    [vcd, clockSignal, clockEdge],
  );
  const n = cycles.length;
  const depth = prop.status.depth ?? Math.max(0, n - 1);
  const isWitness = prop.status.status === "proven" || prop.status.status === "bounded_proven";
  const cover = prop.status.status === "covered" || isWitness;
  const word = isWitness ? "witness" : cover ? "covered" : "cex";

  const [px, setPx] = useState<number>(ZOOM_DEFAULT);
  const [cursor, setCursor] = useState(0);
  const [selectedSig, setSelectedSig] = useState<string | null>(null);
  const [shown, setShown] = useState<string[]>([]);
  const [radix, setRadix] = useState<Record<string, Radix>>({});
  const [adding, setAdding] = useState(false);
  const [sliceOnlyLocal, setSliceOnlyLocal] = useState(false);
  const [scrollPos, setScrollPos] = useState({ left: 0, top: 0, vw: 1000, vh: 500 });
  const scroller = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);

  const sliceOnly = controlledSliceOnly ?? sliceOnlyLocal;
  const setSliceOnly = (v: boolean) => {
    setSliceOnlyLocal(v);
    onSliceOnlyChange?.(v);
  };

  const setActiveTraceValues = useWorkbenchStore((st) => st.setActiveTraceValues);
  const detachFollowOnUserAction = useWorkbenchStore((st) => st.detachFollowOnUserAction);

  const shownRef = useRef<string[]>([]);
  shownRef.current = shown;
  const shownFor = useRef<Vcd | null>(null);
  const reveal = useRef(false);

  useEffect(() => {
    if (!vcd) return;
    shownFor.current = vcd;
    shownRef.current = vcd.signals.filter((s) => !helper(s)).map((s) => s.path);
    setShown(shownRef.current);
    setCursor(Math.max(0, Math.min(n - 1, depth)));
    setAdding(false);
    setSliceOnlyLocal(false);
    reveal.current = true;
  }, [vcd, n, depth]);

  const byPath = useMemo(() => new Map(vcd?.signals.map((s) => [s.path, s]) ?? []), [vcd]);

  const handled = useRef(0);
  useEffect(() => {
    if (!addRequest || !vcd || shownFor.current !== vcd || handled.current === addRequest.n) return;
    handled.current = addRequest.n;
    const { path: p } = addRequest;
    const result = !byPath.has(p) ? "missing" : shownRef.current.includes(p) ? "shown" : "added";
    if (result === "added") {
      shownRef.current = [...shownRef.current, p];
      setShown(shownRef.current);
    }
    onAddResult?.(p, result);
  }, [addRequest, vcd, byPath, onAddResult]);

  const showSlice = !!whyCexOpen && !!whyCex;
  const isCausalSig = useCallback(
    (vcdPath: string) => showSlice && whyCex!.active_signals.some((cs) => matchesCausalPath(vcdPath, cs)),
    [showSlice, whyCex],
  );

  const allLanes = useMemo(() => shown.map((p) => byPath.get(p)).filter((s): s is Signal => !!s), [shown, byPath]);
  const lanes = useMemo(
    () => (showSlice && sliceOnly ? allLanes.filter((s) => isCausalSig(s.path)) : allLanes),
    [allLanes, showSlice, sliceOnly, isCausalSig],
  );
  const hidden = useMemo(() => (vcd ? vcd.signals.filter((s) => !shown.includes(s.path)) : []), [vcd, shown]);

  const eventsBySig = useMemo(() => {
    const map = new Map<string, CausalEvent[]>();
    if (!showSlice || !whyCex || !vcd) return map;
    for (const s of vcd.signals) {
      const evs = whyCex.events.filter((ev) => matchesCausalPath(s.path, ev.signal));
      if (evs.length) map.set(s.path, evs);
    }
    return map;
  }, [showSlice, whyCex, vcd]);

  const causalFramesBySig = useMemo(() => {
    const m = new Map<string, Set<number>>();
    for (const [k, evs] of eventsBySig.entries()) {
      m.set(k, new Set(evs.map((e) => e.frame)));
    }
    return m;
  }, [eventsBySig]);

  // Publish live signal values at `cursor` to Zustand so CodeMirror 6 (`SourceView`)
  // and `SchematicView` annotate RTL nets in real time while scrubbing!
  useEffect(() => {
    if (!vcd || !n) {
      setActiveTraceValues(null, {});
      return;
    }
    const vals: Record<string, string> = {};
    const t = cycles[Math.max(0, Math.min(n - 1, cursor))];
    // Cap eager broadcast to 120 signals so 500-signal traces stay < 0.3ms per scrub
    const limit = Math.min(vcd.signals.length, 120);
    for (let i = 0; i < limit; i++) {
      const s = vcd.signals[i];
      if (helper(s)) continue;
      const raw = vcd.valuesAt(s, t);
      if (raw === null) continue;
      const fmt = s.width === 1 ? `1'b${raw}` : `${s.width}'h${formatValue(raw, "hex")}`;
      vals[s.path] = fmt;
      if (!(s.name in vals)) vals[s.name] = fmt;
    }
    setActiveTraceValues(cursor, vals);
  }, [vcd, n, cycles, cursor, setActiveTraceValues]);

  // Expose deterministic standalone SVG capture for `gui_capture_view`
  useEffect(() => {
    if (!onCaptureSvgRef) return;
    onCaptureSvgRef.current = () => {
      if (!vcd || !n) return null;
      return renderTraceSvg({
        propName: prop.name,
        cover,
        depth,
        vcd,
        cycles,
        lanes: lanes.slice(0, 64),
        cursor,
        px: Math.max(24, Math.min(120, px)),
        radix,
        causalFramesBySig,
        annotations,
        dark: darkProp,
      });
    };
    return () => {
      onCaptureSvgRef.current = null;
    };
  }, [onCaptureSvgRef, vcd, n, prop.name, cover, depth, cycles, lanes, cursor, px, radix, causalFramesBySig, annotations, darkProp]);

  const scrollTo = (c: number) => {
    const el = scroller.current;
    if (!el) return;
    const left = NAME_W + c * px;
    if (left < el.scrollLeft + NAME_W) el.scrollLeft = left - NAME_W;
    else if (left + px > el.scrollLeft + el.clientWidth) el.scrollLeft = left + px - el.clientWidth;
  };

  const move = (c: number, byUser = false) => {
    if (byUser) detachFollowOnUserAction();
    const next = Math.max(0, Math.min(n - 1, c));
    setCursor(next);
    scrollTo(next);
  };

  useLayoutEffect(() => {
    if (!reveal.current || !scroller.current) return;
    reveal.current = false;
    scrollTo(cursor);
  });

  const cursorHandled = useRef(0);
  useEffect(() => {
    if (!cursorRequest || !vcd || shownFor.current !== vcd || cursorHandled.current === cursorRequest.n) return;
    cursorHandled.current = cursorRequest.n;
    setCursor(Math.max(0, Math.min(n - 1, cursorRequest.cycle)));
    reveal.current = true;
  }, [cursorRequest, vcd, n]);

  useEffect(() => {
    if (vcd) onView?.({ cursor, signals: shown });
  }, [vcd, cursor, shown, onView]);

  const jumpEdge = (dir: 1 | -1) => {
    if (!vcd || !n) return;
    const targetSig = (selectedSig && byPath.get(selectedSig)) || lanes.find((s) => s.name !== "clk") || lanes[0];
    if (!targetSig) return;
    const curVal = vcd.valuesAt(targetSig, cycles[cursor]);
    let c = cursor + dir;
    while (c >= 0 && c < n) {
      if (vcd.valuesAt(targetSig, cycles[c]) !== curVal) {
        move(c, true);
        return;
      }
      c += dir;
    }
  };

  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "n" || e.key === "N") {
      e.preventDefault();
      jumpEdge(1);
      return;
    }
    if (e.key === "p" || e.key === "P") {
      e.preventDefault();
      jumpEdge(-1);
      return;
    }
    const to =
      e.key === "ArrowLeft" ? cursor - 1 : e.key === "ArrowRight" ? cursor + 1 : e.key === "Home" ? 0 : e.key === "End" ? n - 1 : null;
    if (to === null) return;
    e.preventDefault();
    move(to, true);
  };

  const zoom = (d: 1 | -1) => {
    setPx((p) => {
      const idx = ZOOM.indexOf(p);
      if (idx >= 0) return ZOOM[Math.max(0, Math.min(ZOOM.length - 1, idx + d))];
      // When coming from Fit macro-zoom, snap into ZOOM presets
      return d > 0 ? ZOOM[0] : Math.max(1, Math.floor(p * 0.5));
    });
  };

  const fitTrace = () => {
    if (!n) return;
    const avail = Math.max(240, (scroller.current?.clientWidth ?? 900) - NAME_W - 16);
    setPx(Math.max(1, Math.min(160, avail / n)));
  };

  const exportVcd = () => {
    if (!text) return;
    const a = document.createElement("a");
    a.href = URL.createObjectURL(new Blob([text], { type: "text/plain" }));
    a.download = path.split("/").pop() || `${prop.name}.vcd`;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(a.href), 0);
  };

  // Track scroll & resize for viewport-clipped HiDPI <canvas> rendering and row virtualization
  const syncViewport = useCallback(() => {
    const el = scroller.current;
    if (!el) return;
    setScrollPos((prev) => {
      const vw = el.clientWidth || 1000;
      const vh = el.clientHeight || 500;
      if (prev.left === el.scrollLeft && prev.top === el.scrollTop && prev.vw === vw && prev.vh === vh) return prev;
      return { left: el.scrollLeft, top: el.scrollTop, vw, vh };
    });
  }, []);

  useEffect(() => {
    syncViewport();
    const el = scroller.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(syncViewport);
    ro.observe(el);
    return () => ro.disconnect();
  }, [syncViewport, vcd]);

  const at = (s: Signal) => (vcd && n ? vcd.valuesAt(s, cycles[cursor]) : null);
  const rdx = (s: Signal): Radix => radix[s.path] ?? "hex";
  const W = Math.max(1, Math.round(n * px));
  const H = HEAD + LANE * (lanes.length + 1);
  const virtualized = lanes.length > VIRT_THRESHOLD;
  const semanticDom = lanes.length * n <= SEMANTIC_CELL_CAP;

  const firstVisibleLane = virtualized ? Math.max(0, Math.floor((scrollPos.top - HEAD - LANE) / LANE) - 4) : 0;
  const lastVisibleLane = virtualized
    ? Math.min(lanes.length, Math.ceil((scrollPos.top + scrollPos.vh - HEAD) / LANE) + 4)
    : lanes.length;

  // HiDPI <canvas> painter: renders only visible cycles [c0..c1] and visible lanes [firstVisibleLane..lastVisibleLane],
  // using min/max bucket decimation when zoomed out (`px < 5`) so 500x5000 traces paint in < 2ms.
  useLayoutEffect(() => {
    const cvs = canvasRef.current;
    if (!cvs || !vcd || !n) return;
    let ctx: CanvasRenderingContext2D | null = null;
    try {
      ctx = cvs.getContext("2d");
    } catch {
      ctx = null;
    }
    if (!ctx) return;

    const dpr = typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1;
    const cw = Math.max(1, Math.min(W, Math.ceil(scrollPos.vw)));
    const ch = Math.max(1, Math.min(H, Math.ceil(scrollPos.vh + 52)));
    const offsetX = Math.max(0, Math.min(Math.max(0, W - cw), Math.floor(scrollPos.left)));
    const offsetY = Math.max(0, Math.min(Math.max(0, H - ch), Math.floor(scrollPos.top)));

    if (cvs.width !== Math.round(cw * dpr) || cvs.height !== Math.round(ch * dpr)) {
      cvs.width = Math.round(cw * dpr);
      cvs.height = Math.round(ch * dpr);
    }
    cvs.style.width = `${cw}px`;
    cvs.style.height = `${ch}px`;
    cvs.style.transform = `translate(${offsetX}px, ${offsetY}px)`;

    const pal = resolveSvgPalette(darkProp);
    const dark = pal.dark;
    const colSurface = pal.surface;
    const colHead = pal.chrome;
    const colGrid = pal.border;
    const colWave = pal.text;
    const colMuted = pal.muted;
    const colCausal = dark ? "rgba(224, 174, 74, 0.20)" : "rgba(251, 241, 217, 0.92)";
    const colStatus = cover ? pal.stCovered : pal.stCex;

    ctx.save();
    ctx.scale(dpr, dpr);
    ctx.clearRect(0, 0, cw, ch);
    ctx.fillStyle = colSurface;
    ctx.fillRect(0, 0, cw, ch);
    ctx.translate(-offsetX, -offsetY);

    const c0 = Math.max(0, Math.floor(offsetX / px) - 1);
    const c1 = Math.min(n, Math.ceil((offsetX + cw) / px) + 1);
    const cycleStep = Math.max(1, Math.ceil(36 / px));

    // Cursor column highlight
    ctx.fillStyle = cover ? "rgba(59, 91, 196, 0.09)" : "rgba(180, 71, 27, 0.09)";
    ctx.fillRect(cursor * px, HEAD, Math.max(2, px), H - HEAD);

    // Grid & ruler header
    ctx.fillStyle = colHead;
    ctx.fillRect(offsetX, 0, cw, HEAD);
    ctx.font = '11px "IBM Plex Mono", ui-monospace, monospace';
    ctx.textAlign = "center";
    ctx.strokeStyle = colGrid;
    ctx.lineWidth = 1;
    for (let c = c0; c < c1; c += cycleStep) {
      const gx = Math.round(c * px) + 0.5;
      ctx.beginPath();
      ctx.moveTo(gx, 0);
      ctx.lineTo(gx, offsetY + ch);
      ctx.stroke();
      ctx.fillStyle = c === depth ? colStatus : colMuted;
      ctx.fillText(String(c), c * px + Math.max(8, px / 2), 15);
    }

    // Property lane (row 0)
    if (offsetY < HEAD + LANE) {
      const py = HEAD;
      const failX = Math.max(0, Math.min(n - 1, depth)) * px;
      ctx.strokeStyle = colGrid;
      ctx.beginPath();
      ctx.moveTo(offsetX, py + LANE + 0.5);
      ctx.lineTo(offsetX + cw, py + LANE + 0.5);
      ctx.stroke();

      ctx.strokeStyle = colWave;
      ctx.lineWidth = 1.4;
      ctx.beginPath();
      ctx.moveTo(Math.max(offsetX, 0), py + LANE - 5);
      ctx.lineTo(Math.min(offsetX + cw, failX), py + LANE - 5);
      ctx.stroke();

      const mw = Math.max(6, px - 4);
      ctx.fillStyle = cover ? "rgba(59, 91, 196, 0.18)" : "rgba(180, 71, 27, 0.18)";
      ctx.strokeStyle = colStatus;
      ctx.fillRect(failX + 2, py + 4, mw, LANE - 8);
      ctx.strokeRect(failX + 2, py + 4, mw, LANE - 8);
      if (mw >= 28) {
        ctx.fillStyle = colStatus;
        ctx.fillText(cover ? "cover" : "fail", failX + px / 2, py + LANE / 2 + 4);
      }
    }

    // Signal lanes (virtualized vertically + bucket-decimated horizontally when px < 4)
    const lStart = Math.max(0, Math.floor((offsetY - HEAD - LANE) / LANE));
    const lEnd = Math.min(lanes.length, Math.ceil((offsetY + ch - HEAD) / LANE));
    const bucketCycles = px < 4 ? Math.max(1, Math.floor(2 / px)) : 1;

    for (let li = lStart; li < lEnd; li++) {
      const s = lanes[li];
      const y = HEAD + (li + 1) * LANE;
      const hi = y + 5;
      const lo = y + LANE - 5;
      const mid = y + LANE / 2;

      // Causal highlight cells
      const cset = causalFramesBySig.get(s.path);
      if (cset) {
        ctx.fillStyle = colCausal;
        for (const cf of cset) {
          if (cf >= c0 && cf < c1) {
            ctx.fillRect(cf * px + 1, y + 2, Math.max(2, px - 2), LANE - 4);
          }
        }
      }

      // Row separator
      ctx.strokeStyle = colGrid;
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(offsetX, y + LANE + 0.5);
      ctx.lineTo(offsetX + cw, y + LANE + 0.5);
      ctx.stroke();

      ctx.strokeStyle = colWave;
      ctx.lineWidth = 1.35;

      if (bucketCycles > 1) {
        // Min/Max zoom decimation envelope for 5,000+ cycle traces
        let prevVal: string | null = null;
        ctx.beginPath();
        for (let c = c0; c < c1; c += bucketCycles) {
          const endB = Math.min(c1, c + bucketCycles);
          const firstV = vcd.valuesAt(s, cycles[c]) ?? "x";
          let toggled = false;
          for (let k = c + 1; k < endB; k += Math.max(1, Math.floor(bucketCycles / 4))) {
            if ((vcd.valuesAt(s, cycles[k]) ?? "x") !== firstV) {
              toggled = true;
              break;
            }
          }
          const x0 = c * px;
          const x1 = endB * px;
          if (toggled) {
            ctx.fillStyle = dark ? "rgba(95, 179, 171, 0.35)" : "rgba(29, 95, 91, 0.25)";
            ctx.fillRect(x0, hi, Math.max(1.5, x1 - x0), lo - hi);
          } else if (s.width === 1) {
            const yv = firstV === "1" ? hi : firstV === "0" ? lo : mid;
            if (prevVal === null) ctx.moveTo(x0, yv);
            else ctx.lineTo(x0, yv);
            ctx.lineTo(x1, yv);
          } else {
            ctx.strokeRect(x0, hi, Math.max(1, x1 - x0), lo - hi);
          }
          prevVal = firstV;
        }
        if (s.width === 1) ctx.stroke();
      } else if (s.width === 1) {
        ctx.beginPath();
        for (let c = c0; c < c1; c++) {
          const v = vcd.valuesAt(s, cycles[c]) ?? "x";
          const yv = v === "1" ? hi : v === "0" ? lo : mid;
          const x0 = c * px;
          const x1 = (c + 1) * px;
          if (c === c0) ctx.moveTo(x0, yv);
          else ctx.lineTo(x0, yv);
          ctx.lineTo(x1, yv);
        }
        ctx.stroke();
      } else {
        const sRadix = radix[s.path] ?? "hex";
        let runStart = c0;
        let runVal = vcd.valuesAt(s, cycles[c0]) ?? "x".repeat(s.width);
        const drawBusRun = (cEnd: number, val: string) => {
          const x0 = runStart * px;
          const x1 = cEnd * px;
          const e = Math.min(3, (x1 - x0) / 4);
          ctx!.beginPath();
          ctx!.moveTo(x0, mid);
          ctx!.lineTo(x0 + e, hi);
          ctx!.lineTo(x1 - e, hi);
          ctx!.lineTo(x1, mid);
          ctx!.lineTo(x1 - e, lo);
          ctx!.lineTo(x0 + e, lo);
          ctx!.closePath();
          ctx!.fillStyle = /[xz]/.test(val) ? "rgba(180, 35, 24, 0.12)" : colSurface;
          ctx!.fill();
          ctx!.strokeStyle = colWave;
          ctx!.stroke();
          const txt = formatValue(val, sRadix);
          if (txt.length * 7 + 8 <= x1 - x0) {
            ctx!.fillStyle = colWave;
            ctx!.fillText(txt, (x0 + x1) / 2, mid + 4);
          }
        };
        for (let c = c0 + 1; c < c1; c++) {
          const v = vcd.valuesAt(s, cycles[c]) ?? "x".repeat(s.width);
          if (v !== runVal) {
            drawBusRun(c, runVal);
            runStart = c;
            runVal = v;
          }
        }
        drawBusRun(c1, runVal);
      }

      // AI Callout Badges (`gui_annotate_trace`)
      if (annotations?.length) {
        for (const ann of annotations) {
          if (matchesCausalPath(s.path, ann.signal) && ann.frame >= c0 && ann.frame < c1) {
            const ax = ann.frame * px + 2;
            const bw = Math.max(36, ann.label.length * 6.4 + 10);
            ctx.fillStyle = ann.tone === "danger" ? "#b42318" : ann.tone === "ok" ? "#2f7a4c" : "#1d5f5b";
            ctx.fillRect(ax, y + 2, bw, 14);
            ctx.fillStyle = "#ffffff";
            ctx.textAlign = "left";
            ctx.fillText(ann.label, ax + 4, y + 12);
            ctx.textAlign = "center";
          }
        }
      }
    }

    // Cursor vertical line
    ctx.strokeStyle = colStatus;
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    ctx.moveTo(cursor * px + 0.5, offsetY);
    ctx.lineTo(cursor * px + 0.5, offsetY + ch);
    ctx.stroke();

    ctx.restore();
  }, [vcd, n, W, H, px, cursor, depth, cover, cycles, lanes, radix, causalFramesBySig, annotations, scrollPos, darkProp]);

  return (
    <>
      {tabs && <div className="strip">{tabs}</div>}
      <div className="trace-bar">
        <span className="strip-summary">
          {!tabs && <><strong className="mono">{prop.name}</strong>{" · "}</>}
          {isWitness
            ? `${STATUS_LABEL[prop.status.status]} (witness) · ${prop.status.engine ?? "—"}`
            : `${STATUS_LABEL[prop.status.status]} · depth ${prop.status.depth ?? "?"} · ${prop.status.engine ?? "—"}`}
          {vcd ? ` · ${n} cycles` : ""}
        </span>
        {vcd && (
          <span className="trace-cursor mono" role="status" aria-label="Cursor">
            cycle {cursor}
          </span>
        )}
        <span className="spacer" />
        {vcd && onPinContext && (
          <button
            className="btn small"
            onClick={() => {
              const sample = lanes
                .slice(0, 4)
                .map((s) => `${s.name}=${formatValue(at(s), rdx(s))}`)
                .join(", ");
              onPinContext(`@${prop.name}@C${cursor}`, `Trace ${prop.name} cycle ${cursor}: ${sample}`);
            }}
            title="Attach this cycle's signal values as an @context pill for AI Ask / MCP"
          >
            ✦ @cycle
          </button>
        )}
        {onToggleWhyCex && (
          <button
            className={`btn ${whyCexOpen ? "active" : ""}`}
            onClick={onToggleWhyCex}
            disabled={!vcd || whyCexLoading}
            aria-pressed={!!whyCexOpen}
            title="Highlight active backward causal slice from the failing / covering cycle (why_cex)"
          >
            {whyCexLoading ? "Slicing…" : "Why CEX"}
          </button>
        )}
        <span className="zoom">
          <button
            className="tool"
            onClick={() => zoom(-1)}
            disabled={!vcd || px === ZOOM[0]}
            aria-label="Zoom out"
            title="Zoom out"
          >
            −
          </button>
          <span className="zoom-text mono" title="Pixels per cycle (N/P jumps next/prev edge)">
            {px >= 10 ? `${Math.round(px)}px` : `${px.toFixed(1)}px`}
          </span>
          <button
            className="tool"
            onClick={() => zoom(1)}
            disabled={!vcd || px === ZOOM[ZOOM.length - 1]}
            aria-label="Zoom in"
            title="Zoom in"
          >
            +
          </button>
          {n > 20 && (
            <button className="btn small" onClick={fitTrace} title="Fit all cycles into viewport">
              Fit
            </button>
          )}
        </span>
        <span className="add-wrap">
          <button className="btn" onClick={() => setAdding((a) => !a)} disabled={!vcd} aria-expanded={adding}>
            Add signal…
          </button>
          {adding && (
            <AddSignal
              signals={hidden}
              onAdd={(s) => {
                setShown((l) => [...l, s.path]);
                setAdding(false);
              }}
              onClose={() => setAdding(false)}
            />
          )}
        </span>
        <button className="btn" onClick={exportVcd} disabled={!text} title={`Save ${path}`}>
          Export VCD
        </button>
        {onPopOut && (
          <button className="btn small" onClick={onPopOut} title="Pop waveform out into a secondary window">
            ↗
          </button>
        )}
        {actions}
      </div>

      {((showSlice && whyCex) || Boolean(annotations?.length)) && (
        <div className="trace-overlay-row">
          {showSlice && whyCex && (
            <div className="causal-bar" role="region" aria-label="Causal root-cause slice">
              <span className="causal-summary">
                Causal slice: <strong>{whyCex.active_slice_signals}</strong>/{whyCex.static_cone_signals} COI · C{whyCex.failure_frame}
              </span>
              <label className="causal-filter small">
                <input type="checkbox" checked={sliceOnly} onChange={(e) => setSliceOnly(e.target.checked)} />
                Slice only
              </label>
              <div className="causal-events">
                {whyCex.events.map((ev, idx) => (
                  <button
                    key={idx}
                    className="causal-chip mono"
                    onClick={() => move(ev.frame, true)}
                    title={`${ev.kind} (${ev.reason}) — click to jump to cycle ${ev.frame}`}
                  >
                    @{ev.frame} {ev.signal.split(".").pop()}: {ev.prev_value ? `${ev.prev_value}→${ev.value}` : ev.value}
                  </button>
                ))}
              </div>
            </div>
          )}

          {!!annotations?.length && (
            <div className="trace-ann-bar" role="region" aria-label="AI waveform callouts">
              <span className="small strong">AI Callouts:</span>
              {annotations.map((ann, idx) => (
                <button
                  key={idx}
                  className={`ann-chip mono ann-${ann.tone ?? "accent"}`}
                  onClick={() => move(ann.frame, true)}
                  title={`Jump to cycle ${ann.frame} (${ann.signal})`}
                >
                  @{ann.frame} {ann.signal.split(".").pop()}: {ann.label}
                </button>
              ))}
            </div>
          )}
        </div>
      )}

      {!vcd ? (
        <p className="empty">{error ? `Could not read ${path}: ${error}` : `Loading ${path}…`}</p>
      ) : (
        <div
          className="trace"
          ref={scroller}
          tabIndex={0}
          onKeyDown={onKey}
          onScroll={syncViewport}
          onWheel={(e) => {
            if (e.ctrlKey || e.metaKey) {
              e.preventDefault();
              setPx((cur) => Math.max(1, Math.min(160, cur * (e.deltaY < 0 ? 1.25 : 0.8))));
            }
          }}
          aria-label={`Trace of ${prop.name} (Left / Right move the cursor, Home / End jump, N / P next/prev edge)`}
        >
          <div className="trace-names" style={{ width: NAME_W, height: H }}>
            <div className="trace-head trace-name">Signal</div>
            <div className={`trace-name prop-lane st-${word}`} title={prop.expr}>
              <span className="mono ellipsis">{prop.name}</span>
              <span className="trace-val mono">{cursor === depth ? (cover ? "hit" : "fail") : cover ? "" : "ok"}</span>
            </div>
            {virtualized && firstVisibleLane > 0 && <div style={{ height: firstVisibleLane * LANE }} />}
            {lanes.slice(firstVisibleLane, lastVisibleLane).map((s) => {
              const causal = isCausalSig(s.path);
              const isSel = selectedSig === s.path;
              return (
                <div
                  key={s.path}
                  className={`trace-name ${causal ? "causal-sig" : ""} ${isSel ? "selected-sig" : ""}`}
                  data-causal={causal ? "true" : undefined}
                  onClick={() => setSelectedSig(s.path)}
                  title={`${s.path}${s.width > 1 ? ` [${s.width - 1}:0]` : ""}${causal ? " (in active causal slice)" : ""}`}
                >
                  <span className="mono ellipsis">{s.path}</span>
                  {s.width > 1 && (
                    <select
                      className="radix"
                      value={rdx(s)}
                      onClick={(e) => e.stopPropagation()}
                      onChange={(e) => setRadix((r) => ({ ...r, [s.path]: e.target.value as Radix }))}
                      aria-label={`Radix of ${s.path}`}
                    >
                      <option value="hex">hex</option>
                      <option value="dec">dec</option>
                      <option value="bin">bin</option>
                    </select>
                  )}
                  <span className="trace-val mono">{formatValue(at(s), rdx(s))}</span>
                </div>
              );
            })}
            {virtualized && lastVisibleLane < lanes.length && (
              <div style={{ height: (lanes.length - lastVisibleLane) * LANE }} />
            )}
          </div>

          <div className="trace-canvas-stage" style={{ position: "relative", width: W, height: H, flexShrink: 0 }}>
            <canvas
              ref={canvasRef}
              className="trace-canvas"
              aria-hidden="true"
              style={{ position: "absolute", top: 0, left: 0, pointerEvents: "none" }}
            />
            <svg
              className="trace-lanes"
              width={W}
              height={H}
              role="img"
              aria-label={`Waveforms, ${n} cycles`}
              style={semanticDom ? { position: "relative", opacity: 0.001 } : { position: "relative" }}
              onClick={(e) => {
                const x = e.clientX - e.currentTarget.getBoundingClientRect().left;
                move(Math.floor(x / px), true);
                scroller.current?.focus();
              }}
            >
              {semanticDom && (
                <>
                  <PropLane y={HEAD} n={n} px={px} depth={depth} cover={cover} />
                  {lanes.map((s, i) => (
                    <Lane
                      key={s.path}
                      vcd={vcd}
                      s={s}
                      y={HEAD + LANE * (i + 1)}
                      cycles={cycles}
                      px={px}
                      radix={rdx(s)}
                      events={eventsBySig.get(s.path)}
                    />
                  ))}
                </>
              )}
            </svg>
          </div>
        </div>
      )}
    </>
  );
}

function xOf(t: number, cycles: number[], px: number): number {
  const n = cycles.length;
  if (!n) return 0;
  if (t <= cycles[0]) return 0;
  let i = n - 1;
  while (i > 0 && cycles[i] > t) i--;
  const period = (cycles[i + 1] ?? cycles[i] + (n > 1 ? cycles[n - 1] - cycles[n - 2] : 1)) - cycles[i];
  return Math.min(n * px, (i + (t - cycles[i]) / (period || 1)) * px);
}

function runs(vcd: Vcd, s: Signal, cycles: number[], px: number) {
  const end = cycles.length * px;
  const out: { x0: number; x1: number; v: string }[] = [];
  const first = vcd.valuesAt(s, cycles[0]);
  if (first !== null) out.push({ x0: 0, x1: end, v: first });
  for (const c of vcd.changes(s)) {
    const x = xOf(c.t, cycles, px);
    if (c.t <= cycles[0] || x >= end) continue;
    const last = out[out.length - 1];
    if (last && last.v === c.v) continue;
    if (last) last.x1 = x;
    out.push({ x0: x, x1: end, v: c.v });
  }
  return out;
}

function Lane({
  vcd,
  s,
  y,
  cycles,
  px,
  radix,
  events,
}: {
  vcd: Vcd;
  s: Signal;
  y: number;
  cycles: number[];
  px: number;
  radix: Radix;
  events?: CausalEvent[];
}) {
  const rs = runs(vcd, s, cycles, px);
  const hi = y + 5;
  const lo = y + LANE - 5;
  const mid = y + LANE / 2;
  const overlays = (events ?? []).filter((ev) => ev.frame >= 0 && ev.frame < cycles.length);
  if (s.width === 1) {
    let d = "";
    for (const r of rs) {
      const yv = r.v === "1" ? hi : r.v === "0" ? lo : mid;
      d += d ? ` L${r.x0},${yv}` : `M${r.x0},${yv}`;
      d += ` L${r.x1},${yv}`;
    }
    return (
      <g data-signal={s.path}>
        {overlays.map((ev, idx) => (
          <rect key={idx} className="trace-causal-cell" x={ev.frame * px + 1} y={y + 2} width={px - 2} height={LANE - 4} rx={2}>
            <title>{`@${ev.frame} ${s.path} = ${ev.value} (${ev.reason})`}</title>
          </rect>
        ))}
        <path className="trace-wave" d={d} />
      </g>
    );
  }
  return (
    <g data-signal={s.path}>
      {overlays.map((ev, idx) => (
        <rect key={idx} className="trace-causal-cell" x={ev.frame * px + 1} y={y + 2} width={px - 2} height={LANE - 4} rx={2}>
          <title>{`@${ev.frame} ${s.path} = ${ev.value} (${ev.reason})`}</title>
        </rect>
      ))}
      {rs.map((r, i) => {
        const e = Math.min(3, (r.x1 - r.x0) / 4);
        const text = formatValue(r.v, radix);
        const fits = text.length * 7 + 6 <= r.x1 - r.x0;
        const unknown = /[xz]/.test(r.v);
        return (
          <g key={i}>
            <polygon
              className={`trace-bus ${unknown ? "unknown" : ""}`}
              points={`${r.x0 + e},${hi} ${r.x1 - e},${hi} ${r.x1},${mid} ${r.x1 - e},${lo} ${r.x0 + e},${lo} ${r.x0},${mid}`}
            />
            {fits && (
              <text className="trace-busval" x={(r.x0 + r.x1) / 2} y={mid + 4}>
                {text}
              </text>
            )}
            <title>{`${s.path} = ${text}`}</title>
          </g>
        );
      })}
    </g>
  );
}

function PropLane({ y, n, px, depth, cover }: { y: number; n: number; px: number; depth: number; cover: boolean }) {
  const lo = y + LANE - 5;
  const x = Math.max(0, Math.min(n - 1, depth)) * px;
  const word = cover ? "covered" : "cex";
  return (
    <g data-signal="property">
      <path className="trace-wave" d={`M0,${lo} L${x},${lo}`} />
      <rect className={`trace-marker st-${word}`} x={x + 2} y={y + 4} width={px - 4} height={LANE - 8} rx={2} />
      <text className={`trace-markertext st-${word}`} x={x + px / 2} y={y + LANE / 2 + 4}>
        {cover ? "cover" : "fail"}
      </text>
    </g>
  );
}

function AddSignal({ signals, onAdd, onClose }: { signals: Signal[]; onAdd: (s: Signal) => void; onClose: () => void }) {
  const [q, setQ] = useState("");
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const down = (e: PointerEvent) => !box.current?.contains(e.target as Node) && !(e.target as Element).closest?.(".add-wrap") && onClose();
    window.addEventListener("pointerdown", down);
    return () => window.removeEventListener("pointerdown", down);
  }, [onClose]);
  const list = signals.filter((s) => s.path.toLowerCase().includes(q.trim().toLowerCase()));
  return (
    <div
      className="add-pop"
      ref={box}
      role="dialog"
      aria-label="Add signal"
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.preventDefault();
          e.stopPropagation();
          onClose();
        }
      }}
    >
      <input
        className="filter"
        autoFocus
        placeholder="Filter signals"
        aria-label="Filter signals"
        value={q}
        onChange={(e) => setQ(e.target.value)}
        onKeyDown={(e) => e.key === "Enter" && list[0] && onAdd(list[0])}
      />
      <div className="add-list" role="listbox" aria-label="Signals not shown">
        {list.map((s) => (
          <button key={s.path} role="option" aria-selected={false} className="add-item mono" onClick={() => onAdd(s)}>
            {s.path}
            {s.width > 1 && <span className="muted"> [{s.width - 1}:0]</span>}
          </button>
        ))}
        {!list.length && <p className="empty small">{signals.length ? "No signal matches." : "Every signal is shown."}</p>}
      </div>
    </div>
  );
}
