import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";
import { Glyph, Status } from "./Glyph";
import { prop } from "../test/fixtures";

const keys = ["proven", "cex", "covered", "unreachable", "undetermined", "error", "assume", "unprocessed", "running"];

describe("Glyph", () => {
  it.each(keys)("renders exactly one hidden svg for %s", (k) => {
    const { container } = render(<Glyph k={k} />);
    const svgs = container.querySelectorAll("svg");
    expect(svgs).toHaveLength(1);
    expect(svgs[0]).toHaveClass("glyph", `st-${k}`);
    expect(svgs[0]).toHaveAttribute("aria-hidden", "true");
  });

  it("draws a distinct shape per status", () => {
    const shapes = keys.map((k) => render(<Glyph k={k} />).container.querySelector("svg")!.innerHTML);
    expect(new Set(shapes).size).toBe(keys.length);
  });

  it("draws running as a dashed ring", () => {
    const svg = render(<Glyph k="running" />).container.querySelector("svg")!;
    expect(svg.querySelector("circle")).toHaveAttribute("stroke-dasharray");
  });

  it("Status pairs the glyph with the label", () => {
    const { container } = render(<Status p={prop("p", {}, { status: "cex" })} />);
    expect(container).toHaveTextContent("CEX");
    expect(container.querySelector("svg")).toHaveClass("st-cex");
  });
});
