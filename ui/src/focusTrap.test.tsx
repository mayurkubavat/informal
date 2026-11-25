import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { getTabbables, trapFocusKeyDown } from "./focusTrap";

describe("focusTrap", () => {
  it("cycles Tab and Shift+Tab strictly across visible enabled tabbable controls", () => {
    render(
      <div>
        <button>Outside before</button>
        <div data-testid="modal" tabIndex={-1}>
          <button>First</button>
          <button disabled>Disabled</button>
          <button hidden>Hidden</button>
          <div aria-hidden="true">
            <button>AriaHidden</button>
          </div>
          <input aria-label="Middle" />
          <button>Last</button>
        </div>
        <button>Outside after</button>
      </div>,
    );

    const modal = screen.getByTestId("modal");
    const first = screen.getByRole("button", { name: "First" });
    const middle = screen.getByLabelText("Middle");
    const last = screen.getByRole("button", { name: "Last" });

    expect(getTabbables(modal)).toEqual([first, middle, last]);

    first.focus();
    const mk = (shiftKey = false) => {
      let prevented = false;
      return {
        ev: { key: "Tab", shiftKey, preventDefault: () => { prevented = true; } },
        wasPrevented: () => prevented,
      };
    };

    const t1 = mk(false);
    expect(trapFocusKeyDown(modal, t1.ev)).toBe(true);
    expect(t1.wasPrevented()).toBe(true);
    expect(middle).toHaveFocus();

    const t2 = mk(false);
    trapFocusKeyDown(modal, t2.ev);
    expect(last).toHaveFocus();

    // Wraps from last back to first (never leaks to Outside after)
    const t3 = mk(false);
    trapFocusKeyDown(modal, t3.ev);
    expect(first).toHaveFocus();

    // Shift+Tab wraps from first back to last
    const t4 = mk(true);
    trapFocusKeyDown(modal, t4.ev);
    expect(last).toHaveFocus();

    // If focus somehow drifted outside, Tab pulls it back into modal
    screen.getByRole("button", { name: "Outside before" }).focus();
    const t5 = mk(false);
    trapFocusKeyDown(modal, t5.ev);
    expect(first).toHaveFocus();
  });

  it("keeps focus pinned on single-control modals on both Tab and Shift+Tab", () => {
    render(
      <div>
        <button>Outside</button>
        <div data-testid="single">
          <input aria-label="Only control" />
        </div>
      </div>,
    );
    const modal = screen.getByTestId("single");
    const only = screen.getByLabelText("Only control");
    only.focus();

    let prevented = 0;
    trapFocusKeyDown(modal, { key: "Tab", shiftKey: false, preventDefault: () => { prevented++; } });
    expect(only).toHaveFocus();
    trapFocusKeyDown(modal, { key: "Tab", shiftKey: true, preventDefault: () => { prevented++; } });
    expect(only).toHaveFocus();
    expect(prevented).toBe(2);
  });
});
