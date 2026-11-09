// A small VCD (IEEE 1364 value change dump) reader for the trace tab: the
// header's timescale, scopes and variables, then value changes per time.
// Enough for the traces informal writes (docs/ui/trace-format.md) and the usual
// simulator dumps; reals and strings are kept as text, not interpreted.

export interface Signal {
  /** Leaf name as declared, e.g. "q". */
  name: string;
  /** Dotted scope path plus name, e.g. "counter.q". Unique per signal. */
  path: string;
  width: number;
  /** The VCD identifier code (shared by aliases). */
  id: string;
}

export interface Vcd {
  timescale: string;
  signals: Signal[];
  /** Every time with at least one change, ascending. */
  times: number[];
  /** Bit string (MSB first, width chars of 0/1/x/z) in effect at `time`, or null before the first change. */
  valuesAt(signal: Signal | string, time: number): string | null;
  /** Times where this signal changes, with the new value. */
  changes(signal: Signal | string): { t: number; v: string }[];
}

export function parseVcd(text: string): Vcd {
  const tok = text.split(/\s+/).filter(Boolean);
  const signals: Signal[] = [];
  const widths = new Map<string, number>();
  const ch = new Map<string, { t: number; v: string }[]>();
  const scope: string[] = [];
  let timescale = "";
  let t = 0;
  const times: number[] = [];
  const set = (id: string, raw: string) => {
    const w = widths.get(id);
    if (w === undefined) return; // undeclared id: ignore
    const v = fit(raw.toLowerCase(), w);
    const list = ch.get(id)!;
    const last = list[list.length - 1];
    if (last && last.t === t) last.v = v;
    else if (!last || last.v !== v) list.push({ t, v });
  };
  let i = 0;
  const until = (end = "$end") => {
    const out: string[] = [];
    while (i < tok.length && tok[i] !== end) out.push(tok[i++]);
    i++; // skip $end
    return out;
  };
  while (i < tok.length) {
    const w = tok[i++];
    switch (w) {
      case "$timescale":
        timescale = until().join("");
        break;
      case "$scope": {
        const [, name] = until();
        scope.push(name ?? "");
        break;
      }
      case "$upscope":
        until();
        scope.pop();
        break;
      case "$var": {
        // $var <type> <width> <id> <name> [<range>] $end
        const [, width, id, name] = until();
        const n = Number(width) || 1;
        signals.push({ name, path: [...scope, name].filter(Boolean).join("."), width: n, id });
        if (!widths.has(id)) {
          widths.set(id, n);
          ch.set(id, []);
        }
        break;
      }
      case "$comment":
      case "$date":
      case "$version":
      case "$enddefinitions":
        until();
        break;
      case "$dumpvars":
      case "$dumpall":
      case "$dumpon":
      case "$dumpoff":
      case "$end":
        break; // their contents are ordinary value changes
      default:
        if (w[0] === "#") {
          t = Number(w.slice(1));
          if (times[times.length - 1] !== t) times.push(t);
        } else if (w[0] === "b" || w[0] === "B" || w[0] === "r" || w[0] === "R") {
          set(tok[i++], w[0] === "b" || w[0] === "B" ? w.slice(1) : w);
        } else if (/^[01xzXZ]/.test(w)) {
          set(w.slice(1), w[0]);
        }
    }
  }
  // A dump may mention a time with no changes; keep only times with changes.
  const used = new Set<number>();
  for (const list of ch.values()) for (const c of list) used.add(c.t);
  const allTimes = times.filter((x) => used.has(x));
  const byPath = new Map(signals.map((s) => [s.path, s]));
  const idOf = (s: Signal | string) => (typeof s === "string" ? byPath.get(s)?.id : s.id);
  return {
    timescale,
    signals,
    times: allTimes,
    changes: (s) => ch.get(idOf(s) ?? "") ?? [],
    valuesAt(s, time) {
      const list = ch.get(idOf(s) ?? "");
      if (!list || !list.length || list[0].t > time) return null;
      let lo = 0;
      let hi = list.length - 1;
      while (lo < hi) {
        const mid = (lo + hi + 1) >> 1;
        if (list[mid].t <= time) lo = mid;
        else hi = mid - 1;
      }
      return list[lo].v;
    },
  };
}

// VCD left-extends short vectors: with 0/1 by 0, with x or z by that letter.
function fit(v: string, width: number): string {
  if (v[0] === "r") return v; // real: kept as text
  if (v.length >= width) return v.slice(v.length - width);
  const pad = v[0] === "x" || v[0] === "z" ? v[0] : "0";
  return pad.repeat(width - v.length) + v;
}

export type Radix = "bin" | "dec" | "hex";

/** A bit string in the given radix, without a prefix. x/z digits stay x/z. */
export function formatValue(bits: string | null, radix: Radix): string {
  if (bits === null) return "";
  if (bits[0] === "r") return bits.slice(1);
  if (radix === "bin") return bits;
  if (/[xz]/.test(bits)) {
    if (radix === "dec") return /^z+$/.test(bits) ? "z" : "x";
    // Hex digit by digit: a nibble with any x shows x (z only if all z).
    const pad = (4 - (bits.length % 4)) % 4;
    const b = (/[xz]/.test(bits[0]) ? bits[0] : "0").repeat(pad) + bits;
    let out = "";
    for (let i = 0; i < b.length; i += 4) {
      const n = b.slice(i, i + 4);
      out += /^z+$/.test(n) ? "z" : /[xz]/.test(n) ? "x" : parseInt(n, 2).toString(16);
    }
    return out;
  }
  const n = BigInt("0b" + bits);
  return radix === "dec" ? n.toString(10) : n.toString(16);
}

/**
 * Cycle start times. With a 1-bit `clk` (any scope) these are its rising
 * edges; without one, every change time is a cycle.
 */
export function cycleTimes(vcd: Vcd): number[] {
  const clk = vcd.signals.find((s) => s.width === 1 && /^(clk|clock)$/i.test(s.name));
  if (clk) {
    const rise = vcd.changes(clk).filter((c, i, a) => c.v === "1" && (i === 0 || a[i - 1].v !== "1")).map((c) => c.t);
    if (rise.length) return rise;
  }
  return vcd.times;
}
