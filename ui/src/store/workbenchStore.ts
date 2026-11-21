// Fine-grained Zustand store slices for InFormal v2:
// - Per-property live progress map (`progressByProp`) so 5 Hz engine heartbeats
//   only notify cells whose status/depth/ms changed.
// - Live trace cursor value map (`activeTraceValues`) driving inline RTL values
//   in CodeMirror 6 (`SourceView`) and net values on `SchematicView`.
// - AI Co-Pilot state (`traceAnnotations`, `walkthrough`, `followAgent`, `focusChips`).
// - Multi-pane layout presets (`triage` | `cex-debug` | `signoff` | `split`),
//   Command Palette state, recent sessions, and `BroadcastChannel` pop-out sync.

import { create } from "zustand";
import type { PropProgress, ProveProgress } from "../backend";
import type { TraceAnnotation } from "../traceBin";

export type LayoutPreset = "triage" | "cex-debug" | "signoff" | "split";

export interface WalkthroughStep {
  label: string;
  note: string;
  property?: string;
  frame?: number;
  signals?: string[];
  file?: string;
  first_line?: number;
  last_line?: number;
}

export interface WalkthroughState {
  title: string;
  steps: WalkthroughStep[];
  step: number;
}

export interface FocusChip {
  id: string;
  label: string;
  detail: string;
}

export interface RecentSession {
  path: string;
  label: string;
  at: number;
}

export interface GuiCaptureSnapshot {
  target: "trace" | "schematic" | "window";
  format: "svg";
  svg: string;
  captured_at: string;
}

interface WorkbenchStore {
  // 5 Hz prove heartbeat slice
  progressByProp: Record<string, PropProgress>;
  setProveProgress: (p: ProveProgress | null) => void;

  // Workspace layout & pop-out
  layoutPreset: LayoutPreset;
  setLayoutPreset: (preset: LayoutPreset) => void;
  commandPaletteOpen: boolean;
  setCommandPaletteOpen: (open: boolean) => void;

  // Schematic state
  schematicProp: string | null;
  schematicDepth: number;
  setSchematicTarget: (prop: string | null, depth?: number) => void;

  // Live trace cursor signal values (`leaf` and `full.path` -> formatted value)
  activeTraceCycle: number | null;
  activeTraceValues: Record<string, string>;
  setActiveTraceValues: (cycle: number | null, values: Record<string, string>) => void;

  // AI Co-Pilot & Screen-Control v2 state
  traceAnnotations: TraceAnnotation[];
  setTraceAnnotations: (anns: TraceAnnotation[], append?: boolean) => void;
  sliceOnlyControlled: boolean | null;
  setSliceOnlyControlled: (v: boolean | null) => void;
  walkthrough: WalkthroughState | null;
  setWalkthrough: (w: WalkthroughState | null) => void;
  stepWalkthrough: (deltaOrIndex: number, absolute?: boolean) => WalkthroughStep | null;
  followAgent: boolean;
  agentActiveRecently: boolean;
  setFollowAgent: (follow: boolean) => void;
  markAgentDriven: () => void;
  detachFollowOnUserAction: () => void;

  // Reverse UI -> AI `@context` pills
  focusChips: FocusChip[];
  addFocusChip: (chip: Omit<FocusChip, "id">) => void;
  removeFocusChip: (id: string) => void;
  clearFocusChips: () => void;

  // Latest deterministic SVG view capture (`gui_capture_view`)
  lastCapture: GuiCaptureSnapshot | null;
  setLastCapture: (cap: GuiCaptureSnapshot | null) => void;

  // Recent standalone sessions
  recentSessions: RecentSession[];
  recordRecentSession: (path: string) => void;
}

const RECENT_KEY = "informal-recent";

function loadRecent(): RecentSession[] {
  try {
    if (typeof localStorage === "undefined" || typeof localStorage.getItem !== "function") return [];
    const raw = localStorage.getItem(RECENT_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.slice(0, 8);
  } catch {
    return [];
  }
}

function saveRecent(list: RecentSession[]): void {
  try {
    if (typeof localStorage === "undefined" || typeof localStorage.setItem !== "function") return;
    localStorage.setItem(RECENT_KEY, JSON.stringify(list.slice(0, 8)));
  } catch {
    // Ignored in sandboxed MCP App iframes or --no-webstorage test runners.
  }
}

const syncChannel: BroadcastChannel | null = (() => {
  try {
    return typeof BroadcastChannel !== "undefined" ? new BroadcastChannel("informal-sync") : null;
  } catch {
    return null;
  }
})();

export const useWorkbenchStore = create<WorkbenchStore>((set, get) => ({
  progressByProp: {},
  setProveProgress: (p) => {
    if (!p) {
      if (Object.keys(get().progressByProp).length > 0) set({ progressByProp: {} });
      return;
    }
    const prev = get().progressByProp;
    let changed = false;
    const next: Record<string, PropProgress> = {};
    for (const item of p.props) {
      const old = prev[item.name];
      if (
        old &&
        old.status === item.status &&
        old.depth === item.depth &&
        old.ms === item.ms &&
        old.engine === item.engine
      ) {
        next[item.name] = old;
      } else {
        next[item.name] = item;
        changed = true;
      }
    }
    if (changed || Object.keys(prev).length !== p.props.length) {
      set({ progressByProp: next });
    }
  },

  layoutPreset: "triage",
  setLayoutPreset: (layoutPreset) => set({ layoutPreset }),
  commandPaletteOpen: false,
  setCommandPaletteOpen: (commandPaletteOpen) => set({ commandPaletteOpen }),

  schematicProp: null,
  schematicDepth: 2,
  setSchematicTarget: (prop, depth) =>
    set((st) => ({
      schematicProp: prop,
      schematicDepth: depth ?? st.schematicDepth,
    })),

  activeTraceCycle: null,
  activeTraceValues: {},
  setActiveTraceValues: (cycle, values) => {
    set({ activeTraceCycle: cycle, activeTraceValues: values });
    try {
      syncChannel?.postMessage({ type: "trace-cursor", cycle, values });
    } catch {
      // ignore
    }
  },

  traceAnnotations: [],
  setTraceAnnotations: (anns, append = false) =>
    set((st) => ({
      traceAnnotations: append ? [...st.traceAnnotations, ...anns] : anns,
    })),
  sliceOnlyControlled: null,
  setSliceOnlyControlled: (sliceOnlyControlled) => set({ sliceOnlyControlled }),

  walkthrough: null,
  setWalkthrough: (walkthrough) => set({ walkthrough }),
  stepWalkthrough: (deltaOrIndex, absolute = false) => {
    const w = get().walkthrough;
    if (!w || !w.steps.length) return null;
    const nextIdx = absolute
      ? Math.max(0, Math.min(w.steps.length - 1, deltaOrIndex))
      : Math.max(0, Math.min(w.steps.length - 1, w.step + deltaOrIndex));
    set({ walkthrough: { ...w, step: nextIdx } });
    return w.steps[nextIdx] ?? null;
  },

  followAgent: true,
  agentActiveRecently: false,
  setFollowAgent: (followAgent) => set({ followAgent }),
  markAgentDriven: () => set({ agentActiveRecently: true }),
  detachFollowOnUserAction: () => {
    const st = get();
    if (st.agentActiveRecently && st.followAgent && st.walkthrough) {
      set({ followAgent: false });
    }
  },

  focusChips: [],
  addFocusChip: (chip) =>
    set((st) => {
      const exists = st.focusChips.some((c) => c.label === chip.label);
      if (exists) return st;
      const next = [...st.focusChips, { ...chip, id: `${Date.now()}-${st.focusChips.length}` }];
      return { focusChips: next };
    }),
  removeFocusChip: (id) =>
    set((st) => ({ focusChips: st.focusChips.filter((c) => c.id !== id) })),
  clearFocusChips: () => set({ focusChips: [] }),

  lastCapture: null,
  setLastCapture: (lastCapture) => set({ lastCapture }),

  recentSessions: loadRecent(),
  recordRecentSession: (path) => {
    const trimmed = path.trim();
    if (!trimmed) return;
    const label = trimmed.split("/").slice(-2).join("/");
    const filtered = get().recentSessions.filter((r) => r.path !== trimmed);
    const next = [{ path: trimmed, label, at: Date.now() }, ...filtered].slice(0, 8);
    saveRecent(next);
    set({ recentSessions: next });
  },
}));

// Listen to cross-window cursor sync messages in popped-out secondary windows
if (syncChannel) {
  syncChannel.onmessage = (ev: MessageEvent) => {
    const data = ev.data as { type?: string; cycle?: number | null; values?: Record<string, string> } | null;
    if (data?.type === "trace-cursor") {
      useWorkbenchStore.setState({
        activeTraceCycle: data.cycle ?? null,
        activeTraceValues: data.values ?? {},
      });
    }
  };
}
