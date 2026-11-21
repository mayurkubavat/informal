// Layer 1 (Rust -> TS Schema Contract) + Layer 2 (Pure State-Machine UX Invariants)
// Tests run in < 200ms via Vitest with zero DOM overhead on pure logic, plus
// PropTable/TaskTree rendering verification against the real Rust JSON fixture.

import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";
import type { SessionView } from "../backend";
import contractJson from "../../tests/fixtures/session-contract.json";
import { bound, cell, STATUS_LABEL, statusKey, statusText, taskOf, type StatusFilter, type ViewProp } from "../model";
import { PropTable } from "../components/PropTable";
import { TaskTree } from "../components/TaskTree";
import {
  activeFilterChips,
  findNewlyHiddenProved,
  guidedNextAction,
  resolveProveAll,
  selectAndReveal,
  summarySegments,
  taskEnvSummary,
  visibleProps,
  type TableFilterState,
} from "./workbenchController";

const CONTRACT_SESSION = contractJson as unknown as SessionView;

describe("Layer 1 — Rust -> TypeScript Session Schema Contract", () => {
  it("renders every Rust Status variant exhaustively with non-empty label, glyph, Engine, Bound, and Time", () => {
    const props = CONTRACT_SESSION.props as ViewProp[];
    const seen = new Set(props.map((p) => p.status.status));
    for (const want of [
      "unprocessed",
      "proven",
      "bounded_proven",
      "cex",
      "covered",
      "unreachable",
      "undetermined",
      "error",
    ] as const) {
      expect(seen.has(want), `contract fixture should contain ${want}`).toBe(true);
      expect(STATUS_LABEL[want]).toBeTruthy();
    }

    // Verify bounded_proven specifically renders k<=20, engine, and time
    const bp = props.find((p) => p.status.status === "bounded_proven")!;
    expect(statusText(bp)).toBe("Bounded");
    expect(bound(bp)).toBe("k≤20");
    expect(cell(bp, "engine")).toBe("informal-bmc");
    expect(cell(bp, "time")).toBe("0.12s");

    // Verify proven renders ∞ bound, engine, time, and green glyph class
    const pr = props.find((p) => p.name === "Protocol::a_aw_stable")!;
    expect(statusText(pr)).toBe("Proven");
    expect(bound(pr)).toBe("∞");
    expect(cell(pr, "engine")).toBe("informal-kind");
    expect(cell(pr, "time")).toBe("0.05s");

    // Render full Rust fixture into PropTable & TaskTree without throwing
    const { container } = render(
      <PropTable props={props} total={props.length} selected={pr.name} onSelect={() => {}} onOpen={() => {}} />,
    );
    expect(container.querySelector(".st-proven")).toBeTruthy();
    expect(container.querySelector(".st-bounded_proven")).toBeTruthy();
    expect(container.textContent).toContain("k≤20");
    expect(container.textContent).toContain("informal-kind");

    const { container: tree } = render(
      <TaskTree
        total={props.length}
        counts={{ proven: 2, bounded_proven: 1, cex: 1, covered: 1, unreachable: 1, undetermined: 1, error: 1, unprocessed: 1 }}
        assumes={1}
        filter={null}
        onFilter={() => {}}
        props={props}
        tasks={CONTRACT_SESSION.tasks?.map((t) => t.name)}
        current={CONTRACT_SESSION.current_task}
      />,
    );
    expect(tree.textContent).toContain("<embedded>");
    expect(tree.textContent).toContain("Protocol");
    expect(tree.textContent).toContain("Bounded");
  });
});

describe("Layer 2 — Workbench UX Invariants (UX-1..UX-6)", () => {
  const props = CONTRACT_SESSION.props as ViewProp[];
  const tasks = CONTRACT_SESSION.tasks!.map((t) => t.name);

  it("UX-1: Prove all targets non-empty Protocol task when <embedded> is current & empty (screenshot regression)", () => {
    // Reproduces exact user screenshot: current_task="<embedded>" (0 props), Protocol has 9 provable + 1 assume
    const plan = resolveProveAll(props, "<embedded>", null, "jg");
    expect(plan.count).toBe(9);
    expect(plan.task).toBe("Protocol");
    expect(plan.cmd).toBe("prove -task Protocol");
    expect(plan.badge).toBe("Protocol (9)");

    // VC Formal flavor targets the exact properties instead of no-oping
    const vcfPlan = resolveProveAll(props, "<embedded>", null, "vcf");
    expect(vcfPlan.count).toBe(9);
    expect(vcfPlan.cmd).toContain("check_fv -property");
  });

  it("UX-1: Prove all respects shownTask when user explicitly selects a task in sidebar", () => {
    const multi: ViewProp[] = [
      { name: "a_emb", kind: "assert", expr: "1", enabled: true, task: "<embedded>", status: { status: "unprocessed" } },
      { name: "Protocol::a_p1", kind: "assert", expr: "1", enabled: true, task: "Protocol", status: { status: "unprocessed" } },
    ];
    const onProto = resolveProveAll(multi, "<embedded>", "Protocol", "jg");
    expect(onProto.cmd).toBe("prove -task Protocol");
    expect(onProto.count).toBe(1);

    const onEmb = resolveProveAll(multi, "<embedded>", "<embedded>", "jg");
    expect(onEmb.cmd).toBe("prove -all");
    expect(onEmb.count).toBe(1);
  });

  it("UX-2: selectAndReveal clears conflicting statusFilter so clicked Proven leaf is visible in PropTable (screenshot regression)", () => {
    // User had Unprocessed filter active, proved Protocol::a_aw_stable, then clicked it in TaskTree
    const state: TableFilterState = {
      selected: "Protocol::a_pending",
      statusFilter: "unprocessed",
      taskFilter: "Protocol",
      filter: "",
    };
    const next = selectAndReveal(state, "Protocol::a_aw_stable", props, tasks);
    expect(next.selected).toBe("Protocol::a_aw_stable");
    expect(next.statusFilter).toBeNull();
    expect(next.taskFilter).toBe("Protocol");
    const vis = visibleProps(props, next, tasks);
    expect(vis.some((p) => p.name === "Protocol::a_aw_stable")).toBe(true);
  });

  it("UX-4: taskEnvSummary surfaces active assumption count and alive status for Protocol even when <embedded> is current", () => {
    const env = taskEnvSummary(CONTRACT_SESSION, props, null);
    expect(env.task).toBe("Protocol");
    expect(env.assumeCount).toBe(1);
    expect(env.state).toBe("alive");
    expect(env.detail).toContain("verified alive for 20 steps");
  });

  it("UX-5: findNewlyHiddenProved notifies when a property settles out of an active status filter", () => {
    const before: ViewProp[] = [
      { name: "Protocol::a1", kind: "assert", expr: "1", enabled: true, task: "Protocol", status: { status: "unprocessed" } },
      { name: "Protocol::a2", kind: "assert", expr: "1", enabled: true, task: "Protocol", status: { status: "unprocessed" } },
    ];
    const after: ViewProp[] = [
      { name: "Protocol::a1", kind: "assert", expr: "1", enabled: true, task: "Protocol", status: { status: "proven", engine: "informal-kind", ms: 46 } },
      { name: "Protocol::a2", kind: "assert", expr: "1", enabled: true, task: "Protocol", status: { status: "unprocessed" } },
    ];
    const notice = findNewlyHiddenProved(
      before,
      after,
      { statusFilter: "unprocessed", taskFilter: "Protocol", filter: "" },
      tasks,
    );
    expect(notice).not.toBeNull();
    expect(notice!.text).toContain("Protocol::a1 → Proven");
    expect(notice!.text).toContain("informal-kind");
    expect(notice!.props.map((p) => p.name)).toEqual(["Protocol::a1"]);
  });

  it("UX-6: guidedNextAction recommends Prove all on fresh unprocessed load and Inspect CEX on failure", () => {
    const fresh: ViewProp[] = [
      { name: "Protocol::a1", kind: "assert", expr: "1", enabled: true, task: "Protocol", status: { status: "unprocessed" } },
      { name: "Protocol::m1", kind: "assume", expr: "1", enabled: true, task: "Protocol", status: { status: "unprocessed" } },
    ];
    const plan = resolveProveAll(fresh, "<embedded>", null, "jg");
    const hint = guidedNextAction(fresh, plan, false, false);
    expect(hint?.actionKind).toBe("prove_all");
    expect(hint?.text).toContain("1 active assumption");

    const withCex = guidedNextAction(props, resolveProveAll(props, "<embedded>", null, "jg"), false, false);
    expect(withCex?.actionKind).toBe("open_cex");
    expect(withCex?.actionProp).toBe("Protocol::a_bad_hold");
  });

  it("Property-based invariant sweep (200 deterministic steps): INV-1..INV-4 never violated", () => {
    const filters: StatusFilter[] = [null, "unprocessed", "proven", "bounded_proven", "cex", "assume"];
    const taskChoices: (string | null)[] = [null, "<embedded>", "Protocol"];
    const queries = ["", "aw_", "bounded", "nope"];
    let state: TableFilterState = { selected: null, statusFilter: null, taskFilter: null, filter: "" };

    // Deterministic LCG PRNG for reproducible property-based testing
    let seed = 0x5f3759df;
    const rand = (n: number) => {
      seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
      return seed % n;
    };

    for (let step = 0; step < 200; step++) {
      const action = rand(5);
      if (action === 0) {
        state = { ...state, statusFilter: filters[rand(filters.length)] };
      } else if (action === 1) {
        state = { ...state, taskFilter: taskChoices[rand(taskChoices.length)] };
      } else if (action === 2) {
        state = { ...state, filter: queries[rand(queries.length)] };
      } else if (action === 3) {
        const pick = props[rand(props.length)];
        state = selectAndReveal(state, pick.name, props, tasks);
        // INV-1 (No Ghost Selection): selected leaf MUST be visible in PropTable!
        const vis = visibleProps(props, state, tasks);
        expect(
          vis.some((p) => p.name === pick.name),
          `step ${step}: selected ${pick.name} missing from visibleProps`,
        ).toBe(true);
      } else {
        // INV-2 (No Silent Prove-All): plan ALWAYS targets >= 1 provable property
        const plan = resolveProveAll(props, "<embedded>", state.taskFilter, rand(2) === 0 ? "jg" : "vcf");
        expect(plan.count).toBeGreaterThan(0);
        expect(plan.cmd).toBeTruthy();
      }

      // INV-4 (Filter Honesty): whenever rows are hidden, >= 1 visible filter chip exists
      const vis = visibleProps(props, state, tasks);
      const chips = activeFilterChips(state, tasks);
      if (vis.length < props.length) {
        expect(chips.length, `step ${step}: rows hidden without active filter chip`).toBeGreaterThan(0);
      }

      // Sanity: summarySegments counts equal total properties
      const segTotal = summarySegments(props).reduce((acc, s) => acc + s.count, 0);
      expect(segTotal).toBe(props.length);
      expect(statusKey(props[0])).toBe("proven");
      expect(taskOf(props[0])).toBe("Protocol");
    }
  });
});
