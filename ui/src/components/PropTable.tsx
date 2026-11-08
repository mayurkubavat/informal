import { useMemo, useRef, useState } from "react";
import type { Property } from "../backend";
import { bound, KIND_LETTER, sortValue, TASK, time, type SortKey } from "../model";
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
  const body = useRef<HTMLTableSectionElement>(null);
  const rows = useMemo(() => {
    if (!sort) return props;
    return [...props].sort((a, b) => {
      const x = sortValue(a, sort.key);
      const y = sortValue(b, sort.key);
      return (x < y ? -1 : x > y ? 1 : 0) * sort.dir;
    });
  }, [props, sort]);
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey)) {
      const i = rows.findIndex((p) => p.name === selected);
      if (i < 0 || !onContext) return;
      e.preventDefault();
      const r = body.current?.children[i]?.getBoundingClientRect();
      onContext(rows[i], (r?.left ?? 0) + 40, r?.bottom ?? 0);
      return;
    }
    if (e.key === "Enter") {
      const p = rows.find((r) => r.name === selected);
      if (p) {
        e.preventDefault();
        onOpen(p);
      }
      return;
    }
    if ((e.key !== "ArrowDown" && e.key !== "ArrowUp") || !rows.length) return;
    e.preventDefault();
    const i = rows.findIndex((p) => p.name === selected);
    const n = e.key === "ArrowDown" ? Math.min(i + 1, rows.length - 1) : Math.max(i - 1, 0);
    onSelect(rows[n].name);
    body.current?.children[n]?.scrollIntoView({ block: "nearest" });
  };
  return (
    <div className="table-wrap" tabIndex={0} onKeyDown={onKey} aria-label="Property table (arrow keys move the selection, Enter opens details)">
      <table className="grid">
        <thead>
          <tr>
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
        <tbody ref={body}>
          {rows.map((p) => (
            <tr
              key={p.name}
              className={`${p.name === selected ? "sel" : ""} ${p.enabled ? "" : "off"}`}
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
              <td className="mono">{p.name}</td>
              <td><Status p={p} /></td>
              <td className="mono dim">{p.status.engine ?? ""}</td>
              <td className="mono num dim" title={bound(p)}>{bound(p)}</td>
              <td className="mono num dim">{time(p)}</td>
              <td className="mono dim">{TASK}</td>
            </tr>
          ))}
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
