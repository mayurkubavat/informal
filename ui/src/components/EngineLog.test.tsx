import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { EngineLog } from "./EngineLog";

const lines = [
  { prop: "a_max", engine: "ric3-ic3", text: "reading fdv_work/a_max.btor2", err: false },
  { prop: "a_max", engine: "ric3-ic3", text: "warning: x", err: true },
];

describe("EngineLog", () => {
  it("shows tagged lines, stderr dimmed by class, and a count", () => {
    render(<EngineLog hidden={false} lines={lines} dropped={3} autoscroll onAutoscroll={() => {}} onClear={() => {}} />);
    const log = screen.getByRole("log", { name: "Engine output" });
    expect(log).toHaveTextContent("a_max ric3-ic3reading fdv_work/a_max.btor2");
    expect(screen.getByText("warning: x")).toHaveClass("stderr");
    expect(screen.getByText("2 lines (3 older dropped)")).toBeInTheDocument();
  });

  it("Clear and the autoscroll toggle call back", async () => {
    const onClear = vi.fn();
    const onAuto = vi.fn();
    render(<EngineLog hidden={false} lines={lines} dropped={0} autoscroll onAutoscroll={onAuto} onClear={onClear} />);
    await userEvent.click(screen.getByRole("button", { name: "Clear" }));
    expect(onClear).toHaveBeenCalled();
    await userEvent.click(screen.getByRole("checkbox", { name: "Autoscroll" }));
    expect(onAuto).toHaveBeenCalledWith(false);
  });

  it("explains itself when empty; Clear is disabled", () => {
    render(<EngineLog hidden={false} lines={[]} dropped={0} autoscroll={false} onAutoscroll={() => {}} onClear={() => {}} />);
    expect(screen.getByText(/No engine output yet/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Clear" })).toBeDisabled();
  });
});
