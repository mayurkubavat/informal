// Pure view-model helpers shared by the workbench components: status words,
// table cell text, sort keys, and light source scanning. No React here.

import type { DesignScope, EngineRun, PropStatus, Property, ProveProgress, StatusWord } from "./backend";

// What a row shows: the backend's status words plus "running", which only
// exists in the view while a prove's progress events say so.
export type ViewStatus = StatusWord | "running";
// `provisional`: known only from a running prove's progress (the script that
// added it has not returned yet); the session replaces it when it does.
export type ViewProp = Omit<Property, "status"> & {
  status: Omit<PropStatus, "status"> & { status: ViewStatus };
  provisional?: boolean;
};

// "ask-in": a question typed in Ask mode; "ask": the streamed answer so far.
// "ask-step": a tool the model used to look something up before answering.
export type LogLine = { kind: "in" | "out" | "err" | "ask-in" | "ask" | "ask-step"; text: string };
export type LogEvent = { at: Date; cmd: string; ok: boolean; ms: number; summary: string };
export type Theme = "system" | "light" | "dark";
export type Center = "table" | "source" | "trace";
export type BottomTab = "console" | "details" | "messages" | "log" | "profiler" | "proposals" | "info";
export type LeftTab = "tasks" | "design";
export type SortKey = "kind" | "name" | "status" | "engine" | "bound" | "time" | "cpu" | "memory" | "lemmas" | "evidence" | "task";
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
export const taskOf = (p: { task?: string }) => p.task ?? TASK;

// The property table's columns that can be hidden (View > Columns, or a
// right-click on the header). Name and status always show.
export type ColKey = "engine" | "bound" | "time" | "cpu" | "memory" | "lemmas" | "evidence" | "task";
export const OPTIONAL_COLS: { key: ColKey; label: string; num?: boolean; title: string }[] = [
  { key: "engine", label: "Engine", title: "The engine whose verdict was taken (while running: the engine reporting the deepest bound)" },
  { key: "bound", label: "Bound", num: true, title: "CEX or cover: trace depth. Proven: ∞. Running: the bound reached so far" },
  { key: "time", label: "Time", num: true, title: "Wall time of the property's portfolio run (while running: elapsed)" },
  { key: "cpu", label: "CPU", num: true, title: "CPU time of the process and its engine processes while the property ran; can exceed the wall time" },
  { key: "memory", label: "Peak mem", num: true, title: "Peak resident memory, when it rose during this property's run" },
  { key: "lemmas", label: "Lemmas", num: true, title: "Clauses of the inductive invariant the winning engine gave" },
  { key: "evidence", label: "Proof", title: "What the winner gave: invariant, k-induction (with k), trace" },
  { key: "task", label: "Task", title: "The task the property belongs to" },
];
export const DEFAULT_HIDDEN: ColKey[] = ["cpu", "memory", "lemmas", "evidence"];

// Peak memory as the table shows it.
export const memText = (kb: number) => (kb < 10240 ? `${kb} KiB` : `${(kb / 1024).toFixed(0)} MiB`);
// The text of an optional column's cell.
export function cell(p: ViewProp, k: ColKey): string {
  const st = p.status.status === "running" ? null : p.stats;
  switch (k) {
    case "engine": return p.status.engine ?? "";
    case "bound": return bound(p);
    case "time": return time(p);
    case "cpu": return st?.cpu_ms != null ? `${(st.cpu_ms / 1000).toFixed(2)}s` : "";
    case "memory": return st?.peak_rss_kb != null ? memText(st.peak_rss_kb) : "";
    case "lemmas": return st?.invariant_clauses != null ? String(st.invariant_clauses) : "";
    case "evidence":
      if (!st?.evidence) return "";
      return st.depth_kind === "k" && st.depth != null ? `${st.evidence} k=${st.depth}` : st.evidence;
    case "task": return taskOf(p);
  }
}

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
    case "cpu": return p.stats?.cpu_ms ?? -1;
    case "memory": return p.stats?.peak_rss_kb ?? -1;
    case "lemmas": return p.stats?.invariant_clauses ?? -1;
    case "evidence": return p.stats?.evidence ?? "";
    case "task": return taskOf(p);
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
  const statusOf = ({ name: _, kind: __, engines: ___, ...status }: ProveProgress["props"][number]) => status;
  const known = new Set(props.map((p) => p.name));
  const extra: ViewProp[] = prog.props
    .filter((q) => !known.has(q.name))
    .map((q) => ({ name: q.name, kind: q.kind ?? guessKind(q.name), expr: "", enabled: true, status: statusOf(q), engines: q.engines ?? [], provisional: true }));
  // A property in the run shows the run's engine record (none until it ends).
  const merged: ViewProp[] = props.map((p) => {
    const q = live.get(p.name);
    return q ? { ...p, status: statusOf(q), engines: q.engines ?? [] } : p;
  });
  return extra.length ? [...merged, ...extra] : merged;
}

// The VCD path of a property's trace: only a finished CEX or cover has one.
export function traceOf(p: ViewProp | null): string | null {
  if (!p || (p.status.status !== "cex" && p.status.status !== "covered")) return null;
  return p.trace ?? null;
}

// ---- console Tab completion ----
// The word Tab completes: the text after the last space, `[`, `{` or `;`.
export function completionWord(cmd: string): { base: string; word: string } {
  const m = cmd.match(/^(.*[\s[{;])?([^\s[{;]*)$/s);
  const base = m?.[1] ?? "";
  return { base, word: cmd.slice(base.length) };
}
// A completion in progress: the input before the word, the candidates, and
// which one is in the input now (-1: none inserted yet).
export type Completion = { base: string; matches: string[]; index: number };
// Start completing `cmd` with `matches` from the backend: one match is
// inserted with a trailing space and ends completion (null state); several
// insert the first and keep the list for cycling; none leaves it unchanged.
export function startCompletion(cmd: string, matches: string[]): { cmd: string; state: Completion | null } {
  const { base } = completionWord(cmd);
  if (!matches.length) return { cmd, state: null };
  if (matches.length === 1) return { cmd: `${base}${matches[0]} `, state: null };
  return { cmd: base + matches[0], state: { base, matches, index: 0 } };
}
// Repeated Tab (dir 1) or Shift+Tab (dir -1): the next candidate, wrapping.
export function cycleCompletion(c: Completion, dir: 1 | -1): { cmd: string; state: Completion } {
  const n = c.matches.length;
  const index = (((c.index + dir) % n) + n) % n;
  return { cmd: c.base + c.matches[index], state: { ...c, index } };
}

// Commands that start a prove (one at a time; the UI refuses a second).
export const isProveCmd = (script: string) => /^\s*(prove|check_fv)\b/.test(script);

export function identifiers(expr: string): string[] {
  const skip = new Set(["past", "rose", "fell", "stable", "changed", "posedge", "negedge", "disable", "iff"]);
  return [...new Set(expr.match(/[A-Za-z_][A-Za-z0-9_]*/g) ?? [])].filter((w) => !skip.has(w) && !/^\d/.test(w));
}

// Fallback for properties without a recorded source: best-effort line: where its name appears, else the
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

// The file the source view shows for a property: where it was written
// (Property.source: its script line, or RTL for SVA in the design), else
// the top design file.
export function sourceFile(p: ViewProp | null, topFile: string | undefined): string | undefined {
  return p?.source?.file ?? topFile;
}
// The line to highlight in `file`: the recorded one when the property came
// from that file, else the old best-effort search (locate) in its text.
export function sourceLine(p: ViewProp | null, file: string | undefined, text: string): number {
  if (p?.source && p.source.file === file) return p.source.line;
  return locate(text, p);
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

// ---- proof profiler ----
// Per engine over a set of properties: how many it ran on and won, its total
// time, and its mean time-to-win (over wins only; null without any).
export type EngineSummary = { engine: string; runs: number; wins: number; totalMs: number; meanWinMs: number | null };
export function profileSummary(props: { engines?: EngineRun[] }[]): EngineSummary[] {
  const by = new Map<string, { runs: number; wins: number; totalMs: number; winMs: number }>();
  for (const p of props)
    for (const r of p.engines ?? []) {
      const e = by.get(r.engine) ?? { runs: 0, wins: 0, totalMs: 0, winMs: 0 };
      e.runs++;
      e.totalMs += r.ms;
      if (r.outcome === "won") {
        e.wins++;
        e.winMs += r.ms;
      }
      by.set(r.engine, e);
    }
  return [...by]
    .map(([engine, e]) => ({ engine, runs: e.runs, wins: e.wins, totalMs: e.totalMs, meanWinMs: e.wins ? e.winMs / e.wins : null }))
    .sort((a, b) => b.wins - a.wins || a.totalMs - b.totalMs || a.engine.localeCompare(b.engine));
}
// A bar's share of the longest time in its table, 0..1 (0 for an empty set).
export const barShare = (ms: number, maxMs: number) => (maxMs > 0 ? Math.max(0, Math.min(1, ms / maxMs)) : 0);
// Milliseconds as the profiler shows them: "41 ms" under a second, else seconds.
export const msText = (ms: number) => (ms < 1000 ? `${Math.round(ms)} ms` : `${(ms / 1000).toFixed(2)} s`);

// ---- design browser ----
// The scope tree the Design tab shows: slang's hierarchy when the snapshot
// has one, else the top module alone with the ports a regex scan of its
// file finds (no SystemVerilog front end). Widths the scan cannot evaluate are 0.
export function designScopes(hierarchy: DesignScope[] | undefined, top: string | null, ports: Port[]): DesignScope[] {
  if (hierarchy?.length) return hierarchy;
  if (!top) return [];
  const width = (w: string) => {
    const m = w.match(/^\[\s*(\d+)\s*:\s*(\d+)\s*\]$/);
    return !w ? 1 : m ? Math.abs(Number(m[1]) - Number(m[2])) + 1 : 0;
  };
  const dir = (d: string) => (d === "input" || d === "output" || d === "inout" ? d : undefined);
  return [{ path: top, name: top, module: top, children: [], signals: ports.map((p) => ({ name: p.name, width: width(p.width), kind: "port" as const, dir: dir(p.dir) })) }];
}
export function findScope(scopes: DesignScope[], path: string | null): DesignScope | null {
  for (const s of scopes) {
    if (s.path === path) return s;
    const hit = findScope(s.children, path);
    if (hit) return hit;
  }
  return null;
}
// The short kind column: port direction, else the declaration kind.
export const KIND_SHORT: Record<string, string> = { input: "in", output: "out", inout: "inout", ref: "ref", variable: "var", net: "net", parameter: "param" };
export const signalKind = (s: { kind: string; dir?: string }) => KIND_SHORT[s.kind === "port" ? s.dir ?? "inout" : s.kind] ?? s.kind;

// Tooltip of the Surfer hand-off when the viewer is not on PATH.
export const SURFER_MISSING = "Install Surfer (surfer-project.org) to open traces externally";
