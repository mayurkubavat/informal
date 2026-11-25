// Pure converter from `informal-mcp` `trace_get` structured JSON (`informal.trace/1`)
// into IEEE 1364 VCD text so the SEP-1865 Antigravity MCP App (`workbench.html`)
// renders real `<canvas>` waveforms, `lp.pd.<domain>.*` power-domain swimlanes,
// and supports `Export VCD` without direct filesystem access.

export interface McpTraceSignal {
  name: string;
  width: number;
  values: string[];
}

export interface McpTracePayload {
  schema?: string;
  frames: number;
  unit?: string;
  first?: number;
  last?: number;
  hit_at?: number[];
  property?: { name?: string };
  signals: McpTraceSignal[];
}

const HEX_NIBBLE: Record<string, string> = {
  "0": "0000",
  "1": "0001",
  "2": "0010",
  "3": "0011",
  "4": "0100",
  "5": "0101",
  "6": "0110",
  "7": "0111",
  "8": "1000",
  "9": "1001",
  a: "1010",
  b: "1011",
  c: "1100",
  d: "1101",
  e: "1110",
  f: "1111",
  x: "xxxx",
  z: "zzzz",
};

function vcdId(index: number): string {
  // Printable ASCII 33..126 (`!`..`~`), base 94
  let n = index;
  let out = "";
  do {
    out = String.fromCharCode(33 + (n % 94)) + out;
    n = Math.floor(n / 94) - 1;
  } while (n >= 0);
  return out;
}

export function traceValueToBits(raw: string, width: number): string {
  const s = (raw ?? "x").trim().toLowerCase();
  if (width <= 1) {
    const ch = s[s.length - 1] ?? "x";
    return ch === "0" || ch === "1" || ch === "x" || ch === "z" ? ch : "x";
  }
  let bits = "";
  for (const ch of s) {
    bits += HEX_NIBBLE[ch] ?? "xxxx";
  }
  if (bits.length < width) bits = bits.padStart(width, bits[0] === "x" || bits[0] === "z" ? bits[0] : "0");
  else if (bits.length > width) bits = bits.slice(bits.length - width);
  return bits;
}

interface ScopeTree {
  vars: { id: string; width: number; leaf: string }[];
  children: Map<string, ScopeTree>;
}

function makeTree(): ScopeTree {
  return { vars: [], children: new Map() };
}

function insertVar(root: ScopeTree, fullPath: string, id: string, width: number) {
  const parts = fullPath.split(".").filter(Boolean);
  const leaf = parts.pop() || "sig";
  let node = root;
  for (const seg of parts) {
    let next = node.children.get(seg);
    if (!next) {
      next = makeTree();
      node.children.set(seg, next);
    }
    node = next;
  }
  node.vars.push({ id, width, leaf });
}

function emitTree(node: ScopeTree, out: string[]) {
  for (const v of node.vars) {
    out.push(`$var wire ${v.width} ${v.id} ${v.leaf} $end`);
  }
  for (const [name, child] of node.children) {
    out.push(`$scope module ${name} $end`);
    emitTree(child, out);
    out.push("$upscope $end");
  }
}

export function mcpTraceToVcdText(payload: McpTracePayload): string {
  const signals = Array.isArray(payload.signals) ? payload.signals : [];
  const frames = Math.max(
    1,
    Number(payload.frames) || signals.reduce((m, s) => Math.max(m, s.values?.length ?? 0), 0),
  );
  const steps = payload.unit === "step";
  const propName = payload.property?.name || "trace";

  const out: string[] = [
    "$timescale 1ns $end",
    `$comment informal trace of ${propName}: ${frames} ${steps ? "model steps" : "cycles"} $end`,
  ];

  const tree = makeTree();
  const entries = signals.map((s, idx) => {
    const id = vcdId(idx + 1);
    const width = Math.max(1, Number(s.width) || 1);
    insertVar(tree, s.name, id, width);
    return { id, width, values: s.values ?? [] };
  });

  // Synthetic clock when cycle-based so `cycleTimes()` locks onto exact frame spacing.
  const clkId = vcdId(0);
  if (!steps) {
    out.push("$scope module informal $end");
    out.push(`$var wire 1 ${clkId} clk $end`);
    out.push("$upscope $end");
  }
  emitTree(tree, out);
  out.push("$enddefinitions $end");

  const prev = new Array<string | null>(entries.length).fill(null);
  for (let f = 0; f < frames; f++) {
    out.push(`#${f * 10}`);
    if (!steps) out.push(`1${clkId}`);
    for (let i = 0; i < entries.length; i++) {
      const e = entries[i];
      const bits = traceValueToBits(e.values[f] ?? "x", e.width);
      if (prev[i] === bits) continue;
      prev[i] = bits;
      out.push(e.width === 1 ? `${bits}${e.id}` : `b${bits} ${e.id}`);
    }
    if (!steps) {
      out.push(`#${f * 10 + 5}`);
      out.push(`0${clkId}`);
    }
  }
  return `${out.join("\n")}\n`;
}
