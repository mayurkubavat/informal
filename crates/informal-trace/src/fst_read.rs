//! Streamed, random-access GTKWave FST (`.fst`) waveform reader (decision D10).
//!
//! Opening an FST file reads ONLY the header, geometry, hierarchy, and block
//! offset directory — seeking past all value-change payloads in O(blocks).
//! Windowed queries (`read_window`, `encode_window_iftr`) read and decompress
//! ONLY the VC blocks overlapping `[first_frame..=last_frame]` and ONLY the
//! signal streams selected by the caller.

use crate::fst_write::{
    FST_AT_MISC, FST_BL_GEOM, FST_BL_HDR, FST_BL_HIER_LZ4, FST_BL_VCDATA_DYN_ALIAS2,
    FST_HT_ATTRBEGIN, FST_MT_COMMENT, FST_ST_VCD_SCOPE, FST_ST_VCD_UPSCOPE,
};
use crate::lz4;
use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;

/// Signal metadata decoded from the FST hierarchy + geometry blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FstSignalMeta {
    /// Full hierarchical path (`top.u_sub.sig`).
    pub name: String,
    /// Bit width.
    pub width: usize,
    /// 0-based handle index into VC blocks.
    pub handle: usize,
    /// True for the trace's first variable (the clock lane).
    pub clock: bool,
}

/// Lightweight file metadata obtained without reading value-change payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FstMeta {
    pub frames: usize,
    pub steps: bool,
    pub timescale: i8,
    pub start_time: u64,
    pub end_time: u64,
    pub signals: Vec<FstSignalMeta>,
    pub notes: Vec<String>,
    pub block_count: usize,
}

/// Decoded signal run-length values within a requested frame window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSignal {
    pub name: String,
    pub width: usize,
    pub clock: bool,
    /// `(frame, msb_first_bits)` transitions starting at `first_frame`.
    pub runs: Vec<(usize, String)>,
}

/// Result of a streamed windowed FST read (`[first_frame..=last_frame]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceWindow {
    pub total_frames: usize,
    pub steps: bool,
    pub first_frame: usize,
    pub last_frame: usize,
    pub signals: Vec<WindowSignal>,
    pub notes: Vec<String>,
    /// Number of VC blocks actually read from disk/stream (for diagnostics/tests).
    pub blocks_read: usize,
}

#[derive(Debug, Clone, Copy)]
struct VcBlockIndex {
    body_offset: u64,
    body_len: usize,
    start_time: u64,
    end_time: u64,
}

/// Random-access FST reader over any seekable byte stream (`File`, `Cursor<Vec<u8>>`).
pub struct FstReader<R: Read + Seek> {
    reader: R,
    meta: FstMeta,
    widths: Vec<usize>,
    blocks: Vec<VcBlockIndex>,
}

impl FstReader<File> {
    /// Open an FST file and index its header, hierarchy, and VC block directory.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::new(file)
    }
}

impl FstReader<Cursor<Vec<u8>>> {
    /// Open an in-memory FST buffer.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        Self::new(Cursor::new(bytes))
    }
}

impl<R: Read + Seek> FstReader<R> {
    /// Parse header, geometry, hierarchy, and VC block offsets without reading
    /// any value-change stream payloads.
    pub fn new(mut reader: R) -> Result<Self> {
        let total_len = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(0))?;

        let mut start_time = 0u64;
        let mut end_time = 0u64;
        let mut timescale = -9i8;
        let mut widths: Vec<usize> = Vec::new();
        let mut blocks: Vec<VcBlockIndex> = Vec::new();
        let mut hier_raw: Option<Vec<u8>> = None;
        let mut saw_header = false;

        let mut pos = 0u64;
        while pos + 9 <= total_len {
            reader.seek(SeekFrom::Start(pos))?;
            let mut hdr = [0u8; 9];
            reader.read_exact(&mut hdr)?;
            let btype = hdr[0];
            let blen = u64::from_be_bytes(hdr[1..9].try_into().unwrap());
            if blen < 8 {
                bail!("corrupt FST block length {blen} at offset {pos}");
            }
            let body_len = (blen - 8) as usize;
            let body_offset = pos + 9;
            if body_offset + (body_len as u64) > total_len {
                bail!("truncated FST block type {btype} at offset {pos}");
            }

            match btype {
                FST_BL_HDR => {
                    if body_len < 65 {
                        bail!("truncated FST header block");
                    }
                    let mut hbuf = vec![0u8; body_len];
                    reader.read_exact(&mut hbuf)?;
                    start_time = u64::from_be_bytes(hbuf[0..8].try_into().unwrap());
                    end_time = u64::from_be_bytes(hbuf[8..16].try_into().unwrap());
                    timescale = hbuf[64] as i8;
                    saw_header = true;
                }
                FST_BL_VCDATA_DYN_ALIAS2 => {
                    if body_len < 24 {
                        bail!("truncated FST VC block");
                    }
                    let mut times_hdr = [0u8; 16];
                    reader.read_exact(&mut times_hdr)?;
                    let b_start = u64::from_be_bytes(times_hdr[0..8].try_into().unwrap());
                    let b_end = u64::from_be_bytes(times_hdr[8..16].try_into().unwrap());
                    blocks.push(VcBlockIndex {
                        body_offset,
                        body_len,
                        start_time: b_start,
                        end_time: b_end,
                    });
                }
                FST_BL_GEOM => {
                    let mut gbuf = vec![0u8; body_len];
                    reader.read_exact(&mut gbuf)?;
                    widths = parse_geometry(&gbuf)?;
                }
                FST_BL_HIER_LZ4 => {
                    let mut hbuf = vec![0u8; body_len];
                    reader.read_exact(&mut hbuf)?;
                    if hbuf.len() < 8 {
                        bail!("truncated FST LZ4 hierarchy");
                    }
                    let uncomp_len = u64::from_be_bytes(hbuf[0..8].try_into().unwrap()) as usize;
                    hier_raw = Some(lz4::decompress(&hbuf[8..], uncomp_len)?);
                }
                _ => {
                    // Skip blackout/auxiliary blocks cleanly.
                }
            }
            pos = body_offset + (body_len as u64);
        }

        if !saw_header {
            bail!("missing FST header block");
        }
        let hier_bytes = hier_raw.ok_or_else(|| anyhow::anyhow!("missing FST hierarchy block"))?;
        let ParsedHierarchy {
            mut signals,
            frames_comment,
            steps,
            notes,
        } = parse_hierarchy(&hier_bytes)?;
        for s in &mut signals {
            if let Some(&w) = widths.get(s.handle) {
                if w > 0 {
                    s.width = w;
                }
            }
        }
        if widths.is_empty() {
            widths = signals.iter().map(|s| s.width).collect();
        }
        let frames = frames_comment.unwrap_or_else(|| ((end_time / 10) as usize).max(1));
        let meta = FstMeta {
            frames,
            steps,
            timescale,
            start_time,
            end_time,
            signals,
            notes,
            block_count: blocks.len(),
        };
        Ok(FstReader {
            reader,
            meta,
            widths,
            blocks,
        })
    }

    pub fn meta(&self) -> &FstMeta {
        &self.meta
    }

    /// Stream only the VC blocks overlapping `[first_frame..=last_frame]` and
    /// decode ONLY the handles in `selected` (or all signals when `None`).
    pub fn read_window(
        &mut self,
        first_frame: usize,
        last_frame: usize,
        selected: Option<&[usize]>,
    ) -> Result<TraceWindow> {
        let max_frame = self.meta.frames.saturating_sub(1);
        let first = first_frame.min(max_frame);
        let last = last_frame.min(max_frame).max(first);

        let sig_indices: Vec<usize> = match selected {
            Some(sel) => sel
                .iter()
                .copied()
                .filter(|&i| i < self.meta.signals.len())
                .collect(),
            None => (0..self.meta.signals.len()).collect(),
        };
        let want_handles: HashSet<usize> = sig_indices
            .iter()
            .map(|&si| self.meta.signals[si].handle)
            .collect();

        let win_start_t = (first as u64) * 10;
        let win_end_t = (last as u64) * 10;

        // Per selected handle: collected `(frame, value)` runs across overlapping blocks.
        let mut handle_runs: Vec<(usize, Vec<(usize, String)>)> = sig_indices
            .iter()
            .map(|&si| (self.meta.signals[si].handle, Vec::new()))
            .collect();

        let mut blocks_read = 0usize;
        for blk in &self.blocks {
            // Skip blocks strictly before or strictly after the requested window.
            // Note: a block covering [0..=2560] holds frames 0..=255 (t = 0..2550).
            if blk.end_time < win_start_t || blk.start_time > win_end_t {
                continue;
            }
            // If `blk.end_time == win_start_t` and there is a later block starting at `win_start_t`,
            // prefer the later block (unless this is the last block).
            if blk.end_time == win_start_t
                && self.blocks.iter().any(|other| {
                    other.start_time == win_start_t && other.body_offset != blk.body_offset
                })
            {
                continue;
            }
            blocks_read += 1;
            self.reader.seek(SeekFrom::Start(blk.body_offset))?;
            let mut buf = vec![0u8; blk.body_len];
            self.reader.read_exact(&mut buf)?;
            decode_vc_block_window(
                &buf,
                &self.widths,
                &want_handles,
                first,
                last,
                &mut handle_runs,
            )?;
        }

        let mut out_signals = Vec::with_capacity(sig_indices.len());
        for (&si, (_, mut runs)) in sig_indices.iter().zip(handle_runs) {
            let sm = &self.meta.signals[si];
            if runs.first().is_none_or(|(f, _)| *f > first) {
                runs.insert(0, (first, "x".repeat(sm.width)));
            }
            out_signals.push(WindowSignal {
                name: sm.name.clone(),
                width: sm.width,
                clock: sm.clock,
                runs,
            });
        }

        Ok(TraceWindow {
            total_frames: self.meta.frames,
            steps: self.meta.steps,
            first_frame: first,
            last_frame: last,
            signals: out_signals,
            notes: self.meta.notes.clone(),
            blocks_read,
        })
    }

    /// Encode a requested frame window directly into compact `IFTR` v1 binary
    /// bytes (matching `informal_mcp::vcd::decode_iftr` and `ui/src/traceBin.ts`).
    pub fn encode_window_iftr(
        &mut self,
        first_frame: usize,
        last_frame: usize,
        selected: Option<&[usize]>,
    ) -> Result<Vec<u8>> {
        let win = self.read_window(first_frame, last_frame, selected)?;
        Ok(encode_trace_window_iftr(&win))
    }
}

/// Encode a [`TraceWindow`] into `IFTR` v1 binary format (runs carry absolute
/// frame numbers within `0..total_frames`, so `decodeIftr` renders any slice
/// or full trace seamlessly).
pub fn encode_trace_window_iftr(win: &TraceWindow) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64 + win.signals.len() * 32);
    buf.extend_from_slice(b"IFTR");
    buf.push(1u8);
    buf.push(u8::from(win.steps));
    buf.extend_from_slice(&(win.total_frames as u32).to_le_bytes());
    buf.extend_from_slice(&(win.signals.len() as u32).to_le_bytes());
    for s in &win.signals {
        let nb = s.name.as_bytes();
        buf.extend_from_slice(&(nb.len() as u16).to_le_bytes());
        buf.extend_from_slice(nb);
        buf.extend_from_slice(&(s.width as u16).to_le_bytes());
        buf.push(u8::from(s.clock));
        buf.extend_from_slice(&(s.runs.len() as u32).to_le_bytes());
        for (start, val) in &s.runs {
            buf.extend_from_slice(&(*start as u32).to_le_bytes());
            let vb = val.as_bytes();
            buf.extend_from_slice(&(vb.len() as u16).to_le_bytes());
            buf.extend_from_slice(vb);
        }
    }
    buf
}

fn read_uleb128(data: &[u8], off: &mut usize) -> Result<u64> {
    let mut val = 0u64;
    let mut shift = 0u32;
    loop {
        if *off >= data.len() {
            bail!("truncated FST uleb128");
        }
        let b = data[*off];
        *off += 1;
        val |= u64::from(b & 0x7f) << shift;
        if (b & 0x80) == 0 {
            return Ok(val);
        }
        shift += 7;
        if shift >= 64 {
            bail!("FST uleb128 overflow");
        }
    }
}

fn read_sleb128(data: &[u8], off: &mut usize) -> Result<i64> {
    let mut val = 0i64;
    let mut shift = 0u32;
    loop {
        if *off >= data.len() {
            bail!("truncated FST sleb128");
        }
        let b = data[*off];
        *off += 1;
        val |= i64::from(b & 0x7f) << shift;
        shift += 7;
        if (b & 0x80) == 0 {
            if shift < 64 && (b & 0x40) != 0 {
                val |= !0i64 << shift;
            }
            return Ok(val);
        }
        if shift >= 64 {
            bail!("FST sleb128 overflow");
        }
    }
}

fn read_cstr<'a>(data: &'a [u8], off: &mut usize) -> Result<&'a str> {
    let start = *off;
    let rel = data[start..]
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| anyhow::anyhow!("unterminated FST string"))?;
    *off = start + rel + 1;
    std::str::from_utf8(&data[start..start + rel]).map_err(|e| anyhow::anyhow!("bad utf8: {e}"))
}

fn parse_geometry(body: &[u8]) -> Result<Vec<usize>> {
    if body.len() < 16 {
        bail!("truncated FST geometry block");
    }
    let uncomp_len = u64::from_be_bytes(body[0..8].try_into().unwrap()) as usize;
    let count = u64::from_be_bytes(body[8..16].try_into().unwrap()) as usize;
    let payload = &body[16..];
    if payload.len() != uncomp_len {
        bail!(
            "compressed FST geometry ({} != {uncomp_len}) requires uncompressed geometry",
            payload.len()
        );
    }
    let mut off = 0usize;
    let mut widths = Vec::with_capacity(count);
    for _ in 0..count {
        widths.push(read_uleb128(payload, &mut off)? as usize);
    }
    Ok(widths)
}

struct ParsedHierarchy {
    signals: Vec<FstSignalMeta>,
    frames_comment: Option<usize>,
    steps: bool,
    notes: Vec<String>,
}

fn parse_hierarchy(raw: &[u8]) -> Result<ParsedHierarchy> {
    let mut off = 0usize;
    let mut scope: Vec<String> = Vec::new();
    let mut signals: Vec<FstSignalMeta> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut frames_comment: Option<usize> = None;
    let mut steps = false;
    let mut next_handle = 0usize;

    while off < raw.len() {
        let tag = raw[off];
        off += 1;
        match tag {
            FST_ST_VCD_SCOPE => {
                if off >= raw.len() {
                    bail!("truncated FST scope");
                }
                let _stype = raw[off];
                off += 1;
                let name = read_cstr(raw, &mut off)?.to_string();
                let _comp = read_cstr(raw, &mut off)?;
                scope.push(name);
            }
            FST_ST_VCD_UPSCOPE => {
                scope.pop();
            }
            FST_HT_ATTRBEGIN => {
                if off + 2 > raw.len() {
                    bail!("truncated FST attr");
                }
                let atype = raw[off];
                let asub = raw[off + 1];
                off += 2;
                let text = read_cstr(raw, &mut off)?;
                let _arg = read_uleb128(raw, &mut off)?;
                if atype == FST_AT_MISC && asub == FST_MT_COMMENT {
                    if let Some(rest) = text.strip_prefix("informal trace of ") {
                        let count = rest.rsplit(": ").next().unwrap_or("");
                        frames_comment = count.split(' ').next().and_then(|n| n.parse().ok());
                        steps = count.ends_with("model steps");
                    } else if !text.is_empty() {
                        notes.push(text.to_string());
                    }
                }
            }
            0xfd => {
                // FST_HT_ATTREND: no payload.
            }
            _ => {
                // Variable declaration (FST_VT_*): dir(1), name\0, len(uleb128), alias(uleb128)
                if off >= raw.len() {
                    bail!("truncated FST var");
                }
                let _dir = raw[off];
                off += 1;
                let raw_name = read_cstr(raw, &mut off)?;
                let width = read_uleb128(raw, &mut off)? as usize;
                let alias = read_uleb128(raw, &mut off)? as usize;
                let handle = if alias == 0 {
                    let h = next_handle;
                    next_handle += 1;
                    h
                } else {
                    alias - 1
                };
                // Strip optional " [N:0]" range suffix added for VCD compatibility.
                let leaf = raw_name.split_once(' ').map_or(raw_name, |(n, _)| n);
                let full_name = if scope.is_empty() {
                    leaf.to_string()
                } else {
                    format!("{}.{leaf}", scope.join("."))
                };
                signals.push(FstSignalMeta {
                    name: full_name,
                    width: width.max(1),
                    handle,
                    clock: signals.is_empty(),
                });
            }
        }
    }
    Ok(ParsedHierarchy {
        signals,
        frames_comment,
        steps,
        notes,
    })
}

fn decode_vc_block_window(
    body: &[u8],
    widths: &[usize],
    want_handles: &HashSet<usize>,
    first_frame: usize,
    last_frame: usize,
    handle_runs: &mut [(usize, Vec<(usize, String)>)],
) -> Result<()> {
    if body.len() < 24 + 24 + 8 {
        bail!("truncated FST VC block");
    }

    // 1. Parse trailer: time table sits at the tail of the block.
    let tail = body.len();
    let time_uncomp = u64::from_be_bytes(body[tail - 24..tail - 16].try_into().unwrap()) as usize;
    let time_comp = u64::from_be_bytes(body[tail - 16..tail - 8].try_into().unwrap()) as usize;
    let time_count = u64::from_be_bytes(body[tail - 8..tail].try_into().unwrap()) as usize;
    if time_comp + 24 + 8 > tail {
        bail!("corrupt FST time table length");
    }
    let time_start = tail - 24 - time_comp;
    if time_comp != time_uncomp {
        bail!("zlib-compressed FST time table is not supported");
    }
    let mut times = Vec::with_capacity(time_count);
    {
        let mut toff = 0usize;
        let tslice = &body[time_start..time_start + time_comp];
        let mut acc = 0u64;
        for _ in 0..time_count {
            acc += read_uleb128(tslice, &mut toff)?;
            times.push(acc);
        }
    }

    // 2. Parse position table immediately preceding the time table.
    let pos_len = u64::from_be_bytes(body[time_start - 8..time_start].try_into().unwrap()) as usize;
    if pos_len + 8 > time_start {
        bail!("corrupt FST position table length");
    }
    let pos_start = time_start - 8 - pos_len;
    let pos_slice = &body[pos_start..time_start - 8];

    // 3. Parse frame initial bits at head of block (after 24-byte header).
    let mut off = 24usize;
    let bits_uncomp = read_uleb128(body, &mut off)? as usize;
    let bits_comp = read_uleb128(body, &mut off)? as usize;
    let _bits_count = read_uleb128(body, &mut off)?;
    if off + bits_comp > pos_start {
        bail!("truncated FST frame bits");
    }
    if bits_comp != bits_uncomp {
        bail!("compressed FST frame bits not supported");
    }
    let frame_bits = &body[off..off + bits_comp];
    off += bits_comp;

    // Precompute byte offsets of each handle's initial bits in `frame_bits`.
    let mut bit_offsets = Vec::with_capacity(widths.len());
    let mut boff = 0usize;
    for &w in widths {
        bit_offsets.push(boff);
        boff += w;
    }

    // 4. Parse wave section header.
    let waves_count = read_uleb128(body, &mut off)? as usize;
    if off >= pos_start {
        bail!("truncated FST wave header");
    }
    let pack_type = body[off];
    off += 1;
    let wave_base = off;
    let wave_blob = &body[wave_base..pos_start];

    // 5. Decode DYN_ALIAS2 position table -> (handle -> Option<byte_offset_0based>).
    let mut offsets: Vec<Option<usize>> = Vec::with_capacity(waves_count);
    let mut poff = 0usize;
    let mut prev_pos: i64 = 0;
    while poff < pos_slice.len() && offsets.len() < waves_count {
        let v = read_sleb128(pos_slice, &mut poff)?;
        if v == 0 {
            offsets.push(None);
        } else if (v & 1) == 1 {
            let delta = v >> 1;
            prev_pos += delta;
            if prev_pos <= 0 {
                bail!("invalid FST wave offset {prev_pos}");
            }
            offsets.push(Some((prev_pos - 1) as usize));
        } else if v > 0 {
            let zeros = (v >> 1) as usize;
            for _ in 0..zeros {
                offsets.push(None);
            }
        } else {
            // Dynamic alias pointing to an earlier handle `(-v >> 1) - 1`.
            let alias_idx = ((-v) >> 1) as usize;
            let aliased = alias_idx
                .checked_sub(1)
                .and_then(|i| offsets.get(i).copied().flatten());
            offsets.push(aliased);
        }
    }
    // Map each distinct starting byte offset to its ending byte offset in `wave_blob`.
    let mut sorted_starts: Vec<usize> = offsets.iter().filter_map(|&o| o).collect();
    sorted_starts.sort_unstable();
    sorted_starts.dedup();

    let end_of = |start: usize| -> usize {
        match sorted_starts.binary_search(&start) {
            Ok(idx) => sorted_starts
                .get(idx + 1)
                .copied()
                .unwrap_or(wave_blob.len()),
            Err(_) => wave_blob.len(),
        }
    };

    // 6. Decode ONLY requested handles!
    for (handle, runs) in handle_runs.iter_mut() {
        let h = *handle;
        if !want_handles.contains(&h) || h >= widths.len() {
            continue;
        }
        let w = widths[h];
        let mut cur_val = if bit_offsets[h] + w <= frame_bits.len() {
            String::from_utf8_lossy(&frame_bits[bit_offsets[h]..bit_offsets[h] + w]).into_owned()
        } else {
            "x".repeat(w)
        };

        // Track state carried up to `first_frame` and emit changes inside `[first_frame..=last_frame]`.
        let mut emitted_initial = false;
        let record_at = |runs: &mut Vec<(usize, String)>, f: usize, val: &str| match runs.last_mut()
        {
            Some((pf, pv)) if *pf == f => pv.replace_range(.., val),
            Some((_, pv)) if pv == val => {}
            _ => runs.push((f, val.to_string())),
        };

        if let Some(Some(start)) = offsets.get(h) {
            let end = end_of(*start);
            let stream = &wave_blob[*start..end];
            if !stream.is_empty() {
                let mut soff = 0usize;
                let uncomp = read_uleb128(stream, &mut soff)? as usize;
                let decoded_owned;
                let raw: &[u8] = if uncomp == 0 {
                    &stream[soff..]
                } else if pack_type == b'4' {
                    decoded_owned = lz4::decompress(&stream[soff..], uncomp)?;
                    &decoded_owned
                } else {
                    bail!("unsupported FST VC pack type '{}'", pack_type as char);
                };

                let mut roff = 0usize;
                let mut tidx = 0usize;
                while roff < raw.len() {
                    let (delta, next_val) = if w == 1 {
                        decode_1bit_change(raw, &mut roff)?
                    } else {
                        decode_vec_change(raw, &mut roff, w)?
                    };
                    tidx += delta as usize;
                    let Some(&t) = times.get(tidx) else {
                        break;
                    };
                    // Only frame-aligned timestamps (multiples of 10) represent cycle frames.
                    if t % 10 != 0 {
                        cur_val = next_val;
                        continue;
                    }
                    let f = (t / 10) as usize;
                    if f < first_frame {
                        cur_val = next_val;
                    } else if f <= last_frame {
                        if !emitted_initial && f > first_frame && runs.is_empty() {
                            record_at(runs, first_frame, &cur_val);
                        }
                        emitted_initial = true;
                        cur_val = next_val;
                        record_at(runs, f, &cur_val);
                    } else {
                        break;
                    }
                }
            }
        }
        if !emitted_initial && runs.is_empty() {
            record_at(runs, first_frame, &cur_val);
        }
    }
    Ok(())
}

fn decode_1bit_change(raw: &[u8], off: &mut usize) -> Result<(u64, String)> {
    let v = read_uleb128(raw, off)?;
    if (v & 1) == 0 {
        let bit = if ((v >> 1) & 1) == 1 { "1" } else { "0" };
        Ok((v >> 2, bit.to_string()))
    } else {
        let code = (v >> 1) & 0x7;
        let ch = match code {
            0 => "x",
            1 => "z",
            2 => "h",
            3 => "u",
            4 => "w",
            5 => "l",
            6 => "-",
            _ => "x",
        };
        Ok((v >> 4, ch.to_string()))
    }
}

fn decode_vec_change(raw: &[u8], off: &mut usize, width: usize) -> Result<(u64, String)> {
    let v = read_uleb128(raw, off)?;
    let delta = v >> 1;
    if (v & 1) == 0 {
        // 2-state MSB-first packed bytes
        let nbytes = width.div_ceil(8);
        if *off + nbytes > raw.len() {
            bail!("truncated FST 2-state vector value");
        }
        let bytes = &raw[*off..*off + nbytes];
        *off += nbytes;
        let mut s = String::with_capacity(width);
        for i in 0..width {
            let b = (bytes[i / 8] >> (7 - (i % 8))) & 1;
            s.push(if b == 1 { '1' } else { '0' });
        }
        Ok((delta, s))
    } else {
        // Raw ASCII bits
        if *off + width > raw.len() {
            bail!("truncated FST 4-state vector value");
        }
        let s = String::from_utf8_lossy(&raw[*off..*off + width]).into_owned();
        *off += width;
        Ok((delta, s))
    }
}
