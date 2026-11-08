import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TaskTree } from "./TaskTree";

const items = () => screen.getAllByRole("treeitem").map((r) => r.textContent);

describe("TaskTree", () => {
  it("shows the task total and only non-empty groups, in summary order", () => {
    render(<TaskTree total={6} counts={{ cex: 1, proven: 2, unprocessed: 0 }} assumes={1} filter={null} onFilter={() => {}} />);
    expect(items()).toEqual(["<embedded>6", "Proven2", "CEX1", "Assumed1"]);
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
});
