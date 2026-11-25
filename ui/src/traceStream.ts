// Windowed FST/IFTR waveform streaming cache for deep traces (decision D10).
// Keeps only loaded frame windows in memory and presents the standard `Vcd`
// interface to `TraceView` while fetching additional windows on demand as the
// user scrolls, zooms, or jumps the cursor.

import { decodeIftr } from "./traceBin";
import type { Signal, Vcd } from "./vcd";

export interface TraceSignalMeta {
  name: string;
  path: string;
  width: number;
  clock: boolean;
}

export interface TraceMeta {
  frames: number;
  steps: boolean;
  format: "fst" | "vcd";
  block_count: number;
  signals: TraceSignalMeta[];
}

export interface FrameRange {
  first: number;
  last: number;
}

export const STREAM_THRESHOLD_FRAMES = 512;
export const STREAM_CHUNK_FRAMES = 256;

/** Stateful accumulator of streamed `IFTR` windows over a single trace. */
export class WindowedTraceStore {
  readonly meta: TraceMeta;
  private readonly changesByPath = new Map<string, Map<number, string>>();
  private readonly sortedCache = new Map<string, { t: number; v: string }[]>();
  private readonly ranges: FrameRange[] = [];
  private readonly signals: Signal[];
  private readonly times: number[];

  constructor(meta: TraceMeta) {
    this.meta = meta;
    this.signals = meta.signals.map((s, i) => ({
      name: s.name,
      path: s.path,
      width: s.width,
      id: `s${i}`,
    }));
    const n = Math.max(1, meta.frames);
    this.times = new Array<number>(n);
    for (let f = 0; f < n; f++) this.times[f] = f * 10;

    // Synthesize regular clock transitions up front when `!steps`.
    if (!meta.steps) {
      for (const sm of meta.signals) {
        if (!sm.clock) continue;
        const list: { t: number; v: string }[] = [];
        for (let f = 0; f < n; f++) {
          list.push({ t: f * 10, v: "1" });
          list.push({ t: f * 10 + 5, v: "0" });
        }
        this.sortedCache.set(sm.path, list);
      }
    }
  }

  /** Merge a streamed `IFTR` binary window covering `[first..=last]` into the store. */
  mergeWindow(first: number, last: number, bytes: Uint8Array): void {
    const decoded = decodeIftr(bytes);
    for (const sig of decoded.signals) {
      const sm = this.meta.signals.find((m) => m.path === sig.path);
      if (sm?.clock && !this.meta.steps) continue;
      let map = this.changesByPath.get(sig.path);
      if (!map) {
        map = new Map<number, string>();
        this.changesByPath.set(sig.path, map);
      }
      for (const ch of decoded.changes(sig)) {
        const f = Math.floor(ch.t / 10);
        if (f >= first && f <= last) {
          map.set(ch.t, ch.v);
        }
      }
      this.sortedCache.delete(sig.path);
    }
    this.addRange(first, last);
  }

  private addRange(first: number, last: number): void {
    const clampedFirst = Math.max(0, first);
    const clampedLast = Math.min(Math.max(0, this.meta.frames - 1), Math.max(clampedFirst, last));
    this.ranges.push({ first: clampedFirst, last: clampedLast });
    this.ranges.sort((a, b) => a.first - b.first);
    const merged: FrameRange[] = [];
    for (const r of this.ranges) {
      const prev = merged[merged.length - 1];
      if (prev && r.first <= prev.last + 1) {
        prev.last = Math.max(prev.last, r.last);
      } else {
        merged.push({ ...r });
      }
    }
    this.ranges.length = 0;
    this.ranges.push(...merged);
  }

  /** True if every frame in `[first..=last]` has already been streamed. */
  isCovered(first: number, last: number): boolean {
    const lo = Math.max(0, first);
    const hi = Math.min(Math.max(0, this.meta.frames - 1), Math.max(lo, last));
    return this.ranges.some((r) => r.first <= lo && r.last >= hi);
  }

  /** Return the next aligned uncached chunk overlapping `[first..=last]`, or `null`. */
  nextMissingChunk(first: number, last: number, chunkSize = STREAM_CHUNK_FRAMES): FrameRange | null {
    const maxF = Math.max(0, this.meta.frames - 1);
    const lo = Math.max(0, Math.min(maxF, first));
    const hi = Math.max(lo, Math.min(maxF, last));
    if (this.isCovered(lo, hi)) return null;
    // Align to chunk boundary so repeated scrolls coalesce cleanly.
    const cFirst = Math.floor(lo / chunkSize) * chunkSize;
    const cLast = Math.min(maxF, Math.max(cFirst + chunkSize - 1, Math.ceil((hi + 1) / chunkSize) * chunkSize - 1));
    // Cap a single fetch to 2 chunks so macro-zoom never loads an entire huge trace at once.
    return { first: cFirst, last: Math.min(cLast, cFirst + chunkSize * 2 - 1) };
  }

  loadedRanges(): readonly FrameRange[] {
    return this.ranges;
  }

  private getSorted(path: string): { t: number; v: string }[] {
    let cached = this.sortedCache.get(path);
    if (cached) return cached;
    const map = this.changesByPath.get(path);
    if (!map || map.size === 0) return [];
    cached = Array.from(map.entries())
      .map(([t, v]) => ({ t, v }))
      .sort((a, b) => a.t - b.t);
    this.sortedCache.set(path, cached);
    return cached;
  }

  /** Produce an immutable `Vcd` snapshot view over the currently loaded windows. */
  toVcd(): Vcd & { frames: number; steps: boolean; format: "fst" | "vcd" } {
    const pathOf = (s: Signal | string) => (typeof s === "string" ? s : s.path);
    const store = this;
    return {
      timescale: "1ns",
      signals: this.signals,
      times: this.times,
      frames: this.meta.frames,
      steps: this.meta.steps,
      format: this.meta.format,
      changes: (s) => store.getSorted(pathOf(s)),
      valuesAt(s, time) {
        const list = store.getSorted(pathOf(s));
        if (!list.length || list[0].t > time) return null;
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
}
