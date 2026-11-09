// The trace tab: a CEX or cover witness as a waveform. A fixed signal list
// on the left (name, radix for buses, value at the cursor), and one 26px
// lane per signal drawn as inline SVG: 1-bit signals as a step waveform,
// buses as boxed values, and the property's own lane with its fail / cover
// marker at the depth cycle. Strokes stay neutral; only the marker, the
// cursor and the depth cycle number carry the status colour.
//
// TODO(D10): FST traces with streamed windows for large waveforms.

import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { STATUS_LABEL, type ViewProp } from "../model";
import { cycleTimes, formatValue, parseVcd, type Radix, type Signal, type Vcd } from "../vcd";

export const ZOOM = [40, 60, 80, 120, 160];
export const ZOOM_DEFAULT = 60;
const LANE = 26;
const HEAD = 22;

// Signals under the `informal` scope are FPV helpers (cycle counter, $past
// registers): listed under Add signal…, not shown by default.
const helper = (s: Signal) => s.path === "informal" || s.path.startsWith("informal.");

export function TraceView({
  tabs,
  prop,
  path,
  text,
  error,
  addRequest,
  onAddResult,
  actions,
}: {
  /** The centre tab strip; the trace's own controls follow it. */
  tabs: React.ReactNode;
  prop: ViewProp;
  path: string;
  text: string | null;
  error?: string | null;
  /** A signal to add by hierarchical path (the Design tab's double-click); `n` makes each request new. */
  addRequest?: { path: string; n: number } | null;
  /** How a request ended: added, already shown, or not in this trace. */
  onAddResult?: (path: string, result: "added" | "shown" | "missing") => void;
  /** Extra buttons at the end of the trace bar. */
  actions?: React.ReactNode;
}) {
  const vcd = useMemo(() => (text ? parseVcd(text) : null), [text]);
  const cycles = useMemo(() => (vcd ? cycleTimes(vcd) : []), [vcd]);
  const n = cycles.length;
  const depth = prop.status.depth ?? n - 1;
  const cover = prop.status.status === "covered";
  const word = cover ? "covered" : "cex";

  const [px, setPx] = useState(ZOOM_DEFAULT);
  const [cursor, setCursor] = useState(0);
  const [shown, setShown] = useState<string[]>([]);
  const [radix, setRadix] = useState<Record<string, Radix>>({});
  const [adding, setAdding] = useState(false);
  const scroller = useRef<HTMLDivElement>(null);

  // A new trace (another property, or a re-prove): default signals, cursor
  // on the failing / covering cycle.
  const shownRef = useRef<string[]>([]);
  shownRef.current = shown;
  const shownFor = useRef<Vcd | null>(null);
  useEffect(() => {
    if (!vcd) return;
    shownFor.current = vcd;
    shownRef.current = vcd.signals.filter((s) => !helper(s)).map((s) => s.path);
    setShown(shownRef.current);
    setCursor(Math.max(0, Math.min(n - 1, depth)));
    setAdding(false);
    reveal.current = true;
  }, [vcd, n, depth]);

  const byPath = useMemo(() => new Map(vcd?.signals.map((s) => [s.path, s]) ?? []), [vcd]);
  // Add a signal asked for from outside (once per request, once the VCD is in).
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
  const lanes = shown.map((p) => byPath.get(p)).filter((s): s is Signal => !!s);
  const hidden = vcd ? vcd.signals.filter((s) => !shown.includes(s.path)) : [];

  // Keep the cursor's cycle in view (the signal list covers the first 200px).
  const scrollTo = (c: number) => {
    const el = scroller.current;
    if (!el) return;
    const left = 200 + c * px;
    if (left < el.scrollLeft + 200) el.scrollLeft = left - 200;
    else if (left + px > el.scrollLeft + el.clientWidth) el.scrollLeft = left + px - el.clientWidth;
  };
  const move = (c: number) => {
    const next = Math.max(0, Math.min(n - 1, c));
    setCursor(next);
    scrollTo(next);
  };
  // A new trace opens with its failing / covering cycle in view.
  const reveal = useRef(false);
  useLayoutEffect(() => {
    if (!reveal.current || !scroller.current) return;
    reveal.current = false;
    scrollTo(cursor);
  });
  const onKey = (e: React.KeyboardEvent) => {
    const to =
      e.key === "ArrowLeft" ? cursor - 1 : e.key === "ArrowRight" ? cursor + 1 : e.key === "Home" ? 0 : e.key === "End" ? n - 1 : null;
    if (to === null) return;
    e.preventDefault();
    move(to);
  };
  const zoom = (d: 1 | -1) => setPx((p) => ZOOM[Math.max(0, Math.min(ZOOM.length - 1, ZOOM.indexOf(p) + d))]);
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

  const at = (s: Signal) => (vcd && n ? vcd.valuesAt(s, cycles[cursor]) : null);
  const rdx = (s: Signal): Radix => radix[s.path] ?? "hex";
  const W = n * px;

  return (
    <>
      <div className="strip">{tabs}</div>
      {/* The trace's own bar: on the strip it would not fit narrow windows. */}
      <div className="trace-bar">
        <span className="strip-summary">
          {STATUS_LABEL[prop.status.status]} · depth {prop.status.depth ?? "?"} · {prop.status.engine ?? "—"}
          {vcd ? ` · ${n} cycles` : ""}
        </span>
        {vcd && (
          <span className="trace-cursor mono" role="status" aria-label="Cursor">
            cycle {cursor}
          </span>
        )}
        <span className="spacer" />
        <span className="zoom">
          <button className="tool" onClick={() => zoom(-1)} disabled={!vcd || px === ZOOM[0]} aria-label="Zoom out" title="Zoom out">
            −
          </button>
          <span className="zoom-text mono" title="Pixels per cycle">{px}px</span>
          <button className="tool" onClick={() => zoom(1)} disabled={!vcd || px === ZOOM[ZOOM.length - 1]} aria-label="Zoom in" title="Zoom in">
            +
          </button>
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
        {actions}
      </div>

      {!vcd ? (
        <p className="empty">{error ? `Could not read ${path}: ${error}` : `Loading ${path}…`}</p>
      ) : (
        <div
          className="trace"
          ref={scroller}
          tabIndex={0}
          onKeyDown={onKey}
          aria-label={`Trace of ${prop.name} (Left / Right move the cursor, Home / End jump)`}
        >
          <div className="trace-names" style={{ width: 200 }}>
            <div className="trace-head trace-name">Signal</div>
            <div className={`trace-name prop-lane st-${word}`} title={prop.expr}>
              <span className="mono ellipsis">{prop.name}</span>
              <span className="trace-val mono">{cursor === depth ? (cover ? "hit" : "fail") : cover ? "" : "ok"}</span>
            </div>
            {lanes.map((s) => (
              <div key={s.path} className="trace-name" title={`${s.path}${s.width > 1 ? ` [${s.width - 1}:0]` : ""}`}>
                <span className="mono ellipsis">{s.path}</span>
                {s.width > 1 && (
                  <select
                    className="radix"
                    value={rdx(s)}
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
            ))}
          </div>
          <svg
            className="trace-lanes"
            width={W}
            height={HEAD + LANE * (lanes.length + 1)}
            role="img"
            aria-label={`Waveforms, ${n} cycles`}
            onClick={(e) => {
              const x = e.clientX - e.currentTarget.getBoundingClientRect().left;
              move(Math.floor(x / px));
              scroller.current?.focus();
            }}
          >
            {/* cycle header and grid */}
            <rect className="trace-headbg" x={0} y={0} width={W} height={HEAD} />
            <rect className="trace-cursorcol" x={cursor * px} y={HEAD} width={px} height={LANE * (lanes.length + 1)} />
            {cycles.map((_, c) => (
              <g key={c}>
                <line className="trace-grid" x1={c * px} x2={c * px} y1={0} y2={HEAD + LANE * (lanes.length + 1)} />
                <text className={`trace-cycle ${c === depth ? `st-${word}` : ""}`} x={c * px + px / 2} y={15}>
                  {c}
                </text>
              </g>
            ))}
            <PropLane y={HEAD} n={n} px={px} depth={depth} cover={cover} />
            {lanes.map((s, i) => (
              <Lane key={s.path} vcd={vcd} s={s} y={HEAD + LANE * (i + 1)} cycles={cycles} px={px} radix={rdx(s)} />
            ))}
            {/* cursor: the cycle's sample point (its start), its column shaded */}
            <line className={`trace-cursorline st-${word}`} x1={cursor * px + 0.5} x2={cursor * px + 0.5} y1={0} y2={HEAD + LANE * (lanes.length + 1)} />
          </svg>
        </div>
      )}
    </>
  );
}

// Time to x: cycle i spans [i*px, (i+1)*px), linear inside a cycle.
function xOf(t: number, cycles: number[], px: number): number {
  const n = cycles.length;
  if (!n) return 0;
  if (t <= cycles[0]) return 0;
  let i = n - 1;
  while (i > 0 && cycles[i] > t) i--;
  const period = (cycles[i + 1] ?? cycles[i] + (n > 1 ? cycles[n - 1] - cycles[n - 2] : 1)) - cycles[i];
  return Math.min(n * px, (i + (t - cycles[i]) / (period || 1)) * px);
}

// Value runs over the trace's span, with x extents.
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

function Lane({ vcd, s, y, cycles, px, radix }: { vcd: Vcd; s: Signal; y: number; cycles: number[]; px: number; radix: Radix }) {
  const rs = runs(vcd, s, cycles, px);
  const hi = y + 5;
  const lo = y + LANE - 5;
  const mid = y + LANE / 2;
  if (s.width === 1) {
    let d = "";
    for (const r of rs) {
      const yv = r.v === "1" ? hi : r.v === "0" ? lo : mid;
      d += d ? ` L${r.x0},${yv}` : `M${r.x0},${yv}`;
      d += ` L${r.x1},${yv}`;
    }
    return (
      <g data-signal={s.path}>
        <line className="trace-sep" x1={0} x2={cycles.length * px} y1={y + LANE} y2={y + LANE} />
        <path className="trace-wave" d={d} />
      </g>
    );
  }
  return (
    <g data-signal={s.path}>
      <line className="trace-sep" x1={0} x2={cycles.length * px} y1={y + LANE} y2={y + LANE} />
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

// The property's lane: a flat line while it holds, the status-coloured
// marker on the depth cycle.
function PropLane({ y, n, px, depth, cover }: { y: number; n: number; px: number; depth: number; cover: boolean }) {
  const lo = y + LANE - 5;
  const x = Math.max(0, Math.min(n - 1, depth)) * px;
  const word = cover ? "covered" : "cex";
  return (
    <g data-signal="property">
      <line className="trace-sep" x1={0} x2={n * px} y1={y + LANE} y2={y + LANE} />
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
