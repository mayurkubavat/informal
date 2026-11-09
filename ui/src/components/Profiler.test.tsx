import { describe, expect, it } from "vitest";
import { render, screen, within } from "@testing-library/react";
import { Profiler } from "./Profiler";
import { MOCK_ENGINES } from "../mock";
import { prop } from "../test/fixtures";

const props = [
  prop("a_never9", { engines: MOCK_ENGINES.a_never9 }, { status: "cex", engine: "ric3-bmc", depth: 11, ms: 30 }),
  prop("a_max", { engines: MOCK_ENGINES.a_max }, { status: "proven", engine: "ric3-ic3", ms: 41 }),
  prop("a_new"),
];

describe("Profiler", () => {
  it("lists the selected property's engines with result, time, depth and a bar", () => {
    render(<Profiler prop={props[0]} props={props} />);
    const pane = screen.getByRole("region", { name: "Engines on the selected property" });
    const rows = within(pane).getAllByRole("row").slice(1);
    expect(rows.map((r) => r.textContent)).toEqual(["ric3-ic3Killed31 ms2", "ric3-bmcWon30 ms11", "informal-kindKilled31 ms9"]);
    // Plain bars: only the winner's is marked (drawn in --text, not a colour).
    const bars = pane.querySelectorAll(".prof-bar");
    expect([...bars].map((b) => b.classList.contains("win"))).toEqual([false, true, false]);
    expect((bars[0] as HTMLElement).style.width).toBe("100%");
  });
  it("summarises every profiled property per engine", () => {
    render(<Profiler prop={null} props={props} />);
    const pane = screen.getByRole("region", { name: "Engines over the run" });
    expect(pane).toHaveTextContent("2 properties profiled");
    const rows = within(pane).getAllByRole("row").slice(1).map((r) => r.textContent);
    expect(rows[0]).toBe("ric3-bmc1272 ms30 ms");
    expect(rows[1]).toBe("ric3-ic31272 ms41 ms");
    expect(rows[2]).toBe("informal-kind0274 ms—");
    expect(screen.getByText("Select a property to see how each engine did on it.")).toBeInTheDocument();
  });
  it("explains an unprofiled property and an empty run", () => {
    render(<Profiler prop={props[2]} props={[props[2]]} />);
    expect(screen.getByText(/No engine runs recorded for this property yet/)).toBeInTheDocument();
    expect(screen.getByText(/No profile yet/)).toBeInTheDocument();
  });
});
