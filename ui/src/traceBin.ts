// Binary `IFTR` v1 waveform codec (`Uint8Array` zero-JSON IPC from Tauri v2),
// zoom bucket decimator for 5,000+ cycle canvas viewports, and deterministic
// standalone SVG exporter for `gui_capture_view`.

import { formatValue, type Radix, type Signal, type Vcd } from "./vcd";

export interface TraceAnnotation {
  frame: number;
  signal: string;
  label: string;
  tone?: "accent" | "danger" | "ok";
}

const textDecoder = new TextDecoder();
const textEncoder = new TextEncoder();

export function decodeIftr(bytes: Uint8Array): Vcd & { frames: number; steps: boolean } {
  if (bytes.byteLength < 14 || String.fromCharCode(bytes[0], bytes[1], bytes[2], bytes[3]) !== "IFTR") {
    throw new Error("Invalid IFTR binary trace header");
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const version = view.getUint8(4);
  if (version !== 1) throw new Error(`Unsupported IFTR version ${version}`);
  const steps = (view.getUint8(5) & 1) !== 0;
  const frames = view.getUint32(6, true);
  const sigCount = view.getUint32(10, true);

  let off = 14;
  const signals: Signal[] = [];
  const changesByPath = new Map<string, { t: number; v: string }[]>();
  const timeSet = new Set<number>();

  for (let i = 0; i < sigCount; i++) {
    const nlen = view.getUint16(off, true);
    off += 2;
    const path = textDecoder.decode(bytes.subarray(off, off + nlen));
    off += nlen;
    const width = view.getUint16(off, true);
    off += 2;
    const isClock = (view.getUint8(off) & 1) !== 0;
    off += 1;
    const runCount = view.getUint32(off, true);
    off += 4;

    const leaf = path.split(".").pop() ?? path;
    signals.push({ name: leaf, path, width, id: `b${i}` });

    const list: { t: number; v: string }[] = [];
    if (isClock && !steps) {
      // Synthesize 50%-duty clock transitions across `frames` cycles (`1` at 10*f, `0` at 10*f + 5).
      for (let r = 0; r < runCount; r++) {
        off += 4;
        const vlen = view.getUint16(off, true);
        off += 2 + vlen;
      }
      for (let f = 0; f < frames; f++) {
        list.push({ t: f * 10, v: "1" });
        list.push({ t: f * 10 + 5, v: "0" });
        timeSet.add(f * 10);
        timeSet.add(f * 10 + 5);
      }
    } else {
      for (let r = 0; r < runCount; r++) {
        const startFrame = view.getUint32(off, true);
        off += 4;
        const vlen = view.getUint16(off, true);
        off += 2;
        const val = textDecoder.decode(bytes.subarray(off, off + vlen));
        off += vlen;
        const t = startFrame * 10;
        list.push({ t, v: val });
        timeSet.add(t);
      }
    }
    changesByPath.set(path, list);
  }

  for (let f = 0; f < frames; f++) timeSet.add(f * 10);
  const times = Array.from(timeSet).sort((a, b) => a - b);
  const pathOf = (s: Signal | string) => (typeof s === "string" ? s : s.path);

  return {
    timescale: "1ns",
    signals,
    times,
    frames,
    steps,
    changes: (s) => changesByPath.get(pathOf(s)) ?? [],
    valuesAt(s, time) {
      const list = changesByPath.get(pathOf(s));
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

/** Encode a parsed `Vcd` + `cycles` into `IFTR` v1 binary bytes (used by Browser Mock & tests). */
export function encodeVcdToIftr(vcd: Vcd, frames: number, clockPath?: string | null): Uint8Array {
  const chunks: number[] = [];
  const pushU8 = (v: number) => chunks.push(v & 0xff);
  const pushU16 = (v: number) => {
    chunks.push(v & 0xff, (v >>> 8) & 0xff);
  };
  const pushU32 = (v: number) => {
    chunks.push(v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff);
  };
  const pushBytes = (b: Uint8Array) => {
    for (let i = 0; i < b.length; i++) chunks.push(b[i]);
  };

  pushBytes(textEncoder.encode("IFTR"));
  pushU8(1);
  pushU8(0);
  pushU32(Math.max(1, frames));
  pushU32(vcd.signals.length);

  for (let si = 0; si < vcd.signals.length; si++) {
    const s = vcd.signals[si];
    const nb = textEncoder.encode(s.path);
    pushU16(nb.length);
    pushBytes(nb);
    pushU16(s.width);
    const isClk = clockPath ? s.path === clockPath || s.name === clockPath : si === 0 && s.name === "clk";
    pushU8(isClk ? 1 : 0);

    const runs: { f: number; v: string }[] = [];
    if (isClk) {
      runs.push({ f: 0, v: "1" });
    } else {
      let prev = "";
      for (let f = 0; f < frames; f++) {
        const v = vcd.valuesAt(s, f * 10) ?? "x".repeat(s.width);
        if (f === 0 || v !== prev) {
          runs.push({ f, v });
          prev = v;
        }
      }
    }
    pushU32(runs.length);
    for (const r of runs) {
      pushU32(r.f);
      const vb = textEncoder.encode(r.v);
      pushU16(vb.length);
      pushBytes(vb);
    }
  }
  return new Uint8Array(chunks);
}

/** Generate a synthetic large binary `IFTR` trace (`sigCount` signals x `frames` cycles) for 60fps stress tests. */
export function generateSyntheticIftr(sigCount: number, frames: number): Uint8Array {
  const chunks: number[] = [];
  const pushU8 = (v: number) => chunks.push(v & 0xff);
  const pushU16 = (v: number) => chunks.push(v & 0xff, (v >>> 8) & 0xff);
  const pushU32 = (v: number) =>
    chunks.push(v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff);
  const pushBytes = (b: Uint8Array) => {
    for (let i = 0; i < b.length; i++) chunks.push(b[i]);
  };

  pushBytes(textEncoder.encode("IFTR"));
  pushU8(1);
  pushU8(0);
  pushU32(frames);
  pushU32(sigCount + 1);

  // Signal 0: counter.clk
  const clkName = textEncoder.encode("counter.clk");
  pushU16(clkName.length);
  pushBytes(clkName);
  pushU16(1);
  pushU8(1);
  pushU32(1);
  pushU32(0);
  pushU16(1);
  pushU8(49); // '1'

  for (let i = 0; i < sigCount; i++) {
    const name = i === 0 ? "counter.q" : i === 1 ? "counter.en" : i === 2 ? "counter.rst_n" : `counter.sig_${String(i).padStart(4, "0")}`;
    const width = i % 3 === 0 ? 8 : 1;
    const nb = textEncoder.encode(name);
    pushU16(nb.length);
    pushBytes(nb);
    pushU16(width);
    pushU8(0);

    const step = Math.max(1, (i % 17) + 3);
    const runs: [number, Uint8Array][] = [];
    for (let f = 0; f < frames; f += step) {
      const val =
        width === 1
          ? textEncoder.encode(((f / step + i) & 1) ? "1" : "0")
          : textEncoder.encode(((f + i) & 0xff).toString(2).padStart(8, "0"));
      runs.push([f, val]);
    }
    pushU32(runs.length);
    for (const [f, vb] of runs) {
      pushU32(f);
      pushU16(vb.length);
      pushBytes(vb);
    }
  }
  return new Uint8Array(chunks);
}

function escapeXml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/** Deterministic standalone SVG snapshot of an open trace (with cursor, causal cells, and AI callouts) for `gui_capture_view`. */
export function renderTraceSvg(opts: {
  propName: string;
  cover: boolean;
  depth: number;
  vcd: Vcd;
  cycles: number[];
  lanes: Signal[];
  cursor: number;
  px: number;
  radix: Record<string, Radix>;
  causalFramesBySig?: Map<string, Set<number>>;
  annotations?: TraceAnnotation[];
}): string {
  const { propName, cover, depth, vcd, cycles, lanes, cursor, px, radix, causalFramesBySig, annotations } = opts;
  const n = Math.max(1, cycles.length);
  const nameCol = 180;
  const laneH = 26;
  const headH = 24;
  const w = nameCol + n * px;
  const h = headH + (lanes.length + 1) * laneH;
  const statusColor = cover ? "#3b5bc4" : "#b4471b";

  const out: string[] = [];
  out.push(`<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}" font-family="IBM Plex Mono, monospace" font-size="11">`);
  out.push(`<rect width="${w}" height="${h}" fill="#fdfcfa" />`);
  out.push(`<rect width="${nameCol}" height="${h}" fill="#f1efe9" />`);
  out.push(`<rect x="${nameCol + cursor * px}" y="0" width="${px}" height="${h}" fill="${statusColor}" fill-opacity="0.09" />`);

  // Ruler
  for (let c = 0; c < n; c++) {
    const x = nameCol + c * px;
    out.push(`<line x1="${x}" y1="0" x2="${x}" y2="${h}" stroke="#e3e0d7" stroke-width="1" />`);
    out.push(`<text x="${x + 4}" y="15" fill="${c === depth ? statusColor : "#6a665d"}">${c}</text>`);
  }
  out.push(`<line x1="0" y1="${headH}" x2="${w}" y2="${headH}" stroke="#d9d5ca" />`);

  // Property lane
  const py = headH;
  const failX = nameCol + Math.max(0, Math.min(n - 1, depth)) * px;
  out.push(`<text x="8" y="${py + 17}" fill="#22211e" font-weight="600">${escapeXml(propName)}</text>`);
  out.push(`<line x1="${nameCol}" y1="${py + laneH - 5}" x2="${failX}" y2="${py + laneH - 5}" stroke="#55524a" stroke-width="1.5" />`);
  out.push(`<rect x="${failX + 2}" y="${py + 4}" width="${Math.max(4, px - 4)}" height="${laneH - 8}" rx="2" fill="${statusColor}" fill-opacity="0.18" stroke="${statusColor}" />`);
  out.push(`<text x="${failX + px / 2}" y="${py + 17}" text-anchor="middle" fill="${statusColor}" font-weight="600">${cover ? "cover" : "fail"}</text>`);

  // Signal lanes
  lanes.forEach((s, idx) => {
    const y = headH + (idx + 1) * laneH;
    const hi = y + 5;
    const lo = y + laneH - 5;
    const mid = y + laneH / 2;
    const rdx = radix[s.path] ?? "hex";
    out.push(`<text x="8" y="${y + 17}" fill="#22211e">${escapeXml(s.path)}</text>`);
    out.push(`<line x1="0" y1="${y + laneH}" x2="${w}" y2="${y + laneH}" stroke="#e3e0d7" />`);

    const causalFrames = causalFramesBySig?.get(s.path);
    if (causalFrames) {
      for (const cf of causalFrames) {
        if (cf >= 0 && cf < n) {
          out.push(`<rect x="${nameCol + cf * px + 1}" y="${y + 2}" width="${Math.max(2, px - 2)}" height="${laneH - 4}" rx="2" fill="#fbf1d9" stroke="#e0ae4a" stroke-opacity="0.6" />`);
        }
      }
    }

    if (s.width === 1) {
      let d = "";
      for (let c = 0; c < n; c++) {
        const v = vcd.valuesAt(s, cycles[c]) ?? "x";
        const yv = v === "1" ? hi : v === "0" ? lo : mid;
        const x0 = nameCol + c * px;
        const x1 = x0 + px;
        d += d ? ` L${x0},${yv} L${x1},${yv}` : `M${x0},${yv} L${x1},${yv}`;
      }
      out.push(`<path d="${d}" fill="none" stroke="#22211e" stroke-width="1.4" />`);
    } else {
      let runStart = 0;
      let runVal = vcd.valuesAt(s, cycles[0]) ?? "x".repeat(s.width);
      const flush = (cEnd: number, val: string) => {
        const x0 = nameCol + runStart * px;
        const x1 = nameCol + cEnd * px;
        const txt = formatValue(val, rdx);
        out.push(`<rect x="${x0 + 1}" y="${hi}" width="${Math.max(2, x1 - x0 - 2)}" height="${lo - hi}" rx="2" fill="#fdfcfa" stroke="#55524a" stroke-width="1.2" />`);
        if (txt.length * 7 + 6 <= x1 - x0) {
          out.push(`<text x="${(x0 + x1) / 2}" y="${mid + 4}" text-anchor="middle" fill="#22211e">${escapeXml(txt)}</text>`);
        }
      };
      for (let c = 1; c < n; c++) {
        const v = vcd.valuesAt(s, cycles[c]) ?? "x".repeat(s.width);
        if (v !== runVal) {
          flush(c, runVal);
          runStart = c;
          runVal = v;
        }
      }
      flush(n, runVal);
    }

    // AI callout pins on this signal
    for (const ann of annotations ?? []) {
      if ((ann.signal === s.path || s.path.endsWith(`.${ann.signal}`) || ann.signal.endsWith(`.${s.path}`)) && ann.frame >= 0 && ann.frame < n) {
        const ax = nameCol + ann.frame * px + 2;
        const badgeColor = ann.tone === "danger" ? "#b42318" : ann.tone === "ok" ? "#2f7a4c" : "#1d5f5b";
        out.push(`<rect x="${ax}" y="${y + 2}" width="${Math.max(36, ann.label.length * 6.5 + 8)}" height="14" rx="3" fill="${badgeColor}" />`);
        out.push(`<text x="${ax + 4}" y="${y + 12}" fill="#ffffff" font-size="9.5" font-weight="600">${escapeXml(ann.label)}</text>`);
      }
    }
  });

  out.push(`<line x1="${nameCol + cursor * px + 0.5}" y1="0" x2="${nameCol + cursor * px + 0.5}" y2="${h}" stroke="${statusColor}" stroke-width="1.5" />`);
  out.push(`</svg>`);
  return out.join("\n");
}
