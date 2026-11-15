import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { bound, KIND_LETTER, sortValue, TASK, time, type SortKey, type ViewProp as Property } from "../model";
import { Status } from "./Glyph";

export const COLS: { key: SortKey | null; label: string; cls?: string }[] = [
  { key: "kind", label: "", cls: "c-kind" },
  { key: "name", label: "Name" },
  { key: "status", label: "Status" },
  { key: "engine", label: "Engine" },
  { key: "bound", label: "Bound", cls: "num" },
  { key: "time", label: "Time", cls: "num" },
  { key: null, label: "Task" },
];

// Fixed row height (px, border included; styles.css .grid td) and the header
// height above the rows. Only the rows in view, plus OVERSCAN above and
// below, are in the DOM; spacer rows keep the scrollbar the full height.
export const ROW_H = 22;
export const HEAD_H = 23;
export const OVERSCAN = 8;
// Rows assumed visible before the pane has been measured (and in jsdom,
// which has no layout).
const FALLBACK_ROWS = 40;

// The slice of `n` rows to render for a pane scrolled to `top` showing
// `height` px of rows.
export function visibleRange(n: number, top: number, height: number): { start: number; end: number } {
  const shown = height > 0 ? Math.ceil(height / ROW_H) + 1 : FALLBACK_ROWS;
  const first = Math.floor(Math.max(0, top) / ROW_H);
  return { start: Math.max(0, first - OVERSCAN), end: Math.min(n, first + shown + OVERSCAN) };
}

// The scrollTop that brings row `i` fully into view with the least movement
// (unchanged when it already is).
export function scrollFor(i: number, top: number, height: number): number {
  const y = i * ROW_H;
  if (y < top) return y;
  if (height > 0 && y + ROW_H > top + height) return y + ROW_H - height;
  return top;
}

export function PropTable({
  props,
  total,
  selected,
  onSelect,
  onOpen,
  onContext,
}: {
  props: Property[];
  total: number;
  selected: string | null;
  onSelect: (n: string) => void;
  onOpen: (p: Property) => void;
  /** Right-click, the Menu key or Shift+F10 on a row; x/y are viewport coordinates. */
  onContext?: (p: Property, x: number, y: number) => void;
}) {
  const [sort, setSort] = useState<{ key: SortKey; dir: 1 | -1 } | null>(null);
  const wrap = useRef<HTMLDivElement>(null);
  const [view, setView] = useState({ top: 0, height: 0 });
  const rows = useMemo(() => {
    if (!sort) return props;
    return [...props].sort((a, b) => {
      const x = sortValue(a, sort.key);
      const y = sortValue(b, sort.key);
      return (x < y ? -1 : x > y ? 1 : 0) * sort.dir;
    });
  }, [props, sort]);
  const selIndex = useMemo(() => rows.findIndex((p) => p.name === selected), [rows, selected]);

  // Track the pane's scroll position and size.
  useLayoutEffect(() => {
    const el = wrap.current;
    if (!el) return;
    const measure = () => setView((v) => {
      const next = { top: el.scrollTop, height: Math.max(0, el.clientHeight - HEAD_H) };
      return v.top === next.top && v.height === next.height ? v : next;
    });
    measure();
    el.addEventListener("scroll", measure, { passive: true });
    const ro = typeof ResizeObserver !== "undefined" ? new ResizeObserver(measure) : null;
    ro?.observe(el);
    return () => {
      el.removeEventListener("scroll", measure);
      ro?.disconnect();
    };
  }, []);

  // Keep the selected row in view when the selection moves (keys, the task
  // tree) or the rows change under it.
  const scrollTo = (i: number) => {
    const el = wrap.current;
    if (!el || i < 0) return;
    const height = Math.max(0, el.clientHeight - HEAD_H);
    const top = scrollFor(i, el.scrollTop, height);
    if (top !== el.scrollTop) {
      el.scrollTop = top;
      setView({ top: el.scrollTop, height });
    }
  };
  useEffect(() => scrollTo(selIndex), [selIndex]);

  const { start, end } = visibleRange(rows.length, view.top, view.height);
  const page = Math.max(1, Math.floor((view.height || FALLBACK_ROWS * ROW_H) / ROW_H) - 1);

  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey)) {
      if (selIndex < 0 || !onContext) return;
      e.preventDefault();
      scrollTo(selIndex);
      const r = wrap.current?.querySelector(`tr[data-index="${selIndex}"]`)?.getBoundingClientRect();
      onContext(rows[selIndex], (r?.left ?? 0) + 40, r?.bottom ?? 0);
      return;
    }
    if (e.key === "Enter") {
      if (selIndex >= 0) {
        e.preventDefault();
        onOpen(rows[selIndex]);
      }
      return;
    }
    if (!rows.length) return;
    const last = rows.length - 1;
    const i = selIndex;
    const to: Record<string, number> = {
      ArrowDown: Math.min(i + 1, last),
      ArrowUp: Math.max(i - 1, 0),
      PageDown: Math.min(Math.max(i, 0) + page, last),
      PageUp: Math.max(i - page, 0),
      Home: 0,
      End: last,
    };
    if (!(e.key in to)) return;
    e.preventDefault();
    onSelect(rows[to[e.key]].name);
  };

  const row = (p: Property, i: number) => (
    <tr
      key={p.name}
      data-index={i}
      aria-rowindex={i + 2}
      className={`${p.name === selected ? "sel" : ""} ${p.enabled ? "" : "off"} ${p.provisional ? "prov" : ""}`}
      aria-selected={p.name === selected}
      onClick={() => onSelect(p.name)}
      onDoubleClick={() => onOpen(p)}
      onContextMenu={(e) => {
        if (!onContext) return;
        e.preventDefault();
        onContext(p, e.clientX, e.clientY);
      }}
    >
      <td className="c-kind mono" title={p.kind}>{KIND_LETTER[p.kind]}</td>
      <td className="mono" title={p.provisional ? "Added by the running script; details follow when it returns" : undefined}>
        {p.name}
        {p.ai && <span className="tag" title="Added through the AI interface">AI</span>}
      </td>
      <td>
        <Status p={p} />
        {!!p.under_ai?.length && (
          <span className="tag" title={`Found with AI-added changes in the model: ${p.under_ai.join(", ")}`} aria-label={`under AI-added ${p.under_ai.join(", ")}`}>
            under AI
          </span>
        )}
      </td>
      <td className="mono dim">{p.status.engine ?? ""}</td>
      <td className="mono num dim" title={bound(p)}>{bound(p)}</td>
      <td className="mono num dim">{time(p)}</td>
      <td className="mono dim">{TASK}</td>
    </tr>
  );

  return (
    <div
      className="table-wrap"
      ref={wrap}
      tabIndex={0}
      onKeyDown={onKey}
      aria-label="Property table (arrow keys, Page Up/Down and Home/End move the selection, Enter opens details)"
    >
      <table className="grid" aria-rowcount={rows.length + 1}>
        <thead>
          <tr aria-rowindex={1}>
            {COLS.map((c) => (
              <th
                key={c.label || "kind"}
                className={c.cls}
                aria-sort={c.key && sort?.key === c.key ? (sort.dir === 1 ? "ascending" : "descending") : undefined}
                onClick={() =>
                  c.key &&
                  setSort((s) => (s?.key !== c.key ? { key: c.key!, dir: 1 } : s.dir === 1 ? { key: c.key!, dir: -1 } : null))
                }
                title={c.key === "kind" ? "Type: A assert, C cover, M assume" : undefined}
              >
                {c.label}
                {c.key && sort?.key === c.key && <span className="sort">{sort.dir === 1 ? "▴" : "▾"}</span>}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {start > 0 && <tr className="spacer" aria-hidden="true" style={{ height: start * ROW_H }}><td colSpan={COLS.length} /></tr>}
          {rows.slice(start, end).map((p, k) => row(p, start + k))}
          {end < rows.length && (
            <tr className="spacer" aria-hidden="true" style={{ height: (rows.length - end) * ROW_H }}><td colSpan={COLS.length} /></tr>
          )}
          {rows.length === 0 && (
            <tr className="empty-row">
              <td colSpan={COLS.length} className="empty">
                {total ? "No properties match the current filter." : "No properties. Source a JasperGold or VC Formal script, or type commands in the console."}
              </td>
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
}
