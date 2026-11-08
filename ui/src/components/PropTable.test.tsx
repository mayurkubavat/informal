import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { OVERSCAN, PropTable, ROW_H, scrollFor, visibleRange } from "./PropTable";
import { prop } from "../test/fixtures";

const props = [
  prop("b_step", {}, { status: "proven", engine: "ic3", ms: 50 }),
  prop("a_max", {}, { status: "cex", engine: "bmc", depth: 3, ms: 20 }),
  prop("c_cov", { kind: "cover" }),
];
const names = () => screen.getAllByRole("row").slice(1).map((r) => within(r).getAllByRole("cell")[1].textContent);
const table = (over: Partial<Parameters<typeof PropTable>[0]> = {}) => {
  const onSelect = vi.fn();
  const onOpen = vi.fn();
  const utils = render(<PropTable props={props} total={props.length} selected={null} onSelect={onSelect} onOpen={onOpen} {...over} />);
  return { onSelect, onOpen, ...utils };
};

describe("PropTable", () => {
  it("renders a row per property with kind letter, status and bound", () => {
    table();
    expect(names()).toEqual(["b_step", "a_max", "c_cov"]);
    const cex = screen.getByRole("row", { name: /a_max/ });
    expect(within(cex).getByText("A")).toBeInTheDocument();
    expect(within(cex).getByText("CEX")).toBeInTheDocument();
    expect(within(cex).getByText("3")).toBeInTheDocument();
    expect(within(screen.getByRole("row", { name: /c_cov/ })).getByText("C")).toBeInTheDocument();
  });

  it("cycles sort asc, desc, off and sets aria-sort", async () => {
    table();
    const th = screen.getByRole("columnheader", { name: "Name" });
    expect(th).not.toHaveAttribute("aria-sort");
    await userEvent.click(th);
    expect(th).toHaveAttribute("aria-sort", "ascending");
    expect(names()).toEqual(["a_max", "b_step", "c_cov"]);
    await userEvent.click(th);
    expect(th).toHaveAttribute("aria-sort", "descending");
    expect(names()).toEqual(["c_cov", "b_step", "a_max"]);
    await userEvent.click(th);
    expect(th).not.toHaveAttribute("aria-sort");
    expect(names()).toEqual(["b_step", "a_max", "c_cov"]);
  });

  it("sorts by status severity and switches column on a new header", async () => {
    table();
    await userEvent.click(screen.getByRole("columnheader", { name: "Name" }));
    const st = screen.getByRole("columnheader", { name: "Status" });
    await userEvent.click(st);
    expect(st).toHaveAttribute("aria-sort", "ascending");
    expect(screen.getByRole("columnheader", { name: "Name" })).not.toHaveAttribute("aria-sort");
    expect(names()).toEqual(["a_max", "b_step", "c_cov"]);
  });

  it("ignores clicks on the unsortable Task column", async () => {
    table();
    const th = screen.getByRole("columnheader", { name: "Task" });
    await userEvent.click(th);
    expect(th).not.toHaveAttribute("aria-sort");
  });

  it("moves the selection with arrow keys, clamped at both ends", () => {
    const { onSelect, rerender, onOpen } = table({ selected: null });
    const wrap = screen.getByLabelText(/Property table/);
    fireEvent.keyDown(wrap, { key: "ArrowDown" });
    expect(onSelect).toHaveBeenLastCalledWith("b_step");
    rerender(<PropTable props={props} total={3} selected="a_max" onSelect={onSelect} onOpen={onOpen} />);
    fireEvent.keyDown(wrap, { key: "ArrowDown" });
    expect(onSelect).toHaveBeenLastCalledWith("c_cov");
    fireEvent.keyDown(wrap, { key: "ArrowUp" });
    expect(onSelect).toHaveBeenLastCalledWith("b_step");
    rerender(<PropTable props={props} total={3} selected="c_cov" onSelect={onSelect} onOpen={onOpen} />);
    fireEvent.keyDown(wrap, { key: "ArrowDown" });
    expect(onSelect).toHaveBeenLastCalledWith("c_cov");
    onSelect.mockClear();
    fireEvent.keyDown(wrap, { key: "x" });
    expect(onSelect).not.toHaveBeenCalled();
  });

  it("marks the selected row and reports click / double-click", async () => {
    const { onSelect, onOpen } = table({ selected: "a_max" });
    expect(screen.getByRole("row", { name: /a_max/ })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("row", { name: /b_step/ })).toHaveAttribute("aria-selected", "false");
    await userEvent.click(screen.getByText("c_cov"));
    expect(onSelect).toHaveBeenLastCalledWith("c_cov");
    await userEvent.dblClick(screen.getByText("b_step"));
    expect(onOpen).toHaveBeenCalledWith(expect.objectContaining({ name: "b_step" }));
  });

  it("explains an empty table: no properties vs nothing matching", () => {
    const { rerender } = table({ props: [], total: 0 });
    expect(screen.getByText(/No properties\. Source a JasperGold or VC Formal script/)).toBeInTheDocument();
    rerender(<PropTable props={[]} total={3} selected={null} onSelect={() => {}} onOpen={() => {}} />);
    expect(screen.getByText("No properties match the current filter.")).toBeInTheDocument();
  });

  it("reports right-click and Shift+F10 / Menu key for the context menu", () => {
    const onContext = vi.fn();
    const { rerender } = table({ onContext });
    fireEvent.contextMenu(screen.getByText("a_max"), { clientX: 30, clientY: 40 });
    expect(onContext).toHaveBeenLastCalledWith(expect.objectContaining({ name: "a_max" }), 30, 40);
    const wrap = screen.getByLabelText(/Property table/);
    onContext.mockClear();
    fireEvent.keyDown(wrap, { key: "F10", shiftKey: true });
    expect(onContext).not.toHaveBeenCalled();
    rerender(<PropTable props={props} total={3} selected="c_cov" onSelect={() => {}} onOpen={() => {}} onContext={onContext} />);
    fireEvent.keyDown(wrap, { key: "ContextMenu" });
    expect(onContext).toHaveBeenLastCalledWith(expect.objectContaining({ name: "c_cov" }), expect.any(Number), expect.any(Number));
  });

  it("opens the selected row on Enter and ignores Enter without a selection", () => {
    const { onOpen, rerender } = table({ selected: null });
    const wrap = screen.getByLabelText(/Property table/);
    fireEvent.keyDown(wrap, { key: "Enter" });
    expect(onOpen).not.toHaveBeenCalled();
    rerender(<PropTable props={props} total={3} selected="a_max" onSelect={() => {}} onOpen={onOpen} />);
    fireEvent.keyDown(wrap, { key: "Enter" });
    expect(onOpen).toHaveBeenCalledWith(expect.objectContaining({ name: "a_max" }));
  });

  describe("virtual rows", () => {
    const many = Array.from({ length: 1000 }, (_, i) => prop(`p_${String(i).padStart(4, "0")}`));

    it("computes the visible slice with overscan, clamped to the rows", () => {
      expect(visibleRange(1000, 0, 220)).toEqual({ start: 0, end: 11 + OVERSCAN });
      const mid = visibleRange(1000, 500 * ROW_H, 220);
      expect(mid).toEqual({ start: 500 - OVERSCAN, end: 500 + 11 + OVERSCAN });
      expect(visibleRange(1000, 999 * ROW_H, 220).end).toBe(1000);
      expect(visibleRange(5, 0, 220)).toEqual({ start: 0, end: 5 });
    });

    it("scrolls just enough to show a row", () => {
      expect(scrollFor(9, 0, 220)).toBe(0); // already in view (rows 0..9)
      expect(scrollFor(20, 0, 220)).toBe(21 * ROW_H - 220); // below: bottom-aligned
      expect(scrollFor(3, 100 * ROW_H, 220)).toBe(3 * ROW_H); // above: top-aligned
    });

    it("renders a slice with spacer rows that keep the full height", () => {
      const { container } = table({ props: many, total: 1000 });
      const real = container.querySelectorAll("tbody tr:not(.spacer)");
      expect(real.length).toBeLessThan(100);
      const spacer = container.querySelector<HTMLElement>("tbody tr.spacer:last-child")!;
      expect(parseInt(spacer.style.height)).toBe((1000 - real.length) * ROW_H);
      expect(container.querySelector("table")).toHaveAttribute("aria-rowcount", "1001");
      expect(real[0]).toHaveAttribute("aria-rowindex", "2");
    });

    it("Home, End, PageDown and PageUp select by position", () => {
      const onSelect = vi.fn();
      const { rerender } = render(<PropTable props={many} total={1000} selected="p_0500" onSelect={onSelect} onOpen={() => {}} />);
      const wrap = screen.getByLabelText(/Property table/);
      fireEvent.keyDown(wrap, { key: "End" });
      expect(onSelect).toHaveBeenLastCalledWith("p_0999");
      fireEvent.keyDown(wrap, { key: "Home" });
      expect(onSelect).toHaveBeenLastCalledWith("p_0000");
      // jsdom has no layout: a page is the fallback 40 rows less one.
      fireEvent.keyDown(wrap, { key: "PageDown" });
      expect(onSelect).toHaveBeenLastCalledWith("p_0539");
      fireEvent.keyDown(wrap, { key: "PageUp" });
      expect(onSelect).toHaveBeenLastCalledWith("p_0461");
      rerender(<PropTable props={many} total={1000} selected="p_0999" onSelect={onSelect} onOpen={() => {}} />);
      fireEvent.keyDown(wrap, { key: "PageDown" });
      expect(onSelect).toHaveBeenLastCalledWith("p_0999");
    });
  });
});
