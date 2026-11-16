import { useState } from "react";
import { groupOf, KIND_LETTER, STATUS_LABEL, SUMMARY, TASK, taskOf, type StatusFilter, type ViewProp as Property, type ViewStatus } from "../model";
import { Glyph } from "./Glyph";
import { ChevronIcon } from "./icons";

type Group = ViewStatus | "assume";

// Leaves listed per group; a larger group ends in a "N more" row that
// filters the table to it (the table scrolls thousands of rows, a tree
// does not). The selected property is always listed.
export const LEAF_CAP = 200;

// Task > status groups > properties. Clicking a group filters the table;
// clicking a property selects it, double-click opens its Details. With
// several tasks (decision D20) each task is a node of its own: clicking it
// shows only its properties, and its groups filter within it. The current
// task (where commands without -task go) is marked.
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
  tasks,
  current = TASK,
  task = null,
  onTask,
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
  /** Task names, "<embedded>" first; one task when absent. */
  tasks?: string[];
  current?: string;
  /** The task the table is filtered to; null for all. */
  task?: string | null;
  onTask?: (t: string | null) => void;
}) {
  const [closedTasks, setClosedTasks] = useState<Set<string>>(() => new Set());
  const [closed, setClosed] = useState<Set<string>>(() => new Set());
  const many = (tasks?.length ?? 1) > 1;
  const toggle = <T,>(set: (f: (s: Set<T>) => Set<T>) => void, k: T) =>
    set((c) => {
      const n = new Set(c);
      if (!n.delete(k)) n.add(k);
      return n;
    });
  const groupsOf = (t: string, members: Property[], groupCounts: Partial<Record<ViewStatus, number>>, assumed: number) => {
    const groups: [Group, string, number][] = [
      ["running", STATUS_LABEL.running, groupCounts.running ?? 0],
      ...SUMMARY.map((s) => [s, STATUS_LABEL[s], groupCounts[s] ?? 0] as [ViewStatus, string, number]),
      ["unprocessed", "Unprocessed", groupCounts.unprocessed ?? 0],
      ["assume", "Assumed", assumed],
    ];
    const here = !many || task === t;
    return groups
      .filter(([, , n]) => n > 0)
      .map(([k, label, n]) => {
        const inGroup = members.filter((p) => groupOf(p) === k);
        const key = `${t}\u0000${k}`;
        const isOpen = !closed.has(key);
        const shown = inGroup.length > LEAF_CAP ? inGroup.filter((p, i) => i < LEAF_CAP || p.name === selected) : inGroup;
        const more = inGroup.length - shown.length;
        const pick = (f: StatusFilter) => {
          if (many) onTask?.(t);
          onFilter(f);
        };
        return (
          <div key={k} role="group">
            <div
              role="treeitem"
              aria-expanded={inGroup.length ? isOpen : undefined}
              className={`tree-row child ${many ? "in-task" : ""} ${here && filter === k ? "sel" : ""}`}
              onClick={() => pick(here && filter === k ? null : k)}
            >
              {inGroup.length > 0 && (
                <button
                  className={`twisty group-twisty ${isOpen ? "open" : ""}`}
                  onClick={(e) => { e.stopPropagation(); toggle(setClosed, key); }}
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
                  className={`tree-row leaf ${many ? "in-task" : ""} ${p.name === selected ? "sel" : ""} ${p.enabled ? "" : "off"}`}
                  title={p.expr}
                  onClick={() => onSelect?.(p.name)}
                  onDoubleClick={() => onOpen?.(p.name)}
                >
                  <span className="leaf-kind mono">{KIND_LETTER[p.kind]}</span>
                  <span className="mono ellipsis">{p.name}</span>
                </div>
              ))}
            {isOpen && more > 0 && (
              <div role="treeitem" className={`tree-row leaf more ${many ? "in-task" : ""}`} title="Show the whole group in the property table" onClick={() => pick(k)}>
                <span className="leaf-kind" />
                <span className="muted">{more} more…</span>
              </div>
            )}
          </div>
        );
      });
  };
  const names = many ? tasks! : [tasks?.[0] ?? TASK];
  return (
    <div className="tree" role="tree" aria-label="Tasks">
      {names.map((t) => {
        const members = many ? props.filter((p) => taskOf(p) === t) : props;
        let groupCounts = counts;
        let assumed = assumes;
        if (many) {
          groupCounts = {};
          for (const p of members) if (p.kind !== "assume") groupCounts[p.status.status] = (groupCounts[p.status.status] ?? 0) + 1;
          assumed = members.filter((p) => p.kind === "assume").length;
        }
        const open = !closedTasks.has(t);
        const sel = many ? task === t && filter === null : filter === null;
        return (
          <div key={t} role="group">
            <div
              className={`tree-row ${sel ? "sel" : ""}`}
              role="treeitem"
              aria-expanded={open}
              title={many && t === current ? "Current task: commands without -task apply to it" : undefined}
              onClick={() => {
                if (many) onTask?.(task === t && filter === null ? null : t);
                onFilter(null);
              }}
            >
              <button className={`twisty ${open ? "open" : ""}`} onClick={(e) => { e.stopPropagation(); toggle(setClosedTasks, t); }} aria-label="Expand">
                <ChevronIcon />
              </button>
              <span className="mono">{t}</span>
              {many && t === current && <span className="tag" aria-label="current task">current</span>}
              <span className="tree-count">{many ? members.length : total}</span>
            </div>
            {open && groupsOf(t, members, groupCounts, assumed)}
          </div>
        );
      })}
    </div>
  );
}
