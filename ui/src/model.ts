// Pure view-model helpers shared by the workbench components: status words,
// table cell text, sort keys, and light source scanning. No React here.

import type { PropStatus, Property, ProveProgress, StatusWord } from "./backend";

// What a row shows: the backend's status words plus "running", which only
// exists in the view while a prove's progress events say so.
export type ViewStatus = StatusWord | "running";
// `provisional`: known only from a running prove's progress (the script that
// added it has not returned yet); the session replaces it when it does.
export type ViewProp = Omit<Property, "status"> & {
  status: Omit<PropStatus, "status"> & { status: ViewStatus };
  provisional?: boolean;
};

export type LogLine = { kind: "in" | "out" | "err"; text: string };
export type LogEvent = { at: Date; cmd: string; ok: boolean; ms: number; summary: string };
export type Theme = "system" | "light" | "dark";
export type Center = "table" | "source";
export type BottomTab = "console" | "details" | "messages";
export type LeftTab = "tasks" | "design";
export type SortKey = "kind" | "name" | "status" | "engine" | "bound" | "time";
export type StatusFilter = ViewStatus | "assume" | null;

export const STATUS_LABEL: Record<ViewStatus, string> = {
  running: "Running",
  unprocessed: "Unprocessed",
  proven: "Proven",
  cex: "CEX",
  covered: "Covered",
  unreachable: "Unreachable",
  undetermined: "Undetermined",
  error: "Error",
};
export const SUMMARY: StatusWord[] = ["proven", "cex", "covered", "unreachable", "undetermined", "error"];
export const STATUS_ORDER: ViewStatus[] = ["error", "cex", "running", "undetermined", "covered", "unreachable", "proven", "unprocessed"];
export const KIND_LETTER = { assert: "A", cover: "C", assume: "M" } as const;
export const TASK = "<embedded>";

export function bound(p: ViewProp): string {
  const s = p.status;
  switch (s.status) {
    case "running":
      return `k=${s.depth ?? 0}`;
    case "cex":
    case "covered":
      return String(s.depth ?? "?");
    case "proven":
    case "unreachable":
      return "∞";
    case "undetermined":
      return s.reason ?? "";
    case "error":
      return (s.message ?? "").split("\n")[0];
    default:
      return "";
  }
}
// Elapsed time as m:ss (running rows, the progress readout).
export function clock(ms: number): string {
  const t = Math.floor(Math.max(0, ms) / 1000);
  return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, "0")}`;
}
export function time(p: ViewProp): string {
  if (p.status.status === "running") return clock(p.status.ms ?? 0);
  return p.status.ms !== undefined ? `${(p.status.ms / 1000).toFixed(2)}s` : "";
}
export function statusKey(p: ViewProp): string {
  if (p.kind === "assume") return "assume";
  return p.enabled ? p.status.status : "unprocessed";
}
// Text filter: case-insensitive substring of name, expression or engine.
export function matches(p: ViewProp, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return [p.name, p.expr, p.status.engine ?? ""].some((s) => s.toLowerCase().includes(q));
}

// Task-tree group: assumes together, everything else by raw status.
export function groupOf(p: ViewProp): ViewStatus | "assume" {
  return p.kind === "assume" ? "assume" : p.status.status;
}
export function statusText(p: ViewProp): string {
  if (p.kind === "assume") return "Assumed";
  return p.enabled ? STATUS_LABEL[p.status.status] : "Disabled";
}

export function sortValue(p: ViewProp, k: SortKey): string | number {
  switch (k) {
    case "kind": return p.kind;
    case "name": return p.name;
    case "status": return p.kind === "assume" ? 99 : STATUS_ORDER.indexOf(p.status.status);
    case "engine": return p.status.engine ?? "";
    case "bound": return p.status.depth ?? (p.status.status === "proven" || p.status.status === "unreachable" ? 1e9 : -1);
    case "time": return p.status.ms ?? -1;
  }
}

// Kind of a property seen only in progress events from an older backend
// that does not send `kind`: by name prefix, assert when in doubt.
export const guessKind = (name: string): "assert" | "cover" => (/^(c_|cover)/i.test(name) ? "cover" : "assert");

// Overlay a prove's progress on the session's properties: properties in the
// run show their live status (queued, running with k-bound and elapsed, or
// final); the rest keep the session's status. Run members the session does
// not have yet (a sourced script that adds properties and proves them in one
// call) are appended as provisional rows until its reply brings the session.
export function withProgress(props: Property[], prog: ProveProgress | null): ViewProp[] {
  if (!prog) return props;
  const live = new Map(prog.props.map((q) => [q.name, q]));
  const statusOf = ({ name: _, kind: __, ...status }: ProveProgress["props"][number]) => status;
  const known = new Set(props.map((p) => p.name));
  const extra: ViewProp[] = prog.props
    .filter((q) => !known.has(q.name))
    .map((q) => ({ name: q.name, kind: q.kind ?? guessKind(q.name), expr: "", enabled: true, status: statusOf(q), provisional: true }));
  const merged: ViewProp[] = props.map((p) => {
    const q = live.get(p.name);
    return q ? { ...p, status: statusOf(q) } : p;
  });
  return extra.length ? [...merged, ...extra] : merged;
}

// Commands that start a prove (one at a time; the UI refuses a second).
export const isProveCmd = (script: string) => /^\s*(prove|check_fv)\b/.test(script);

export function identifiers(expr: string): string[] {
  const skip = new Set(["past", "rose", "fell", "stable", "changed", "posedge", "negedge", "disable", "iff"]);
  return [...new Set(expr.match(/[A-Za-z_][A-Za-z0-9_]*/g) ?? [])].filter((w) => !skip.has(w) && !/^\d/.test(w));
}

// Best-effort source line for a property: where its name appears, else the
// first line that mentions one of its signals. 1-based; 0 when unknown.
export function locate(text: string, p: ViewProp | null): number {
  if (!p || !text) return 0;
  const lines = text.split("\n");
  const byName = lines.findIndex((l) => new RegExp(`\\b${p.name}\\b`).test(l));
  if (byName >= 0) return byName + 1;
  const ids = identifiers(p.expr);
  if (!ids.length) return 0;
  const re = new RegExp(`\\b(${ids.join("|")})\\b`);
  return lines.findIndex((l) => re.test(l)) + 1;
}

// The snapshot carries no netlist, so ports come from a light scan of the
// top file's ANSI port list. Good enough for a hierarchy preview.
export type Port = { dir: string; name: string; width: string };
export function parsePorts(text: string): Port[] {
  const head = text.match(/\bmodule\s+\w+\s*(?:#\s*\([\s\S]*?\)\s*)?\(([\s\S]*?)\)\s*;/);
  if (!head) return [];
  const out: Port[] = [];
  let dir = "";
  let width = "";
  for (const seg of head[1].split(",")) {
    const m = seg.trim().match(/^(input|output|inout)?\s*(?:wire|logic|reg|bit)?\s*(?:signed\s*)?(\[[^\]]+\])?\s*([A-Za-z_]\w*)$/);
    if (!m) continue;
    if (m[1]) {
      dir = m[1];
      width = m[2] ?? "";
    } else if (m[2]) width = m[2];
    if (dir) out.push({ dir, name: m[3], width });
  }
  return out;
}
