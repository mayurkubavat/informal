import { describe, expect, it } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import vcdText from "../../tests/fixtures/a_never9.vcd?raw";
import { prop } from "../test/fixtures";
import { TraceView } from "./TraceView";

const cex = prop("a_never9", { expr: "q != 4'd9", trace: "fdv_work/a_never9.vcd" }, { status: "cex", engine: "fdv-bmc", depth: 11, ms: 30 });
const view = (over: Partial<Parameters<typeof TraceView>[0]> = {}) =>
  render(<TraceView tabs={<div />} prop={cex} path="fdv_work/a_never9.vcd" text={vcdText} {...over} />);
const lane = (path: string) => screen.getByTitle(new RegExp(`^${path.replace(".", "\\.")}`));

describe("TraceView", () => {
  it("shows the strip, default signals and the cursor on the failing cycle", () => {
    view();
    expect(screen.getByText("CEX · depth 11 · fdv-bmc · 12 cycles")).toBeInTheDocument();
    expect(screen.getByRole("status", { name: "Cursor" })).toHaveTextContent("cycle 11");
    const names = [...document.querySelectorAll(".trace-names .trace-name .ellipsis")].map((e) => e.textContent);
    // fdv.* helpers are hidden until added
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
    expect(within(dialog).getAllByRole("option").map((o) => o.textContent)).toEqual(["fdv.fdv_cyc [15:0]"]);
    await userEvent.type(within(dialog).getByLabelText("Filter signals"), "zzz");
    expect(within(dialog).getByText("No signal matches.")).toBeInTheDocument();
    await userEvent.clear(within(dialog).getByLabelText("Filter signals"));
    await userEvent.click(within(dialog).getByRole("option"));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(within(lane("fdv.fdv_cyc")).getByText("b")).toBeInTheDocument(); // cycle 11 in hex
  });

  it("marks a cover with the covered colour", () => {
    view({ prop: { ...cex, name: "c_done", kind: "cover", status: { status: "covered", engine: "fdv-bmc", depth: 11 } } });
    expect(screen.getByText("Covered · depth 11 · fdv-bmc · 12 cycles")).toBeInTheDocument();
    expect(document.querySelector(".trace-marker")).toHaveClass("st-covered");
    expect(document.querySelector(".trace-markertext")).toHaveTextContent("cover");
  });

  it("says when the trace cannot be read", () => {
    view({ text: null, error: "no such trace" });
    expect(screen.getByText("Could not read fdv_work/a_never9.vcd: no such trace")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Export VCD" })).toBeDisabled();
  });
});
