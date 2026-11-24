import { describe, expect, it } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import vcdText from "../../tests/fixtures/a_never9.vcd?raw";
import { prop } from "../test/fixtures";
import { TraceView } from "./TraceView";

const cex = prop("a_never9", { expr: "q != 4'd9", trace: "informal_work/a_never9.vcd" }, { status: "cex", engine: "informal-bmc", depth: 11, ms: 30 });
const view = (over: Partial<Parameters<typeof TraceView>[0]> = {}) =>
  render(<TraceView tabs={<div />} prop={cex} path="informal_work/a_never9.vcd" text={vcdText} {...over} />);
const lane = (path: string) => screen.getByTitle(new RegExp(`^${path.replace(".", "\\.")}`));

describe("TraceView", () => {
  it("shows the strip, default signals and the cursor on the failing cycle", () => {
    view();
    expect(screen.getByText("CEX · depth 11 · informal-bmc · 12 cycles")).toBeInTheDocument();
    expect(screen.getByRole("status", { name: "Cursor" })).toHaveTextContent("cycle 11");
    const names = [...document.querySelectorAll(".trace-names .trace-name .ellipsis")].map((e) => e.textContent);
    // informal.* helpers are hidden until added
    expect(names).toEqual(["a_never9", "counter.clk", "counter.rst_n", "counter.en", "counter.q", "counter.done"]);
    expect(within(lane("counter.q")).getByText("9")).toBeInTheDocument();
    expect(document.querySelectorAll('[data-signal="counter.q"] text')).toHaveLength(10); // 0,1,2,...,9 (2 and 0 held)
  });

  it("moves the cursor with the keyboard and updates values", () => {
    view();
    const trace = screen.getByLabelText(/^Trace of a_never9/);
    fireEvent.keyDown(trace, { key: "Home" });
    expect(screen.getByRole("status", { name: "Cursor" })).toHaveTextContent("cycle 0");
    expect(within(lane("counter.rst_n")).getByText("0")).toBeInTheDocument();
    fireEvent.keyDown(trace, { key: "ArrowRight" });
    fireEvent.keyDown(trace, { key: "ArrowRight" });
    fireEvent.keyDown(trace, { key: "ArrowRight" });
    expect(screen.getByRole("status", { name: "Cursor" })).toHaveTextContent("cycle 3");
    expect(within(lane("counter.en")).getByText("0")).toBeInTheDocument();
    fireEvent.keyDown(trace, { key: "ArrowLeft" });
    expect(within(lane("counter.q")).getByText("1")).toBeInTheDocument();
    fireEvent.keyDown(trace, { key: "End" });
    expect(screen.getByRole("status", { name: "Cursor" })).toHaveTextContent("cycle 11");
    fireEvent.keyDown(trace, { key: "ArrowRight" });
    expect(screen.getByRole("status", { name: "Cursor" })).toHaveTextContent("cycle 11");
  });

  it("switches a bus's radix", async () => {
    view();
    await userEvent.selectOptions(screen.getByLabelText("Radix of counter.q"), "bin");
    expect(within(lane("counter.q")).getByText("1001")).toBeInTheDocument();
    await userEvent.selectOptions(screen.getByLabelText("Radix of counter.q"), "dec");
    expect(within(lane("counter.q")).getByText("9")).toBeInTheDocument();
  });

  it("zooms between 40 and 160 px per cycle", async () => {
    view();
    const svg = screen.getByRole("img", { name: "Waveforms, 12 cycles" });
    expect(svg).toHaveAttribute("width", String(12 * 60));
    await userEvent.click(screen.getByRole("button", { name: "Zoom out" }));
    expect(svg).toHaveAttribute("width", String(12 * 40));
    expect(screen.getByRole("button", { name: "Zoom out" })).toBeDisabled();
    for (let i = 0; i < 4; i++) await userEvent.click(screen.getByRole("button", { name: "Zoom in" }));
    expect(svg).toHaveAttribute("width", String(12 * 160));
    expect(screen.getByRole("button", { name: "Zoom in" })).toBeDisabled();
  });

  it("adds a hidden signal from the filterable list", async () => {
    view();
    await userEvent.click(screen.getByRole("button", { name: "Add signal…" }));
    const dialog = screen.getByRole("dialog", { name: "Add signal" });
    expect(within(dialog).getAllByRole("option").map((o) => o.textContent)).toEqual(["informal.informal_cyc [15:0]"]);
    await userEvent.type(within(dialog).getByLabelText("Filter signals"), "zzz");
    expect(within(dialog).getByText("No signal matches.")).toBeInTheDocument();
    await userEvent.clear(within(dialog).getByLabelText("Filter signals"));
    await userEvent.click(within(dialog).getByRole("option"));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(within(lane("informal.informal_cyc")).getByText("b")).toBeInTheDocument(); // cycle 11 in hex
  });

  it("marks a cover with the covered colour", () => {
    view({ prop: { ...cex, name: "c_done", kind: "cover", status: { status: "covered", engine: "informal-bmc", depth: 11 } } });
    expect(screen.getByText("Covered · depth 11 · informal-bmc · 12 cycles")).toBeInTheDocument();
    expect(document.querySelector(".trace-marker")).toHaveClass("st-covered");
    expect(document.querySelector(".trace-markertext")).toHaveTextContent("cover");
  });

  it("says when the trace cannot be read", () => {
    view({ text: null, error: "no such trace" });
    expect(screen.getByText("Could not read informal_work/a_never9.vcd: no such trace")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Export VCD" })).toBeDisabled();
  });

  it("adds a signal requested from the Design tab once, and reports shown or missing ones", () => {
    const results: [string, string][] = [];
    const onAddResult = (p: string, r: string) => results.push([p, r]);
    const names = () => [...document.querySelectorAll(".trace-names .trace-name .ellipsis")].map((e) => e.textContent);
    const { rerender } = view({ addRequest: { path: "informal.informal_cyc", n: 1 }, onAddResult });
    expect(names()).toContain("informal.informal_cyc");
    // The same request again (a re-render) does nothing.
    rerender(<TraceView tabs={<div />} prop={cex} path="informal_work/a_never9.vcd" text={vcdText} addRequest={{ path: "informal.informal_cyc", n: 1 }} onAddResult={onAddResult} />);
    rerender(<TraceView tabs={<div />} prop={cex} path="informal_work/a_never9.vcd" text={vcdText} addRequest={{ path: "counter.q", n: 2 }} onAddResult={onAddResult} />);
    rerender(<TraceView tabs={<div />} prop={cex} path="informal_work/a_never9.vcd" text={vcdText} addRequest={{ path: "counter.nosuch", n: 3 }} onAddResult={onAddResult} />);
    expect(results).toEqual([["informal.informal_cyc", "added"], ["counter.q", "shown"], ["counter.nosuch", "missing"]]);
    expect(names().filter((n) => n === "informal.informal_cyc")).toHaveLength(1);
  });

  it("puts the cursor where asked from outside, once, and reports what it shows", () => {
    const views: { cursor: number; signals: string[] }[] = [];
    const onView = (v: { cursor: number; signals: string[] }) => views.push(v);
    const { rerender } = view({ cursorRequest: { cycle: 4, n: 1 }, onView });
    expect(screen.getByRole("status", { name: "Cursor" })).toHaveTextContent("cycle 4");
    expect(views[views.length - 1]).toEqual({ cursor: 4, signals: ["counter.clk", "counter.rst_n", "counter.en", "counter.q", "counter.done"] });
    // The person moves on; the same request again (a re-render) does not pull the cursor back.
    fireEvent.keyDown(screen.getByLabelText(/^Trace of a_never9/), { key: "Home" });
    rerender(<TraceView tabs={<div />} prop={cex} path="informal_work/a_never9.vcd" text={vcdText} cursorRequest={{ cycle: 4, n: 1 }} onView={onView} />);
    expect(screen.getByRole("status", { name: "Cursor" })).toHaveTextContent("cycle 0");
    expect(views[views.length - 1].cursor).toBe(0);
    // A cycle past the end stops at the last one.
    rerender(<TraceView tabs={<div />} prop={cex} path="informal_work/a_never9.vcd" text={vcdText} cursorRequest={{ cycle: 99, n: 2 }} onView={onView} />);
    expect(screen.getByRole("status", { name: "Cursor" })).toHaveTextContent("cycle 11");
  });

  it("pins lp.pd.* power-domain swimlanes and displays domain states across cycles", () => {
    const lpVcd = [
      "$timescale 1ns $end",
      "$scope module top $end",
      "$var wire 1 ! clk $end",
      "$var wire 1 \" dout $end",
      "$upscope $end",
      "$scope module lp $end",
      "$scope module pd $end",
      "$scope module PD_CORE $end",
      "$var wire 1 # on $end",
      "$var wire 1 $ iso $end",
      "$var wire 1 % ret $end",
      "$var wire 1 & corrupt $end",
      "$upscope $end",
      "$upscope $end",
      "$upscope $end",
      "$enddefinitions $end",
      "#0",
      "0!",
      "0\"",
      "1#",
      "0$",
      "0%",
      "0&",
      "#10",
      "1!",
      "1$",
      "#20",
      "0!",
      "#30",
      "1!",
      "0#",
      "1%",
      "1&",
      "#40",
      "",
    ].join("\n");
    const lpProp = prop(
      "LowPower::lp_iso_before_pwroff_PD_CORE",
      { expr: "pwr_off |-> iso", trace: "informal_work/lp.vcd" },
      { status: "cex", engine: "informal-bmc", depth: 2, ms: 5 },
    );
    render(<TraceView tabs={<div />} prop={lpProp} path="informal_work/lp.vcd" text={lpVcd} />);
    const swimlanes = screen.getByRole("region", { name: "Power domain swimlanes" });
    expect(swimlanes).toHaveTextContent("PD_CORE: RET");
    const names = [...document.querySelectorAll(".trace-names .trace-name .ellipsis")].map((e) => e.textContent);
    expect(names[1]).toMatch(/^lp\.pd\.PD_CORE\./);
  });
});
