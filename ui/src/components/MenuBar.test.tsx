import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MenuBar, type MenuDef } from "./MenuBar";

function setup() {
  const open = vi.fn();
  const close = vi.fn();
  const menus: MenuDef[] = [
    { label: "File", items: [{ label: "Open", onClick: open, hint: "o" }, { sep: true }, { label: "Close design", onClick: close, disabled: true }] },
    { label: "View", items: [{ label: "Dark", checked: true }] },
  ];
  render(
    <>
      <MenuBar menus={menus}>
        <input aria-label="extra" />
      </MenuBar>
      <p>outside</p>
    </>,
  );
  return { open, close };
}
const title = (n: string) => screen.getByRole("menuitem", { name: new RegExp(`^${n}$`) });

describe("MenuBar", () => {
  it("opens a menu on click and toggles it closed", async () => {
    setup();
    expect(screen.queryByRole("menu")).toBeNull();
    await userEvent.click(title("File"));
    expect(screen.getByRole("menu")).toBeInTheDocument();
    expect(title("File")).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText("o")).toHaveClass("menu-hint");
    await userEvent.click(title("File"));
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("closes on Escape", async () => {
    setup();
    await userEvent.click(title("File"));
    await userEvent.keyboard("{Escape}");
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("closes on an outside click but not on a click inside the bar", async () => {
    setup();
    await userEvent.click(title("File"));
    fireEvent.mouseDown(screen.getByLabelText("extra"));
    expect(screen.getByRole("menu")).toBeInTheDocument();
    fireEvent.mouseDown(screen.getByText("outside"));
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("runs an item and closes; disabled items do nothing", async () => {
    const { open, close } = setup();
    await userEvent.click(title("File"));
    expect(screen.getByRole("menuitem", { name: /Close design/ })).toBeDisabled();
    await userEvent.click(screen.getByRole("menuitem", { name: /Close design/ }));
    expect(close).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("menuitem", { name: /Open/ }));
    expect(open).toHaveBeenCalledOnce();
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("follows the pointer to a neighbouring menu once one is open, and shows checks", async () => {
    setup();
    await userEvent.hover(title("View"));
    expect(screen.queryByRole("menu")).toBeNull();
    await userEvent.click(title("File"));
    await userEvent.hover(title("View"));
    expect(screen.getByRole("menuitem", { name: /Dark/ })).toHaveTextContent("✓Dark");
  });

  it("keeps extra controls out of the menubar role", () => {
    setup();
    expect(screen.getByRole("menubar")).not.toContainElement(screen.getByLabelText("extra"));
  });
});
