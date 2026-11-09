import { afterEach, describe, expect, it } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { useTheme } from "./useTheme";

const root = () => document.documentElement;

describe("useTheme", () => {
  afterEach(() => {
    localStorage.clear();
    root().removeAttribute("data-theme");
  });

  it("defaults to system and leaves data-theme unset", () => {
    const { result } = renderHook(() => useTheme());
    expect(result.current[0]).toBe("system");
    expect(root()).not.toHaveAttribute("data-theme");
  });

  it("pins light/dark on <html> and persists the choice", () => {
    const { result } = renderHook(() => useTheme());
    act(() => result.current[1]("dark"));
    expect(root()).toHaveAttribute("data-theme", "dark");
    expect(localStorage.getItem("informal-theme")).toBe("dark");
    act(() => result.current[1]("system"));
    expect(root()).not.toHaveAttribute("data-theme");
    expect(localStorage.getItem("informal-theme")).toBe("system");
  });

  it("restores a stored theme", () => {
    localStorage.setItem("informal-theme", "light");
    const { result } = renderHook(() => useTheme());
    expect(result.current[0]).toBe("light");
    expect(root()).toHaveAttribute("data-theme", "light");
  });
});
