import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { LEAF_CAP, TaskTree } from "./TaskTree";
import { prop } from "../test/fixtures";

const items = () => screen.getAllByRole("treeitem").map((r) => r.textContent);

describe("TaskTree", () => {
  it("shows the task total and only non-empty groups, in summary order", () => {
    render(<TaskTree total={6} counts={{ cex: 1, proven: 2, unprocessed: 0 }} assumes={1} filter={null} onFilter={() => {}} />);
    expect(items()).toEqual(["<embedded>6", "Proven2", "CEX1", "Assumed1"]);
  });

  it("puts a Running group first while a prove runs", () => {
    const props = [prop("a", {}, { status: "running" } as never), prop("b", {}, { status: "proven" })];
    render(<TaskTree total={2} counts={{ running: 1, proven: 1 }} assumes={0} filter={null} onFilter={() => {}} props={props} />);
    expect(items()).toEqual(["<embedded>2", "Running1", "Aa", "Proven1", "Ab"]);
  });

  it("caps leaves per group, keeps the selected one, and 'more' filters", async () => {
    const props = Array.from({ length: LEAF_CAP + 50 }, (_, i) => prop(`p${i}`));
    const onFilter = vi.fn();
    render(<TaskTree total={props.length} counts={{ unprocessed: props.length }} assumes={0} filter={null} onFilter={onFilter} props={props} selected="p240" />);
    const leaves = screen.getAllByRole("treeitem").filter((r) => r.classList.contains("leaf") && !r.classList.contains("more"));
    expect(leaves).toHaveLength(LEAF_CAP + 1);
    expect(leaves.at(-1)).toHaveTextContent("p240");
    await userEvent.click(screen.getByText("49 more…"));
    expect(onFilter).toHaveBeenCalledWith("unprocessed");
  });

  it("lists unprocessed properties before assumes", () => {
    render(<TaskTree total={6} counts={{ unprocessed: 5 }} assumes={1} filter={null} onFilter={() => {}} />);
    expect(items()).toEqual(["<embedded>6", "Unprocessed5", "Assumed1"]);
  });

  it("toggles a status filter and clears it from the root", async () => {
    const onFilter = vi.fn();
    const { rerender } = render(<TaskTree total={3} counts={{ cex: 1, proven: 2 }} assumes={0} filter={null} onFilter={onFilter} />);
    await userEvent.click(screen.getByText("CEX"));
    expect(onFilter).toHaveBeenLastCalledWith("cex");
    rerender(<TaskTree total={3} counts={{ cex: 1, proven: 2 }} assumes={0} filter="cex" onFilter={onFilter} />);
    expect(screen.getByText("CEX").closest(".tree-row")).toHaveClass("sel");
    await userEvent.click(screen.getByText("CEX"));
    expect(onFilter).toHaveBeenLastCalledWith(null);
    await userEvent.click(screen.getByText("<embedded>"));
    expect(onFilter).toHaveBeenLastCalledWith(null);
  });

  it("collapses with the twisty without changing the filter", async () => {
    const onFilter = vi.fn();
    render(<TaskTree total={3} counts={{ proven: 3 }} assumes={0} filter={null} onFilter={onFilter} />);
    await userEvent.click(screen.getByRole("button", { name: "Expand" }));
    expect(items()).toEqual(["<embedded>3"]);
    expect(screen.getAllByRole("treeitem")[0]).toHaveAttribute("aria-expanded", "false");
    expect(onFilter).not.toHaveBeenCalled();
  });

  describe("property leaves", () => {
    const props = [
      prop("a_max", {}, { status: "proven" }),
      prop("a_never9", {}, { status: "cex" }),
      prop("a_step", {}, { status: "proven" }),
      prop("m_en", { kind: "assume" }),
    ];
    const tree = (over = {}) => {
      const onSelect = vi.fn();
      const onOpen = vi.fn();
      const onFilter = vi.fn();
      render(<TaskTree total={4} counts={{ proven: 2, cex: 1 }} assumes={1} filter={null} onFilter={onFilter} props={props} selected={null} onSelect={onSelect} onOpen={onOpen} {...over} />);
      return { onSelect, onOpen, onFilter };
    };

    it("lists each property under its status group", () => {
      tree();
      expect(items()).toEqual(["<embedded>4", "Proven2", "Aa_max", "Aa_step", "CEX1", "Aa_never9", "Assumed1", "Mm_en"]);
    });

    it("selects on click and opens on double-click without filtering", async () => {
      const { onSelect, onOpen, onFilter } = tree();
      await userEvent.click(screen.getByText("a_never9"));
      expect(onSelect).toHaveBeenLastCalledWith("a_never9");
      await userEvent.dblClick(screen.getByText("a_step"));
      expect(onOpen).toHaveBeenCalledWith("a_step");
      expect(onFilter).not.toHaveBeenCalled();
    });

    it("marks the selected property", () => {
      tree({ selected: "a_step" });
      const leaf = screen.getByText("a_step").closest(".tree-row")!;
      expect(leaf).toHaveClass("sel");
      expect(leaf).toHaveAttribute("aria-selected", "true");
    });

    it("collapses one group with its own twisty", async () => {
      tree();
      await userEvent.click(screen.getByRole("button", { name: "Collapse Proven" }));
      expect(items()).toEqual(["<embedded>4", "Proven2", "CEX1", "Aa_never9", "Assumed1", "Mm_en"]);
      await userEvent.click(screen.getByRole("button", { name: "Expand Proven" }));
      expect(screen.getByText("a_max")).toBeInTheDocument();
    });
  });
});
