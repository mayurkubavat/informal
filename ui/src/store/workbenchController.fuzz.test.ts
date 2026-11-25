// Pillar 4 (Unit) — Deterministic Seeded Stateful Fuzzer for `workbenchController`.
// Exercises 250 randomized sequences of task filters, status filters, text queries,
// selection reveals, prove target resolution, verdict-hiding notices, and spatial
// navigation stack operations, enforcing UX-1..UX-8 invariants after every step.

import { describe, expect, it } from "vitest";
import type { Flavor, SessionView } from "../backend";
import type { StatusFilter, ViewProp, ViewStatus } from "../model";
import {
  activeFilterChips,
  findNewlyHiddenProved,
  guidedNextAction,
  popNavSnapshot,
  pushNavSnapshot,
  resolveProveAll,
  selectAndReveal,
  summarySegments,
  taskEnvSummary,
  visibleProps,
  type NavSnapshot,
  type TableFilterState,
} from "./workbenchController";

function makeRng(seed: number) {
  let s = seed >>> 0;
  return () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = Math.imul(s ^ (s >>> 15), 1 | s);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const TASKS = ["<embedded>", "Protocol", "CSR", "LowPower"] as const;
const STATUSES: ViewStatus[] = ["unprocessed", "proven", "cex", "covered", "unreachable", "undetermined"];
const FILTERS: StatusFilter[] = [null, "unprocessed", "proven", "cex", "covered", "assume"];
const QUERIES = ["", "axi", "csr", "q", "en", "nomatch_zzz"];

function makePopulation(rng: () => number, count: number): ViewProp[] {
  const out: ViewProp[] = [];
  for (let i = 0; i < count; i++) {
    const task = TASKS[Math.floor(rng() * TASKS.length)];
    const r = rng();
    const kind: ViewProp["kind"] = r < 0.2 ? "assume" : r < 0.4 ? "cover" : "assert";
    const status = kind === "assume" ? "unprocessed" : STATUSES[Math.floor(rng() * STATUSES.length)];
    const prefix = task === "<embedded>" ? "" : `${task}::`;
    const tag = i % 3 === 0 ? "axi" : i % 3 === 1 ? "csr" : "q";
    out.push({
      name: `${prefix}p_${tag}_${i}`,
      kind,
      task,
      expr: `${tag} == ${i}`,
      enabled: rng() > 0.12,
      vacuity: status === "proven" && rng() < 0.15 ? "vacuous" : undefined,
      status: {
        status,
        engine: status === "unprocessed" ? undefined : "informal-ic3",
        depth: status === "cex" || status === "covered" ? 4 + (i % 9) : undefined,
        ms: status === "unprocessed" ? undefined : 12 + i,
      },
    });
  }
  return out;
}

describe("workbenchController stateful seeded fuzzer (UX-1..UX-8 invariants)", () => {
  it("preserves all UX laws across 250 randomized transitions over multiple seeds", () => {
    for (const seed of [0x1001, 0x2026, 0x5eedcafe]) {
      const rng = makeRng(seed);
      let props = makePopulation(rng, 18);
      const availTasks: string[] = [...TASKS];
      let state: TableFilterState = { selected: null, statusFilter: null, taskFilter: null, filter: "" };
      let navStack: NavSnapshot[] = [];

      for (let step = 0; step < 250; step++) {
        const action = Math.floor(rng() * 8);
        const flavor: Flavor = rng() < 0.5 ? "jg" : "vcf";
        const currentTask = TASKS[Math.floor(rng() * TASKS.length)];

        if (action === 0) {
          state = { ...state, statusFilter: FILTERS[Math.floor(rng() * FILTERS.length)] };
        } else if (action === 1) {
          state = { ...state, taskFilter: rng() < 0.35 ? null : TASKS[Math.floor(rng() * TASKS.length)] };
        } else if (action === 2) {
          state = { ...state, filter: QUERIES[Math.floor(rng() * QUERIES.length)] };
        } else if (action === 3) {
          // UX-2: selectAndReveal MUST guarantee selected target becomes visible
          const target = props[Math.floor(rng() * props.length)];
          state = selectAndReveal(state, target.name, props, availTasks);
          const vis = visibleProps(props, state, availTasks);
          expect(vis.some((p) => p.name === target.name)).toBe(true);
          expect(state.selected).toBe(target.name);
        } else if (action === 4) {
          // Simulate a property settling and verify UX-5 hidden verdict notice honesty
          const before = props.map((p) => ({ ...p, status: { ...p.status } }));
          const idx = Math.floor(rng() * props.length);
          const nextStatus = STATUSES[1 + Math.floor(rng() * (STATUSES.length - 1))];
          props = props.map((p, i) =>
            i === idx && p.kind !== "assume"
              ? { ...p, status: { ...p.status, status: nextStatus, engine: "informal-bmc", ms: 25 } }
              : p,
          );
          const notice = findNewlyHiddenProved(before, props, state, availTasks);
          if (notice) {
            const visSet = new Set(visibleProps(props, state, availTasks).map((p) => p.name));
            for (const hiddenProp of notice.props) {
              expect(visSet.has(hiddenProp.name)).toBe(false);
            }
          }
        } else if (action === 5) {
          // UX-8: Nav stack cap & duplicate-top idempotency
          const snap: NavSnapshot = {
            center: rng() < 0.5 ? "table" : "trace",
            layoutPreset: rng() < 0.5 ? "triage" : "cex-debug",
            ...state,
          };
          navStack = pushNavSnapshot(navStack, snap);
          const afterDup = pushNavSnapshot(navStack, snap);
          expect(afterDup.length).toBe(navStack.length);
          expect(navStack.length).toBeLessThanOrEqual(16);
        } else if (action === 6) {
          const prevLen = navStack.length;
          const popped = popNavSnapshot(navStack);
          navStack = popped.rest;
          if (prevLen === 0) {
            expect(popped.snapshot).toBeNull();
          } else {
            expect(popped.snapshot).not.toBeNull();
            expect(navStack.length).toBe(prevLen - 1);
          }
        } else {
          // Toggle enablement of a random property
          const idx = Math.floor(rng() * props.length);
          props = props.map((p, i) => (i === idx ? { ...p, enabled: !p.enabled } : p));
        }

        // Universal per-step invariants
        const shownTask = state.taskFilter && availTasks.includes(state.taskFilter) ? state.taskFilter : null;
        const plan = resolveProveAll(props, currentTask, shownTask, flavor);
        const provableCount = props.filter((p) => p.enabled && p.kind !== "assume").length;
        if (provableCount === 0) {
          expect(plan.cmd).toBeNull();
          expect(plan.count).toBe(0);
        } else {
          expect(plan.cmd).not.toBeNull();
          expect(plan.count).toBeGreaterThan(0);
          expect(plan.count).toBeLessThanOrEqual(provableCount);
        }

        const chips = activeFilterChips(state, availTasks);
        const expectedChips =
          (state.statusFilter ? 1 : 0) + (shownTask ? 1 : 0) + (state.filter.trim() ? 1 : 0);
        expect(chips.length).toBe(expectedChips);

        const segs = summarySegments(props);
        const segTotal = segs.reduce((acc, s) => acc + s.count, 0);
        expect(segTotal).toBe(props.length);

        const mockSession = {
          current_task: currentTask,
          environment: null,
        } as unknown as SessionView;
        const env = taskEnvSummary(mockSession, props, shownTask);
        expect(["alive", "conflict", "unchecked", "none"]).toContain(env.state);
        expect(env.assumeCount).toBeGreaterThanOrEqual(0);

        const hint = guidedNextAction(props, plan, false, false);
        if (hint) {
          expect(["info", "warn", "ok"]).toContain(hint.tone);
          expect(hint.text.length).toBeGreaterThan(0);
        }
      }
    }
  });
});
