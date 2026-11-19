import { afterEach, describe, expect, it } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { clampBottom, clampLeft, DEFAULT_LAYOUT, LAYOUT_KEY, parseLayout, useLayout } from "./useLayout";

describe("clamps", () => {
  it("keeps the left dock within 160-480px", () => {
    expect(clampLeft(10)).toBe(160);
    expect(clampLeft(300.4)).toBe(300);
    expect(clampLeft(9999)).toBe(480);
  });
  it("keeps the bottom dock between 120px and 65% of the window", () => {
    expect(clampBottom(50, 900)).toBe(120);
    expect(clampBottom(400, 900)).toBe(400);
    expect(clampBottom(800, 900)).toBe(585);
  });
});

describe("parseLayout", () => {
  it("falls back to defaults for missing or corrupt data", () => {
    expect(parseLayout(null, 900)).toEqual(DEFAULT_LAYOUT);
    expect(parseLayout("{not json", 900)).toEqual(DEFAULT_LAYOUT);
    expect(parseLayout("[1,2]", 900)).toEqual(DEFAULT_LAYOUT);
    expect(parseLayout('"x"', 900)).toEqual(DEFAULT_LAYOUT);
  });
  it("restores valid fields, clamping sizes", () => {
    const raw = JSON.stringify({ leftW: 900, bottomH: 50, leftOpen: false, left: "design", center: "source", bottom: "messages", profiler: false, hidden: DEFAULT_LAYOUT.hidden });
    expect(parseLayout(raw, 900)).toEqual({ leftW: 480, bottomH: 120, leftOpen: false, left: "design", center: "source", bottom: "messages", profiler: false, hidden: DEFAULT_LAYOUT.hidden });
  });
  it("drops invalid fields one by one", () => {
    const raw = JSON.stringify({ leftW: "wide", bottomH: null, leftOpen: "yes", left: "files", center: 3, bottom: "details" });
    expect(parseLayout(raw, 900)).toEqual({ ...DEFAULT_LAYOUT, bottom: "details" });
  });
});

describe("useLayout", () => {
  afterEach(() => localStorage.clear());

  it("persists changes under informal-layout", () => {
    const { result } = renderHook(() => useLayout());
    expect(result.current.layout).toEqual(DEFAULT_LAYOUT);
    act(() => result.current.set("leftW")(300));
    act(() => result.current.set("bottom")("messages"));
    act(() => result.current.set("leftOpen")((o) => !o));
    expect(JSON.parse(localStorage.getItem(LAYOUT_KEY)!)).toMatchObject({ leftW: 300, bottom: "messages", leftOpen: false });
  });

  it("starts from the stored layout", () => {
    localStorage.setItem(LAYOUT_KEY, JSON.stringify({ left: "design", bottomH: 300 }));
    const { result } = renderHook(() => useLayout());
    expect(result.current.layout).toMatchObject({ left: "design", bottomH: 300, leftW: null });
  });
});
