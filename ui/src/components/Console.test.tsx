import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Console } from "./Console";

describe("Console", () => {
  it("submits on Enter, clears the input and ignores blank lines", async () => {
    const onSubmit = vi.fn();
    render(<Console hidden={false} log={[]} busy={false} flavor="jg" onSubmit={onSubmit} />);
    const input = screen.getByLabelText("Tcl command");
    await userEvent.type(input, "   {Enter}");
    expect(onSubmit).not.toHaveBeenCalled();
    await userEvent.clear(input);
    await userEvent.type(input, "prove -all{Enter}");
    expect(onSubmit).toHaveBeenCalledWith("prove -all");
    expect(input).toHaveValue("");
  });

  it("walks history with Up/Down, newest first", async () => {
    render(<Console hidden={false} log={[]} busy={false} flavor="jg" onSubmit={() => {}} />);
    const input = screen.getByLabelText("Tcl command");
    await userEvent.type(input, "one{Enter}two{Enter}");
    await userEvent.keyboard("{ArrowUp}");
    expect(input).toHaveValue("two");
    await userEvent.keyboard("{ArrowUp}");
    expect(input).toHaveValue("one");
    await userEvent.keyboard("{ArrowUp}");
    expect(input).toHaveValue("one");
    await userEvent.keyboard("{ArrowDown}");
    expect(input).toHaveValue("two");
    await userEvent.keyboard("{ArrowDown}");
    expect(input).toHaveValue("");
  });

  it("prompt follows the flavor, including on echoed input lines", () => {
    const log = [
      { kind: "in" as const, text: "check_fv" },
      { kind: "out" as const, text: "proven 2" },
      { kind: "err" as const, text: "oops" },
    ];
    const { rerender, container } = render(<Console hidden={false} log={log} busy={false} flavor="jg" onSubmit={() => {}} />);
    expect(container.querySelector(".prompt")).toHaveTextContent("fdv[jg]>");
    expect(screen.getByText("fdv[jg]> check_fv")).toHaveClass("in");
    rerender(<Console hidden={false} log={log} busy={false} flavor="vcf" onSubmit={() => {}} />);
    expect(container.querySelector(".prompt")).toHaveTextContent("fdv[vcf]>");
    expect(screen.getByText("fdv[vcf]> check_fv")).toBeInTheDocument();
    expect(screen.getByText("proven 2")).toHaveClass("out");
    expect(screen.getByText("oops")).toHaveClass("err");
  });

  it("disables input while busy and hides with the hidden attribute", () => {
    const { container } = render(<Console hidden={true} log={[]} busy={true} flavor="jg" onSubmit={() => {}} />);
    expect(container.querySelector(".console")).not.toBeVisible();
    expect(container.querySelector("input")).toBeDisabled();
  });
});
