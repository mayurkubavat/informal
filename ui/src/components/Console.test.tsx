import { describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
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

  describe("Tab completion", () => {
    const names = ["prove", "a_max", "a_never9", "a_step"];
    const complete = vi.fn(async (p: string) => names.filter((n) => n.startsWith(p)));
    const setup = () => {
      complete.mockClear();
      render(<Console hidden={false} log={[]} busy={false} flavor="jg" onSubmit={() => {}} complete={complete} />);
      return screen.getByLabelText("Tcl command");
    };

    it("completes a single match with a trailing space", async () => {
      const input = setup();
      await userEvent.type(input, "pro");
      await userEvent.keyboard("{Tab}");
      await waitFor(() => expect(input).toHaveValue("prove "));
      expect(complete).toHaveBeenCalledWith("pro");
      expect(screen.queryByRole("listbox")).toBeNull();
      expect(input).toHaveFocus();
    });

    it("lists several matches and cycles with Tab / Shift+Tab", async () => {
      const input = setup();
      await userEvent.type(input, "prove -property a_");
      await userEvent.keyboard("{Tab}");
      await waitFor(() => expect(input).toHaveValue("prove -property a_max"));
      const list = screen.getByRole("listbox", { name: "Completions" });
      expect(list).toHaveTextContent("a_maxa_never9a_step");
      expect(screen.getByRole("option", { selected: true })).toHaveTextContent("a_max");
      await userEvent.keyboard("{Tab}");
      expect(input).toHaveValue("prove -property a_never9");
      expect(screen.getByRole("option", { selected: true })).toHaveTextContent("a_never9");
      await userEvent.keyboard("{Tab}{Tab}");
      expect(input).toHaveValue("prove -property a_max");
      await userEvent.keyboard("{Shift>}{Tab}{/Shift}");
      expect(input).toHaveValue("prove -property a_step");
      // The backend is asked once; cycling is local.
      expect(complete).toHaveBeenCalledTimes(1);
      expect(input).toHaveFocus();
    });

    it("typing or Esc closes the list", async () => {
      const input = setup();
      await userEvent.type(input, "a_");
      await userEvent.keyboard("{Tab}");
      await screen.findByRole("listbox");
      await userEvent.keyboard("{Escape}");
      expect(screen.queryByRole("listbox")).toBeNull();
      expect(input).toHaveValue("a_max");
      await userEvent.clear(input);
      await userEvent.type(input, "a_");
      await userEvent.keyboard("{Tab}");
      await screen.findByRole("listbox");
      await userEvent.type(input, "x");
      expect(screen.queryByRole("listbox")).toBeNull();
      expect(input).toHaveValue("a_maxx");
    });

    it("leaves Tab alone when there is no word to complete", async () => {
      const input = setup();
      await userEvent.click(input);
      await userEvent.keyboard("{Tab}");
      expect(complete).not.toHaveBeenCalled();
      expect(input).not.toHaveFocus();
    });
  });
});
