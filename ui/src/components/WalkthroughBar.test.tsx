import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { WalkthroughBar } from "./WalkthroughBar";

describe("WalkthroughBar", () => {
  it("steps through guided CEX tour and toggles Follow-AI state", () => {
    const onStep = vi.fn();
    const onToggleFollow = vi.fn();
    const onClose = vi.fn();

    render(
      <WalkthroughBar
        walkthrough={{
          title: "Counter failure",
          step: 0,
          steps: [
            { label: "Reset", note: "q starts at 0", frame: 0 },
            { label: "Fail", note: "q reaches 9", frame: 10 },
          ],
        }}
        followAgent={true}
        onStep={onStep}
        onToggleFollow={onToggleFollow}
        onClose={onClose}
      />,
    );

    expect(screen.getByText("1 / 2")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Previous walkthrough step" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Next walkthrough step" }));
    expect(onStep).toHaveBeenCalledWith(1, expect.objectContaining({ label: "Fail", frame: 10 }));

    fireEvent.click(screen.getByText(/Following AI/));
    expect(onToggleFollow).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole("button", { name: "Dismiss walkthrough" }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
