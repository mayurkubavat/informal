import { useState } from "react";
import { STATUS_LABEL, SUMMARY, TASK, type StatusFilter } from "../model";
import type { StatusWord } from "../backend";
import { Glyph } from "./Glyph";
import { ChevronIcon } from "./icons";

export function TaskTree({
  total,
  counts,
  assumes,
  filter,
  onFilter,
}: {
  total: number;
  counts: Partial<Record<StatusWord, number>>;
  assumes: number;
  filter: StatusFilter;
  onFilter: (f: StatusFilter) => void;
}) {
  const [open, setOpen] = useState(true);
  const groups: [StatusWord | "assume", string, number][] = [
    ...SUMMARY.map((s) => [s, STATUS_LABEL[s], counts[s] ?? 0] as [StatusWord, string, number]),
    ["unprocessed", "Unprocessed", counts.unprocessed ?? 0],
    ["assume", "Assumed", assumes],
  ];
  return (
    <div className="tree" role="tree">
      <div className={`tree-row ${filter === null ? "sel" : ""}`} role="treeitem" aria-expanded={open} onClick={() => onFilter(null)}>
        <button className={`twisty ${open ? "open" : ""}`} onClick={(e) => { e.stopPropagation(); setOpen((o) => !o); }} aria-label="Expand">
          <ChevronIcon />
        </button>
        <span className="mono">{TASK}</span>
        <span className="tree-count">{total}</span>
      </div>
      {open &&
        groups
          .filter(([, , n]) => n > 0)
          .map(([k, label, n]) => (
            <div
              key={k}
              role="treeitem"
              className={`tree-row child ${filter === k ? "sel" : ""}`}
              onClick={() => onFilter(filter === k ? null : k)}
            >
              <Glyph k={k} />
              <span>{label}</span>
              <span className="tree-count">{n}</span>
            </div>
          ))}
    </div>
  );
}
