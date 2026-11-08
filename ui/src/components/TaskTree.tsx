import { useState } from "react";
import { groupOf, KIND_LETTER, STATUS_LABEL, SUMMARY, TASK, type StatusFilter, type ViewProp as Property, type ViewStatus } from "../model";
import { Glyph } from "./Glyph";
import { ChevronIcon } from "./icons";

type Group = ViewStatus | "assume";

// Leaves listed per group; a larger group ends in a "N more" row that
// filters the table to it (the table scrolls thousands of rows, a tree
// does not). The selected property is always listed.
export const LEAF_CAP = 200;

// <embedded> task > status groups > properties. Clicking a group filters the
// table; clicking a property selects it, double-click opens its Details.
export function TaskTree({
  total,
  counts,
  assumes,
  filter,
  onFilter,
  props = [],
  selected = null,
  onSelect,
  onOpen,
}: {
  total: number;
  counts: Partial<Record<ViewStatus, number>>;
  assumes: number;
  filter: StatusFilter;
  onFilter: (f: StatusFilter) => void;
  props?: Property[];
  selected?: string | null;
  onSelect?: (name: string) => void;
  onOpen?: (name: string) => void;
}) {
  const [open, setOpen] = useState(true);
  const [closed, setClosed] = useState<Set<Group>>(() => new Set());
  const groups: [Group, string, number][] = [
    ["running", STATUS_LABEL.running, counts.running ?? 0],
    ...SUMMARY.map((s) => [s, STATUS_LABEL[s], counts[s] ?? 0] as [ViewStatus, string, number]),
    ["unprocessed", "Unprocessed", counts.unprocessed ?? 0],
    ["assume", "Assumed", assumes],
  ];
  const toggle = (k: Group) =>
    setClosed((c) => {
      const n = new Set(c);
      if (!n.delete(k)) n.add(k);
      return n;
    });
  return (
    <div className="tree" role="tree" aria-label="Tasks">
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
          .map(([k, label, n]) => {
            const members = props.filter((p) => groupOf(p) === k);
            const isOpen = !closed.has(k);
            const shown = members.length > LEAF_CAP
              ? members.filter((p, i) => i < LEAF_CAP || p.name === selected)
              : members;
            const more = members.length - shown.length;
            return (
              <div key={k} role="group">
                <div
                  role="treeitem"
                  aria-expanded={members.length ? isOpen : undefined}
                  className={`tree-row child ${filter === k ? "sel" : ""}`}
                  onClick={() => onFilter(filter === k ? null : k)}
                >
                  {members.length > 0 && (
                    <button
                      className={`twisty group-twisty ${isOpen ? "open" : ""}`}
                      onClick={(e) => { e.stopPropagation(); toggle(k); }}
                      aria-label={`${isOpen ? "Collapse" : "Expand"} ${label}`}
                    >
                      <ChevronIcon />
                    </button>
                  )}
                  <Glyph k={k} />
                  <span>{label}</span>
                  <span className="tree-count">{n}</span>
                </div>
                {isOpen &&
                  shown.map((p) => (
                    <div
                      key={p.name}
                      role="treeitem"
                      aria-selected={p.name === selected}
                      className={`tree-row leaf ${p.name === selected ? "sel" : ""} ${p.enabled ? "" : "off"}`}
                      title={p.expr}
                      onClick={() => onSelect?.(p.name)}
                      onDoubleClick={() => onOpen?.(p.name)}
                    >
                      <span className="leaf-kind mono">{KIND_LETTER[p.kind]}</span>
                      <span className="mono ellipsis">{p.name}</span>
                    </div>
                  ))}
                {isOpen && more > 0 && (
                  <div
                    role="treeitem"
                    className="tree-row leaf more"
                    title="Show the whole group in the property table"
                    onClick={() => onFilter(k)}
                  >
                    <span className="leaf-kind" />
                    <span className="muted">{more} more…</span>
                  </div>
                )}
              </div>
            );
          })}
    </div>
  );
}
