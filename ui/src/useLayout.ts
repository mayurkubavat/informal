import { useCallback, useEffect, useState } from "react";
import type { BottomTab, Center, LeftTab } from "./model";

// Dock sizes and open tabs, kept across sessions in localStorage.
export const LAYOUT_KEY = "fdv-layout";
export const LEFT_MIN = 160;
export const LEFT_MAX = 480;
export const BOTTOM_MIN = 120;
export const BOTTOM_DEFAULT = 250;

export type Layout = {
  /** null = CSS default (236px, 200px on narrow windows) until the user drags. */
  leftW: number | null;
  bottomH: number;
  leftOpen: boolean;
  left: LeftTab;
  center: Center;
  bottom: BottomTab;
  /** The Profiler dock tab is shown (Tools > Proof profiler, Ctrl+5). */
  profiler: boolean;
};

export const DEFAULT_LAYOUT: Layout = { leftW: null, bottomH: BOTTOM_DEFAULT, leftOpen: true, left: "tasks", center: "table", bottom: "console", profiler: false };

export const clampLeft = (w: number) => Math.round(Math.max(LEFT_MIN, Math.min(LEFT_MAX, w)));
export const clampBottom = (h: number, viewH: number) => Math.round(Math.max(BOTTOM_MIN, Math.min(viewH * 0.65, h)));

const pick = <T extends string>(v: unknown, ok: readonly T[], d: T): T => (ok.includes(v as T) ? (v as T) : d);
const num = (v: unknown) => typeof v === "number" && Number.isFinite(v);

// Tolerates missing, stale or hand-edited values: anything invalid falls
// back to the default for that field.
export function parseLayout(raw: string | null, viewH: number): Layout {
  let o: Record<string, unknown> = {};
  try {
    const v = raw ? JSON.parse(raw) : {};
    if (v && typeof v === "object" && !Array.isArray(v)) o = v;
  } catch {
    /* corrupt: use defaults */
  }
  const d = DEFAULT_LAYOUT;
  return {
    leftW: num(o.leftW) ? clampLeft(o.leftW as number) : d.leftW,
    bottomH: num(o.bottomH) ? clampBottom(o.bottomH as number, viewH) : d.bottomH,
    leftOpen: typeof o.leftOpen === "boolean" ? o.leftOpen : d.leftOpen,
    left: pick(o.left, ["tasks", "design"] as const, d.left),
    center: pick(o.center, ["table", "source"] as const, d.center),
    bottom: o.profiler === true || o.bottom !== "profiler" ? pick(o.bottom, ["console", "details", "messages", "log", "profiler"] as const, d.bottom) : d.bottom,
    profiler: typeof o.profiler === "boolean" ? o.profiler : d.profiler,
  };
}

type Update<T> = T | ((prev: T) => T);

export function useLayout() {
  const [layout, setLayout] = useState<Layout>(() => {
    try {
      return parseLayout(localStorage.getItem(LAYOUT_KEY), window.innerHeight);
    } catch {
      return DEFAULT_LAYOUT;
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem(LAYOUT_KEY, JSON.stringify(layout));
    } catch {
      /* storage unavailable */
    }
  }, [layout]);
  const set = useCallback(
    <K extends keyof Layout>(k: K) =>
      (v: Update<Layout[K]>) =>
        setLayout((l) => ({ ...l, [k]: typeof v === "function" ? (v as (p: Layout[K]) => Layout[K])(l[k]) : v })),
    [],
  );
  return { layout, set };
}
