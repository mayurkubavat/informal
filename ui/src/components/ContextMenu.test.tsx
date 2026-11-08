import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ContextMenu } from "./ContextMenu";
import type { MenuItem } from "./MenuBar";

function setup() {
  const onClose = vi.fn();
  const prove = vi.fn();
  const items: MenuItem[] = [
    { label: "Prove", hint: "prove -property p", onClick: prove },
    { label: "Visualize trace", disabled: true, title: "Coming soon" },
    { sep: true },
    { label: "Copy name", onClick: () => {} },
  ];
  render(
    <>
      <button>before</button>
      <ContextMenu x={10} y={20} items={items} label="Actions for p" onClose={onClose} />
    </>,
  );
  return { onClose, prove };
}

describe("ContextMenu", () => {
  it("renders a labelled menu with items, hints and separators", () => {
    setup();
    expect(screen.getByRole("menu", { name: "Actions for p" })).toBeInTheDocument();
    expect(screen.getAllByRole("menuitem").map((b) => b.textContent)).toEqual(["Proveprove -property p", "Visualize trace", "Copy name"]);
    expect(screen.getByRole("separator")).toBeInTheDocument();
  });

  it("focuses the first item and wraps with arrow keys", async () => {
    setup();
    const [first, second, third] = screen.getAllByRole("menuitem");
    expect(first).toHaveFocus();
    await userEvent.keyboard("{ArrowDown}");
    expect(second).toHaveFocus();
    await userEvent.keyboard("{End}");
    expect(third).toHaveFocus();
    await userEvent.keyboard("{ArrowDown}");
    expect(first).toHaveFocus();
    await userEvent.keyboard("{ArrowUp}");
    expect(third).toHaveFocus();
  });

  it("runs an item then closes", async () => {
    const { onClose, prove } = setup();
    await userEvent.click(screen.getByRole("menuitem", { name: /Prove/ }));
    expect(prove).toHaveBeenCalledOnce();
    expect(onClose).toHaveBeenCalled();
  });

  it("keeps disabled items focusable with a tooltip but inert", async () => {
    const { onClose } = setup();
    const vis = screen.getByRole("menuitem", { name: "Visualize trace" });
    expect(vis).toHaveAttribute("aria-disabled", "true");
    expect(vis).toHaveAttribute("title", "Coming soon");
    await userEvent.click(vis);
    expect(onClose).not.toHaveBeenCalled();
  });

  it("closes on Escape and on an outside mousedown", () => {
    const { onClose } = setup();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
    fireEvent.mouseDown(screen.getByRole("menu"));
    expect(onClose).toHaveBeenCalledTimes(1);
    fireEvent.mouseDown(screen.getByText("before"));
    expect(onClose).toHaveBeenCalledTimes(2);
  });
});
