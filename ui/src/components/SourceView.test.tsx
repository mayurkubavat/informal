import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";
import { SourceView } from "./SourceView";

const text = ["one", "two", "three", "four", "five"].join("\n");
const marked = () => [...document.querySelectorAll(".source div.focus")].map((d) => d.textContent);

describe("SourceView", () => {
  it("highlights the one line in focus", () => {
    render(<SourceView text={text} focus={2} />);
    expect(marked()).toEqual(["2two"]);
  });
  it("highlights a range of lines", () => {
    render(<SourceView text={text} focus={2} focusEnd={4} />);
    expect(marked()).toEqual(["2two", "3three", "4four"]);
  });
  it("highlights nothing without a line", () => {
    render(<SourceView text={text} focus={0} />);
    expect(marked()).toEqual([]);
  });
});
