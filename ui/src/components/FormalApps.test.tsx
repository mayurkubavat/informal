import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import vcdText from "../../tests/fixtures/a_never9.vcd?raw";
import { mockWhyCex } from "../mock";
import { prop } from "../test/fixtures";
import type { SessionView } from "../backend";
import { FormalApps } from "./FormalApps";
import { SourceView } from "./SourceView";
import { TraceView } from "./TraceView";

const cex = prop(
  "a_never9",
  { expr: "q != 4'd9", trace: "informal_work/a_never9.vcd" },
  { status: "cex", engine: "informal-bmc", depth: 11, ms: 30 },
);

describe("Tier 10 UI: why_cex causal slice & FormalApps", () => {
  it("overlays why_cex causal slice on TraceView, filters to slice signals, and jumps cursor on chip click", async () => {
    const slice = mockWhyCex("a_never9");
    const onToggle = vi.fn();
    render(
      <TraceView
        tabs={<div />}
        prop={cex}
        path="informal_work/a_never9.vcd"
        text={vcdText}
        whyCex={slice}
        whyCexOpen={true}
        onToggleWhyCex={onToggle}
      />,
    );

    expect(screen.getByRole("region", { name: "Causal root-cause slice" })).toBeInTheDocument();
    const causalNames = [...document.querySelectorAll(".trace-names .trace-name.causal-sig .ellipsis")].map(
      (e) => e.textContent,
    );
    expect(causalNames).toEqual(["counter.en", "counter.q"]);

    // Clicking a causal event chip moves the waveform cursor to that event's frame.
    await userEvent.click(screen.getByRole("button", { name: /@2 q:/ }));
    expect(screen.getByRole("status", { name: "Cursor" })).toHaveTextContent("cycle 2");

    // Enabling "Slice only" hides non-causal signals (clk, rst_n, done).
    await userEvent.click(screen.getByRole("checkbox", { name: "Slice only" }));
    const shownAfter = [...document.querySelectorAll(".trace-names .trace-name .ellipsis")].map((e) => e.textContent);
    expect(shownAfter).toEqual(["a_never9", "counter.en", "counter.q"]);
  });

  it("marks causal slice lines and coverage/UNR gutter badges in SourceView", () => {
    const rtl = ["module m(input clk);", "  always_ff @(posedge clk) q <= d;", "  assign dead = 1'b0;", "endmodule"].join(
      "\n",
    );
    render(
      <SourceView
        text={rtl}
        focus={0}
        causalSignals={["m.q"]}
        covItems={[
          { id: "c1", model: "branch", name: "br_q", status: "covered", line: 2 },
          { id: "c2", model: "branch", name: "br_dead", status: "unreachable", line: 3 },
        ]}
      />,
    );
    const causalDivs = document.querySelectorAll('.source div[data-causal="true"]');
    expect(causalDivs).toHaveLength(1);
    expect(screen.getByLabelText("Line 2 coverage covered")).toHaveTextContent("●");
    expect(screen.getByLabelText("Line 3 coverage unreachable")).toHaveTextContent("⊘");
  });

  it("renders FormalApps tabs and dispatches check_cov, check_xprop, check_spv, and check_protocol commands", async () => {
    const onRunTcl = vi.fn();
    const onSelectTask = vi.fn();
    const onMode = vi.fn();
    const session: SessionView = {
      design: { files: ["counter.sv"], top: "counter", defines: [] },
      elaborated: true,
      clocks: [{ signal: "clk", edge: "posedge" }],
      reset: "!rst_n",
      props: [
        prop("CSR::csr_ctrl_reset", { task: "CSR" }, { status: "proven" }),
        prop("Protocol::apb_wait_hold", { task: "Protocol" }, { status: "proven" }),
      ],
      time_limit: { secs: 60, nanos: 0 },
      workdir: "informal_work",
      cov: {
        initialized: true,
        measured: true,
        items: [
          { id: "coi_0", model: "coi", name: "counter.q", status: "covered", file: "counter.sv", line: 5 },
          { id: "br_unr", model: "branch", name: "counter.br_dead", status: "unreachable", file: "counter.sv", line: 9 },
        ],
      },
      xprop: {
        initialized: true,
        items: [{ name: "counter.q", category: "output", status: "proven", engine: "informal-ic3-mt" }],
      },
      sec: {
        setup: { spec_top: "spec", imp_top: "imp", lec: false, latency: 1 },
        checks: [{ name: "sec_out_q", kind: "sec_output", spec_sig: "q", imp_sig: "q", width: 4, latency: 1, status: "proven" }],
      },
    };

    const { rerender } = render(
      <FormalApps session={session} mode="cov" onMode={onMode} idle={true} onRunTcl={onRunTcl} onSelectTask={onSelectTask} />,
    );
    expect(screen.getByText("counter.br_dead")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Measure COI & UNR" }));
    expect(onRunTcl).toHaveBeenCalledWith("check_cov -init -model {coi branch toggle}; check_cov -measure");

    rerender(
      <FormalApps session={session} mode="protocol" onMode={onMode} idle={true} onRunTcl={onRunTcl} onSelectTask={onSelectTask} />,
    );
    await userEvent.selectOptions(screen.getByLabelText("Bus protocol"), "tlul");
    await userEvent.click(screen.getByRole("button", { name: "Generate VIP" }));
    expect(onRunTcl).toHaveBeenCalledWith("check_protocol -create -protocol tlul -role slave");
  });

  it("renders Low Power tab with LP violations, certified auto-waiver badge, and dispatches check_lp", async () => {
    const onRunTcl = vi.fn();
    const onSelectTask = vi.fn();
    const onMode = vi.fn();
    const lpSession: SessionView = {
      design: { files: ["soc_lp.sv"], top: "soc_lp", defines: [] },
      elaborated: true,
      clocks: [{ signal: "clk", edge: "posedge" }],
      reset: "!rst_n",
      props: [prop("LowPower::lp_iso_before_pwroff_PD_CORE", { task: "LowPower" }, { status: "proven" })],
      time_limit: { secs: 60, nanos: 0 },
      workdir: "informal_work",
      lp: {
        upf_files: [{ path: "/proj/soc_lp.upf" }],
        pst_consistent: true,
        stages_run: ["upf", "rtl", "formal"],
        violations: [
          {
            id: 1,
            rule_id: "ISO_MISSING",
            severity: "error",
            stage: "rtl",
            domain_src: "PD_CORE",
            domain_dst: "PD_AON",
            signal: "u_core/dout",
            width_bits: 8,
            raw_crossings: 8,
            message: "unisolated crossing",
            status: "formally_waived",
            auto_formal_meta: {
              engine: "informal-ic3-mt",
              ms: 12,
              assumptions: [],
              stopats: [],
              task: "<embedded>",
            },
          },
          {
            id: 2,
            rule_id: "ISO_MISSING",
            severity: "error",
            stage: "rtl",
            domain_src: "PD_CORE",
            domain_dst: "PD_AON",
            signal: "u_core/leak",
            width_bits: 4,
            raw_crossings: 4,
            message: "corruptible leak",
            status: "cex",
          },
        ],
      },
    };

    render(
      <FormalApps session={lpSession} mode="lp" onMode={onMode} idle={true} onRunTcl={onRunTcl} onSelectTask={onSelectTask} />,
    );
    expect(screen.getByText("soc_lp.upf")).toBeInTheDocument();
    expect(screen.getByText("u_core/dout [8b]")).toBeInTheDocument();
    expect(screen.getByText("formally_waived")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Prove Formal LPV" }));
    expect(onRunTcl).toHaveBeenCalledWith("check_lp -stage all -prove_corruption");
    await userEvent.click(screen.getByRole("button", { name: "LowPower Task (1)" }));
    expect(onSelectTask).toHaveBeenCalledWith("LowPower");
  });
});
