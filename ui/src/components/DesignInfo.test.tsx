import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { DesignInfo } from "./DesignInfo";
import { mockDesignInfo } from "../mock";

describe("DesignInfo", () => {
  it("shows the design, setup and model counts it is given", () => {
    render(<DesignInfo info={mockDesignInfo()} error={null} loading={false} stale={false} onRefresh={() => {}} />);
    const design = screen.getByRole("region", { name: "Design" });
    expect(design).toHaveTextContent("Flops1 (4 bits)");
    expect(design).toHaveTextContent("Async resetsrst_n (low, 1)");
    expect(screen.getByRole("region", { name: "Model" })).toHaveTextContent("AIG ands84");
    expect(screen.getByRole("region", { name: "Setup" })).toHaveTextContent("Reset!rst_n");
  });

  it("says when it is out of date, and refreshes on request", async () => {
    const onRefresh = vi.fn();
    render(<DesignInfo info={mockDesignInfo()} error={null} loading={false} stale onRefresh={onRefresh} />);
    expect(screen.getByText(/out of date/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Refresh" }));
    expect(onRefresh).toHaveBeenCalled();
  });

  it("says why there is no model", () => {
    const info = { ...mockDesignInfo(), model: null, model_error: "svlang: not supported yet: x" };
    render(<DesignInfo info={info} error={null} loading={false} stale={false} />);
    expect(screen.getByRole("region", { name: "Model" })).toHaveTextContent("Not built: svlang: not supported yet: x");
  });
});
