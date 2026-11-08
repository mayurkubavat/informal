import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { DesignTree } from "./DesignTree";
import { MOCK_FIFO, MOCK_HIERARCHY } from "../mock";

const scopeRows = () => within(screen.getByRole("tree", { name: "Design hierarchy" })).getAllByRole("treeitem");
const sigNames = (path: string) =>
  within(screen.getByRole("listbox", { name: `Signals in ${path}` }))
    .queryAllByRole("option")
    .map((o) => o.querySelector(".sig-name")!.textContent);

describe("DesignTree", () => {
  it("shows the top scope open and selected, with its signals' width and kind", () => {
    render(<DesignTree scopes={MOCK_HIERARCHY} files={["tests/fpv/counter/counter.sv"]} />);
    expect(scopeRows().map((r) => r.textContent)).toEqual(["countercounter"]);
    expect(sigNames("counter")).toEqual(["clk", "rst_n", "en", "q", "done"]);
    const q = screen.getByRole("option", { name: /^q/ });
    expect(q).toHaveTextContent("q4out");
    expect(screen.getByRole("option", { name: /^clk/ })).toHaveTextContent("clk1in");
    expect(screen.getByText("5 signals")).toBeInTheDocument();
    expect(screen.getByText("tests/fpv/counter/counter.sv")).toBeInTheDocument();
  });

  it("expands and collapses instances; selecting one lists its signals", async () => {
    render(<DesignTree scopes={MOCK_FIFO} files={[]} />);
    expect(scopeRows().map((r) => r.textContent)).toEqual(["fifofifo", "u_wpptr", "u_rpptr"]);
    expect(scopeRows()[0]).toHaveAttribute("aria-expanded", "true");
    expect(scopeRows()[1]).toHaveAttribute("aria-level", "2");
    await userEvent.click(scopeRows()[2]);
    expect(scopeRows()[2]).toHaveAttribute("aria-selected", "true");
    expect(sigNames("fifo.u_rp")).toEqual(["clk", "rst_n", "inc", "p", "W"]);
    expect(screen.getByRole("option", { name: /^W/ })).toHaveTextContent("W32param");
    // Collapse the top from the keyboard; the children go away.
    fireEvent.keyDown(scopeRows()[0], { key: "ArrowLeft" });
    expect(scopeRows()).toHaveLength(1);
    fireEvent.doubleClick(scopeRows()[0]);
    expect(scopeRows()).toHaveLength(3);
  });

  it("filters the selected scope's signals, case-insensitively", async () => {
    render(<DesignTree scopes={MOCK_FIFO} files={[]} />);
    await userEvent.type(screen.getByRole("textbox", { name: "Filter design signals" }), "DO_");
    expect(sigNames("fifo")).toEqual(["do_push", "do_pop"]);
    await userEvent.type(screen.getByRole("textbox", { name: "Filter design signals" }), "x");
    expect(screen.getByText("No signal matches.")).toBeInTheDocument();
  });

  it("double-click or Enter on a signal asks to add its hierarchical path to the trace", async () => {
    const onAdd = vi.fn();
    render(<DesignTree scopes={MOCK_FIFO} files={[]} onAddSignal={onAdd} />);
    await userEvent.dblClick(screen.getByRole("option", { name: /^count/ }));
    expect(onAdd).toHaveBeenLastCalledWith("fifo.count");
    await userEvent.click(scopeRows()[1]);
    fireEvent.keyDown(screen.getByRole("option", { name: /^p/ }), { key: "Enter" });
    expect(onAdd).toHaveBeenLastCalledWith("fifo.u_wp.p");
  });

  it("says so when nothing is elaborated, and marks the port-scan fallback", () => {
    const { rerender } = render(<DesignTree scopes={[]} files={[]} />);
    expect(screen.getByText("No design elaborated.")).toBeInTheDocument();
    rerender(<DesignTree scopes={MOCK_HIERARCHY} files={[]} fallback />);
    expect(screen.getByText("ports (scan)")).toBeInTheDocument();
  });
});
