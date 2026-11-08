// Pure view-model helpers shared by the workbench components: status words,
// table cell text, sort keys, and light source scanning. No React here.

import type { Property, StatusWord } from "./backend";

export type LogLine = { kind: "in" | "out" | "err"; text: string };
export type LogEvent = { at: Date; cmd: string; ok: boolean; ms: number; summary: string };
export type Theme = "system" | "light" | "dark";
export type Center = "table" | "source";
export type BottomTab = "console" | "details" | "messages";
export type LeftTab = "tasks" | "design";
export type SortKey = "kind" | "name" | "status" | "engine" | "bound" | "time";
export type StatusFilter = StatusWord | "assume" | null;

export const STATUS_LABEL: Record<StatusWord, string> = {
  unprocessed: "Unprocessed",
  proven: "Proven",
  cex: "CEX",
  covered: "Covered",
  unreachable: "Unreachable",
  undetermined: "Undetermined",
  error: "Error",
};
export const SUMMARY: StatusWord[] = ["proven", "cex", "covered", "unreachable", "undetermined", "error"];
export const STATUS_ORDER: StatusWord[] = ["error", "cex", "undetermined", "covered", "unreachable", "proven", "unprocessed"];
export const KIND_LETTER = { assert: "A", cover: "C", assume: "M" } as const;
export const TASK = "<embedded>";

export function bound(p: Property): string {
  const s = p.status;
  switch (s.status) {
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
export function time(p: Property): string {
  return p.status.ms !== undefined ? `${(p.status.ms / 1000).toFixed(2)}s` : "";
}
export function statusKey(p: Property): string {
  if (p.kind === "assume") return "assume";
  return p.enabled ? p.status.status : "unprocessed";
}
export function statusText(p: Property): string {
  if (p.kind === "assume") return "Assumed";
  return p.enabled ? STATUS_LABEL[p.status.status] : "Disabled";
}

export function sortValue(p: Property, k: SortKey): string | number {
  switch (k) {
    case "kind": return p.kind;
    case "name": return p.name;
    case "status": return p.kind === "assume" ? 99 : STATUS_ORDER.indexOf(p.status.status);
    case "engine": return p.status.engine ?? "";
    case "bound": return p.status.depth ?? (p.status.status === "proven" || p.status.status === "unreachable" ? 1e9 : -1);
    case "time": return p.status.ms ?? -1;
  }
}

export function identifiers(expr: string): string[] {
  const skip = new Set(["past", "rose", "fell", "stable", "changed", "posedge", "negedge", "disable", "iff"]);
  return [...new Set(expr.match(/[A-Za-z_][A-Za-z0-9_]*/g) ?? [])].filter((w) => !skip.has(w) && !/^\d/.test(w));
}

// Best-effort source line for a property: where its name appears, else the
// first line that mentions one of its signals. 1-based; 0 when unknown.
export function locate(text: string, p: Property | null): number {
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
