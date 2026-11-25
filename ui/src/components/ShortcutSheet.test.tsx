import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { ShortcutSheet } from "./ShortcutSheet";

describe("ShortcutSheet", () => {
  it("traps Tab / Shift+Tab inside the modal and restores focus on close", () => {
    const onClose = vi.fn();
    const { rerender } = render(
      <div>
        <button>Open Help</button>
        <ShortcutSheet open={false} onClose={onClose} />
        <button>Outside</button>
      </div>,
    );
    const trigger = screen.getByRole("button", { name: "Open Help" });
    trigger.focus();

    rerender(
      <div>
        <button>Open Help</button>
        <ShortcutSheet open={true} onClose={onClose} />
        <button>Outside</button>
      </div>,
    );
    const closeBtn = screen.getByRole("button", { name: "Close keyboard and workflow guide" });
    expect(closeBtn).toHaveFocus();

    fireEvent.keyDown(window, { key: "Tab" });
    expect(closeBtn).toHaveFocus();
    fireEvent.keyDown(window, { key: "Tab", shiftKey: true });
    expect(closeBtn).toHaveFocus();

    fireEvent.keyDown(window, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);

    rerender(
      <div>
        <button>Open Help</button>
        <ShortcutSheet open={false} onClose={onClose} />
        <button>Outside</button>
      </div>,
    );
    expect(trigger).toHaveFocus();
  });
});
