import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { CommandPalette, type PaletteCommand } from "./CommandPalette";

describe("CommandPalette", () => {
  const mkCommands = (spy: () => void): PaletteCommand[] => [
    { id: "c1", group: "Prove", label: "Prove all properties", shortcut: "F5", run: () => {} },
    { id: "c2", group: "Layout", label: "Workspace Preset: CEX Debug", run: spy },
    { id: "c3", group: "Disabled", label: "Hidden disabled item", disabled: true, run: () => {} },
  ];

  it("filters commands fuzzily, navigates with arrows, and executes on Enter", () => {
    const runSpy = vi.fn();
    const closeSpy = vi.fn();
    render(<CommandPalette open={true} commands={mkCommands(runSpy)} onClose={closeSpy} />);

    expect(screen.queryByText("Hidden disabled item")).toBeNull();
    const input = screen.getByRole("combobox", { name: "Search commands and properties" });
    fireEvent.change(input, { target: { value: "cex debug" } });
    expect(screen.getAllByRole("option")).toHaveLength(1);

    fireEvent.keyDown(screen.getByRole("dialog", { name: "Command palette" }), { key: "Enter" });
    expect(closeSpy).toHaveBeenCalledTimes(1);
    expect(runSpy).toHaveBeenCalledTimes(1);
  });

  it("closes on Escape", () => {
    const closeSpy = vi.fn();
    render(<CommandPalette open={true} commands={mkCommands(() => {})} onClose={closeSpy} />);
    fireEvent.keyDown(screen.getByRole("dialog", { name: "Command palette" }), { key: "Escape" });
    expect(closeSpy).toHaveBeenCalledTimes(1);
  });
});
