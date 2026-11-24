// Pure, framework-free workbench UX controller enforcing dual-persona UX laws:
//   UX-1 Zero Silent No-Ops      (resolveProveAll)
//   UX-2 Selection Beats Filters (selectAndReveal)
//   UX-3 Filter Honesty          (activeFilterChips, summaryCounts)
//   UX-4 Assumption Visibility   (taskEnvSummary)
//   UX-5 Spatial Continuity      (findNewlyHiddenProved)
//   UX-6 Progressive Disclosure  (guidedNextAction)
//
// Tested directly by L1 (Rust->TS contract) and L2 (property-based invariants)
// in `workbenchController.test.ts` with zero DOM/React overhead.

import type { Flavor, SessionView } from "../backend";
import type { LayoutPreset } from "./workbenchStore";
import { isVacuousProof, matches, STATUS_LABEL, statusKey, SUMMARY, TASK, taskOf, type Center, type StatusFilter, type ViewProp, type ViewStatus } from "../model";

export interface TableFilterState {
  selected: string | null;
  statusFilter: StatusFilter;
  taskFilter: string | null;
  filter: string;
}

export interface ProveTargetPlan {
  /** Exact Tcl command to run; null if nothing can be proved. */
  cmd: string | null;
  /** Number of enabled assert/cover targets that will actually run. */
  count: number;
  /** Non-embedded task name if targeting a single named task, else null. */
  task: string | null;
  /** Human-readable tooltip explaining what will run. */
  title: string;
  /** Compact button label suffix (e.g. "Protocol (8)"), empty for default "<embedded>". */
  badge: string;
}

export interface FilterChip {
  kind: "status" | "task" | "query";
  label: string;
  ariaLabel: string;
  title: string;
}

export interface TaskEnvSummary {
  task: string;
  assumeCount: number;
  state: "alive" | "conflict" | "unchecked" | "none";
  detail: string;
  conflictNames: string[];
}

export interface GuidedHint {
  tone: "info" | "warn" | "ok";
  text: string;
  actionLabel?: string;
  actionKind?: "prove_all" | "open_cex" | "open_vacuous" | "clear_filters";
  actionProp?: string;
}

export interface NavSnapshot {
  center: Center;
  layoutPreset: LayoutPreset;
  selected: string | null;
  statusFilter: StatusFilter;
  taskFilter: string | null;
  filter: string;
}

export interface HiddenVerdictNotice {
  props: ViewProp[];
  filterLabel: string;
  text: string;
}

const isProvable = (p: ViewProp) => p.enabled && p.kind !== "assume";

const tclWord = (w: string) => (/^[A-Za-z0-9_.:/-]+$/.test(w) ? w : `{${w}}`);

/** Filter properties according to the current table filter state. */
export function visibleProps(
  allProps: readonly ViewProp[],
  state: Pick<TableFilterState, "filter" | "statusFilter" | "taskFilter">,
  availableTasks?: readonly string[],
): ViewProp[] {
  const shownTask =
    state.taskFilter && (!availableTasks || availableTasks.includes(state.taskFilter))
      ? state.taskFilter
      : null;
  return allProps.filter(
    (p) =>
      matches(p, state.filter) &&
      (!shownTask || taskOf(p) === shownTask) &&
      (!state.statusFilter ||
        statusKey(p) === state.statusFilter ||
        (state.statusFilter !== "assume" &&
          p.kind !== "assume" &&
          p.status.status === state.statusFilter)),
  );
}

/**
 * UX-1 (Zero Silent No-Ops): Resolve what clicking "Prove all" (F5) should run.
 *
 * Guarantees: whenever `count > 0`, `cmd` targets >= 1 enabled assert/cover
 * property (never silently sending bare `prove -all` against an empty
 * `<embedded>` task when properties live in `Protocol`, `CSR`, etc.).
 */
export function resolveProveAll(
  allProps: readonly ViewProp[],
  currentTask: string | undefined,
  shownTask: string | null,
  flavor: Flavor,
): ProveTargetPlan {
  const cur = currentTask || TASK;
  const provable = allProps.filter(isProvable);
  if (provable.length === 0) {
    return {
      cmd: null,
      count: 0,
      task: null,
      title: "No enabled assertions or covers to prove",
      badge: "",
    };
  }

  // 1. If the user filtered to a specific task with provable properties, prove that task.
  if (shownTask) {
    const inShown = provable.filter((p) => taskOf(p) === shownTask);
    if (inShown.length > 0) {
      const cmd =
        flavor === "jg"
          ? shownTask === cur
            ? "prove -all"
            : `prove -task ${tclWord(shownTask)}`
          : `check_fv -property {${inShown.map((p) => p.name).join(" ")}}`;
      const isNamed = shownTask !== TASK;
      return {
        cmd,
        count: inShown.length,
        task: isNamed ? shownTask : null,
        title: `${cmd} (${inShown.length} ${inShown.length === 1 ? "property" : "properties"} in ${shownTask}) (F5)`,
        badge: isNamed ? `${shownTask} (${inShown.length})` : "",
      };
    }
  }

  // 2. Otherwise, if the current task has provable properties, prove it.
  const inCur = provable.filter((p) => taskOf(p) === cur);
  if (inCur.length > 0) {
    const cmd = flavor === "jg" ? "prove -all" : "check_fv";
    const isNamed = cur !== TASK;
    return {
      cmd,
      count: inCur.length,
      task: isNamed ? cur : null,
      title: `${cmd} (${inCur.length} ${inCur.length === 1 ? "property" : "properties"} in ${cur}) (F5)`,
      badge: isNamed ? `${cur} (${inCur.length})` : "",
    };
  }

  // 3. Current task is empty (e.g. `<embedded>` after `check_protocol`/`check_csr`):
  //    target the non-empty task(s) automatically instead of doing a silent no-op!
  const nonEmptyTasks: string[] = [];
  for (const p of provable) {
    const t = taskOf(p);
    if (!nonEmptyTasks.includes(t)) nonEmptyTasks.push(t);
  }
  if (nonEmptyTasks.length === 1) {
    const t = nonEmptyTasks[0];
    const inTask = provable.filter((p) => taskOf(p) === t);
    const cmd =
      flavor === "jg"
        ? `prove -task ${tclWord(t)}`
        : `check_fv -property {${inTask.map((p) => p.name).join(" ")}}`;
    return {
      cmd,
      count: inTask.length,
      task: t,
      title: `${cmd} (${inTask.length} ${inTask.length === 1 ? "property" : "properties"} in ${t}) (F5)`,
      badge: `${t} (${inTask.length})`,
    };
  }

  const cmd =
    flavor === "jg"
      ? nonEmptyTasks.map((t) => `prove -task ${tclWord(t)}`).join("; ")
      : `check_fv -property {${provable.map((p) => p.name).join(" ")}}`;
  return {
    cmd,
    count: provable.length,
    task: null,
    title: `${cmd} (${provable.length} properties across ${nonEmptyTasks.join(", ")}) (F5)`,
    badge: `${nonEmptyTasks.length} tasks (${provable.length})`,
  };
}

/**
 * UX-2 (Selection Beats Filters): Select a property and ensure it is visible
 * in the central Property Table by clearing only the filters that hide it.
 *
 * Guarantees: if `propName` exists in `allProps`, `visibleProps(allProps, next)`
 * ALWAYS contains `propName`.
 */
export function selectAndReveal(
  state: TableFilterState,
  propName: string,
  allProps: readonly ViewProp[],
  availableTasks?: readonly string[],
): TableFilterState {
  const target = allProps.find((p) => p.name === propName);
  if (!target) {
    return { ...state, selected: propName };
  }
  let { statusFilter, taskFilter, filter } = state;

  // Check task filter
  const shownTask =
    taskFilter && (!availableTasks || availableTasks.includes(taskFilter))
      ? taskFilter
      : null;
  if (shownTask && taskOf(target) !== shownTask) {
    taskFilter = taskOf(target);
  }

  // Check status filter
  const statusMatches =
    !statusFilter ||
    statusKey(target) === statusFilter ||
    (statusFilter !== "assume" &&
      target.kind !== "assume" &&
      target.status.status === statusFilter);
  if (!statusMatches) {
    statusFilter = null;
  }

  // Check text query filter
  if (filter.trim() && !matches(target, filter)) {
    filter = "";
  }

  return {
    selected: propName,
    statusFilter,
    taskFilter,
    filter,
  };
}

export function statusFilterLabel(f: StatusFilter): string {
  if (!f) return "";
  if (f === "assume") return "Assumed";
  return STATUS_LABEL[f] ?? f;
}

/**
 * UX-3 (Filter Honesty): Produce visible, dismissible filter chips for every
 * active constraint narrowing the Property Table.
 */
export function activeFilterChips(
  state: Pick<TableFilterState, "statusFilter" | "taskFilter" | "filter">,
  availableTasks?: readonly string[],
): FilterChip[] {
  const chips: FilterChip[] = [];
  if (state.statusFilter) {
    const lbl = statusFilterLabel(state.statusFilter);
    chips.push({
      kind: "status",
      label: `Status: ${lbl}`,
      ariaLabel: "clear status filter",
      title: `Showing ${lbl} properties only — click to show all statuses`,
    });
  }
  const shownTask =
    state.taskFilter && (!availableTasks || availableTasks.includes(state.taskFilter))
      ? state.taskFilter
      : null;
  if (shownTask) {
    chips.push({
      kind: "task",
      label: `Task: ${shownTask}`,
      ariaLabel: "all tasks",
      title: `Showing task ${shownTask} only — click to show all tasks`,
    });
  }
  if (state.filter.trim()) {
    chips.push({
      kind: "query",
      label: `Match: "${state.filter.trim()}"`,
      ariaLabel: "clear text filter",
      title: "Click to clear text filter",
    });
  }
  return chips;
}

export interface SummarySegment {
  filter: StatusFilter;
  text: string;
  count: number;
  tone: string;
}

/** Clickable summary segments for the Property Table strip. */
export function summarySegments(
  allProps: readonly ViewProp[],
): SummarySegment[] {
  const counts: Partial<Record<ViewStatus, number>> = {};
  let assumes = 0;
  for (const p of allProps) {
    if (p.kind === "assume") assumes++;
    else counts[p.status.status] = (counts[p.status.status] ?? 0) + 1;
  }
  const out: SummarySegment[] = [];
  if (counts.running) {
    out.push({ filter: "running", text: `${counts.running} running`, count: counts.running, tone: "running" });
  }
  for (const s of SUMMARY) {
    const n = counts[s] ?? 0;
    if (n > 0) {
      out.push({
        filter: s,
        text: `${n} ${STATUS_LABEL[s].toLowerCase()}`,
        count: n,
        tone: s,
      });
    }
  }
  if (counts.unprocessed) {
    out.push({
      filter: "unprocessed",
      text: `${counts.unprocessed} unprocessed`,
      count: counts.unprocessed,
      tone: "unprocessed",
    });
  }
  if (assumes > 0) {
    out.push({
      filter: "assume",
      text: `${assumes} assumed`,
      count: assumes,
      tone: "assume",
    });
  }
  return out;
}

/**
 * UX-4 (Assumption & Soundness Visibility): Summarize active environment
 * assumptions and vacuity/conflict state for the focused task.
 */
export function taskEnvSummary(
  session: SessionView | null,
  allProps: readonly ViewProp[],
  focusedTask: string | null,
): TaskEnvSummary {
  const targetTask =
    focusedTask ??
    session?.environment?.task ??
    (allProps.length > 0 && !allProps.some((p) => taskOf(p) === (session?.current_task ?? TASK))
      ? taskOf(allProps[0])
      : session?.current_task ?? TASK);

  const enabledAssumes = allProps.filter(
    (p) => p.kind === "assume" && p.enabled && taskOf(p) === targetTask,
  );
  const env = session?.environment ?? null;
  if (env && env.task === targetTask) {
    const conflict = env.allowed !== null && env.allowed < env.asked;
    if (conflict) {
      return {
        task: targetTask,
        assumeCount: enabledAssumes.length,
        state: "conflict",
        conflictNames: env.conflict_set,
        detail: `Environment conflict in ${targetTask} at step ${env.allowed} (${env.conflict_set.join(", ") || "assumptions contradict reset"})`,
      };
    }
    return {
      task: targetTask,
      assumeCount: enabledAssumes.length,
      state: "alive",
      conflictNames: [],
      detail: `${targetTask}: ${enabledAssumes.length} active assumption${enabledAssumes.length === 1 ? "" : "s"} verified alive for ${env.asked} steps`,
    };
  }
  if (enabledAssumes.length > 0) {
    return {
      task: targetTask,
      assumeCount: enabledAssumes.length,
      state: "unchecked",
      conflictNames: [],
      detail: `${targetTask}: ${enabledAssumes.length} active assumption${enabledAssumes.length === 1 ? "" : "s"} (checked automatically on prove)`,
    };
  }
  return {
    task: targetTask,
    assumeCount: 0,
    state: "none",
    conflictNames: [],
    detail: `${targetTask}: no active environment assumptions`,
  };
}

/**
 * UX-5 (Spatial Continuity): Detect properties that just settled during a prove
 * run but are currently hidden by an active filter (e.g. proving an item while
 * filtered to "Unprocessed").
 */
export function findNewlyHiddenProved(
  before: readonly ViewProp[],
  after: readonly ViewProp[],
  state: Pick<TableFilterState, "filter" | "statusFilter" | "taskFilter">,
  availableTasks?: readonly string[],
): HiddenVerdictNotice | null {
  if (!state.statusFilter && !state.filter.trim() && !state.taskFilter) {
    return null;
  }
  const prevByName = new Map(before.map((p) => [p.name, p.status.status]));
  const visible = new Set(visibleProps(after, state, availableTasks).map((p) => p.name));
  const settledAndHidden = after.filter((p) => {
    if (p.kind === "assume") return false;
    const prev = prevByName.get(p.name);
    const cur = p.status.status;
    return prev !== cur && cur !== "unprocessed" && cur !== "running" && !visible.has(p.name);
  });
  if (settledAndHidden.length === 0) return null;

  const filterLabel = state.statusFilter
    ? statusFilterLabel(state.statusFilter)
    : state.taskFilter
      ? `Task ${state.taskFilter}`
      : `"${state.filter.trim()}"`;

  if (settledAndHidden.length === 1) {
    const p = settledAndHidden[0];
    const lbl = STATUS_LABEL[p.status.status];
    const eng = p.status.engine ? ` · ${p.status.engine}` : "";
    const ms = p.status.ms != null ? ` · ${(p.status.ms / 1000).toFixed(2)}s` : "";
    return {
      props: settledAndHidden,
      filterLabel,
      text: `${p.name} → ${lbl}${eng}${ms} (hidden by ${filterLabel} filter)`,
    };
  }
  return {
    props: settledAndHidden,
    filterLabel,
    text: `${settledAndHidden.length} newly settled properties are hidden by the ${filterLabel} filter`,
  };
}

/**
 * UX-6 (Dual-Track Progressive Disclosure): Non-intrusive next-action guidance
 * for novices that stays concise and actionable for experts.
 */
export function guidedNextAction(
  allProps: readonly ViewProp[],
  plan: ProveTargetPlan,
  running: boolean,
  busy: boolean,
): GuidedHint | null {
  if (running || busy || allProps.length === 0) return null;
  const assertsCovers = allProps.filter((p) => p.kind !== "assume");
  const unproc = assertsCovers.filter((p) => p.status.status === "unprocessed");
  const settled = assertsCovers.filter((p) => p.status.status !== "unprocessed" && p.status.status !== "running");
  const cex = assertsCovers.find((p) => p.status.status === "cex");

  if (cex) {
    return {
      tone: "warn",
      text: `Counterexample in ${cex.name}${cex.status.depth != null ? ` (depth ${cex.status.depth})` : ""} — inspect causal trace and RTL drivers.`,
      actionLabel: "Inspect CEX",
      actionKind: "open_cex",
      actionProp: cex.name,
    };
  }
  const vacuous = assertsCovers.find((p) => isVacuousProof(p));
  if (vacuous) {
    const vacCount = assertsCovers.filter((p) => isVacuousProof(p)).length;
    return {
      tone: "warn",
      text:
        vacCount === 1
          ? `Vacuous proof in ${vacuous.name} — antecedent never reached; inspect assumptions or driving logic.`
          : `${vacCount} proven assertions are vacuous (first: ${vacuous.name}) — antecedent never reached.`,
      actionLabel: "Inspect vacuous",
      actionKind: "open_vacuous",
      actionProp: vacuous.name,
    };
  }
  if (settled.length === 0 && unproc.length > 0 && plan.count > 0) {
    const assumes = allProps.filter((p) => p.kind === "assume" && p.enabled).length;
    const scope = plan.task ? ` in task ${plan.task}` : "";
    const assumeText = assumes > 0 ? ` (${assumes} active assumption${assumes === 1 ? "" : "s"})` : "";
    return {
      tone: "info",
      text: `${plan.count} ready to prove${scope}${assumeText}.`,
      actionLabel: plan.badge ? `Prove all · ${plan.badge}` : `Prove all (${plan.count})`,
      actionKind: "prove_all",
    };
  }
  return null;
}

const NAV_STACK_CAP = 16;

/**
 * UX-8 (Cross-Pane Spatial History): Push a snapshot when pivoting away from
 * the Property Table (or switching workspace preset) without duplicating identical tops.
 */
export function pushNavSnapshot(
  stack: readonly NavSnapshot[],
  snap: NavSnapshot,
): NavSnapshot[] {
  const top = stack[stack.length - 1];
  if (
    top &&
    top.center === snap.center &&
    top.layoutPreset === snap.layoutPreset &&
    top.selected === snap.selected &&
    top.statusFilter === snap.statusFilter &&
    top.taskFilter === snap.taskFilter &&
    top.filter === snap.filter
  ) {
    return [...stack];
  }
  const next = [...stack, snap];
  return next.length > NAV_STACK_CAP ? next.slice(next.length - NAV_STACK_CAP) : next;
}

export function popNavSnapshot(
  stack: readonly NavSnapshot[],
): { snapshot: NavSnapshot | null; rest: NavSnapshot[] } {
  if (stack.length === 0) return { snapshot: null, rest: [] };
  return {
    snapshot: stack[stack.length - 1],
    rest: stack.slice(0, -1),
  };
}

/**
 * UX-9 (Zero-Dead-End Toggle): Exact Tcl command to disable/enable a property
 * in either JasperGold (`<kind> -disable/-enable {<name>}`) or VC Formal (`fvdisable/fvenable {<name>}`).
 */
export function propertyToggleCmd(
  p: Pick<ViewProp, "name" | "kind" | "enabled">,
  flavor: Flavor,
): string {
  if (flavor === "vcf") {
    return `${p.enabled ? "fvdisable" : "fvenable"} {${p.name}}`;
  }
  return `${p.kind} ${p.enabled ? "-disable" : "-enable"} {${p.name}}`;
}

export function relativeSignalPath(signalPath: string, topModule?: string | null): string {
  if (topModule && signalPath.startsWith(`${topModule}.`)) {
    return signalPath.slice(topModule.length + 1);
  }
  return signalPath;
}

export function isSignalCut(
  signalPath: string,
  topModule: string | null | undefined,
  stopats: readonly string[] = [],
): boolean {
  const rel = relativeSignalPath(signalPath, topModule);
  return stopats.includes(rel) || stopats.includes(signalPath);
}

/**
 * UX-9 (Signal Cut-Point Tcl): Exact `stopat` (JG) / `snip_driver` (VCF) command
 * for cutting or restoring a signal's driver in the current task.
 */
export function signalStopatCmd(
  signalPath: string,
  flavor: Flavor,
  topModule?: string | null,
  clear = false,
): string {
  const rel = relativeSignalPath(signalPath, topModule);
  const verb = flavor === "vcf" ? "snip_driver" : "stopat";
  const arg = tclWord(rel);
  return clear ? `${verb} -clear ${arg}` : `${verb} ${arg}`;
}
