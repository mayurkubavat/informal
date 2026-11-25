//! Pure-Rust GTKWave-compatible FST (`.fst`) waveform writer (decision D10).
//!
//! Writes standard chunked multi-block FST files:
//! - Block 0 (`FST_BL_HDR`, type `0`): header with time bounds, timescale
//!   (`1ns`), signal counts, and VC block count.
//! - Blocks 1..K (`FST_BL_VCDATA_DYN_ALIAS2`, type `8`): chunked into
//!   `block_frames` cycles per block (default `256`), each carrying its own
//!   initial-state frame, per-signal value-change stream, signed-delta position
//!   table, and delta-encoded time table — enabling O(1) random-access
//!   windowed streaming in [`crate::fst_read::FstReader`], GTKWave, and Surfer.
//! - Geometry block (`FST_BL_GEOM`, type `3`): per-handle bit widths.
//! - Hierarchy block (`FST_BL_HIER_LZ4`, type `6`): LZ4-compressed scope tree,
//!   variable declarations, and trace comments (`informal trace of ...`,
//!   liveness loop annotations).

use crate::{bits_text, lz4, TraceData};

/// Default frames per FST value-change block for random-access streaming.
pub const DEFAULT_BLOCK_FRAMES: usize = 256;

pub(crate) const FST_BL_HDR: u8 = 0;
pub(crate) const FST_BL_GEOM: u8 = 3;
pub(crate) const FST_BL_HIER_LZ4: u8 = 6;
pub(crate) const FST_BL_VCDATA_DYN_ALIAS2: u8 = 8;

pub(crate) const FST_ST_VCD_MODULE: u8 = 0;
pub(crate) const FST_VT_VCD_REG: u8 = 5;
pub(crate) const FST_VT_VCD_WIRE: u8 = 16;
pub(crate) const FST_HT_ATTRBEGIN: u8 = 0xfc;
pub(crate) const FST_ST_VCD_SCOPE: u8 = 0xfe;
pub(crate) const FST_ST_VCD_UPSCOPE: u8 = 0xff;
pub(crate) const FST_AT_MISC: u8 = 0;
pub(crate) const FST_MT_COMMENT: u8 = 0;
pub(crate) const FST_VD_IMPLICIT: u8 = 0;

/// Encode an unsigned integer as FST LEB128 varint.
pub(crate) fn write_uleb128(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
            out.push(b);
        } else {
            out.push(b);
            break;
        }
    }
}

/// Encode a signed integer as FST signed LEB128 varint (used by `DYN_ALIAS2`
/// position tables).
pub(crate) fn write_sleb128(out: &mut Vec<u8>, mut v: i64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        let done = (v == 0 && (b & 0x40) == 0) || (v == -1 && (b & 0x40) != 0);
        if done {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
}

/// Serialize `data` into a GTKWave-standard FST byte vector with the default
/// [`DEFAULT_BLOCK_FRAMES`] chunk size.
pub fn write_fst(data: &TraceData) -> Vec<u8> {
    write_fst_chunked(data, DEFAULT_BLOCK_FRAMES)
}

/// Serialize `data` into a GTKWave-standard FST byte vector split into VC
/// blocks of `block_frames` cycles each (minimum 1).
pub fn write_fst_chunked(data: &TraceData, block_frames: usize) -> Vec<u8> {
    let block_frames = block_frames.max(1);
    let total_frames = data.frames.max(1);
    let end_time = (10 * data.frames) as u64;

    // Order variables by first-seen scope (matching `write_vcd` and hierarchy
    // declaration order) so sequential FST handle IDs match geometry and VC
    // blocks 1-to-1.
    let mut scopes: Vec<&[String]> = Vec::new();
    for v in &data.vars {
        if !scopes.contains(&v.scope.as_slice()) {
            scopes.push(&v.scope);
        }
    }
    let mut var_order: Vec<usize> = Vec::with_capacity(data.vars.len());
    for sc in &scopes {
        for (i, v) in data.vars.iter().enumerate() {
            if v.scope.as_slice() == *sc {
                var_order.push(i);
            }
        }
    }
    let num_vars = var_order.len();

    let widths: Vec<usize> = var_order
        .iter()
        .map(|&vi| {
            if vi == 0 {
                1
            } else {
                data.vars[vi].bits.first().map_or(1, Vec::len).max(1)
            }
        })
        .collect();

    let (hier_raw, num_scopes) = build_hierarchy(data, &scopes, &var_order, &widths);
    let block_count = total_frames.div_ceil(block_frames);

    let mut out = Vec::with_capacity(512 + num_vars * total_frames * 2);
    write_header(
        &mut out,
        0,
        end_time,
        num_scopes as u64,
        num_vars as u64,
        block_count as u64,
    );

    // Emit one VC block per [b_start..b_end) frame chunk.
    for bi in 0..block_count {
        let f_start = bi * block_frames;
        let f_end = ((bi + 1) * block_frames).min(data.frames);
        let is_last = bi + 1 == block_count;
        write_vc_block(&mut out, data, &var_order, &widths, f_start, f_end, is_last);
    }

    write_geometry(&mut out, &widths);
    write_hierarchy_lz4(&mut out, &hier_raw);
    out
}

fn write_block(out: &mut Vec<u8>, btype: u8, body: &[u8]) {
    out.push(btype);
    // FST block length includes the 8-byte length field itself.
    let blen = (body.len() as u64) + 8;
    out.extend_from_slice(&blen.to_be_bytes());
    out.extend_from_slice(body);
}

fn write_header(
    out: &mut Vec<u8>,
    start_time: u64,
    end_time: u64,
    num_scopes: u64,
    num_vars: u64,
    num_vc_blocks: u64,
) {
    let mut body = Vec::with_capacity(321);
    body.extend_from_slice(&start_time.to_be_bytes());
    body.extend_from_slice(&end_time.to_be_bytes());
    // Endianness probe constant `M_E` in native (little-endian on x86_64/arm64) IEEE-754.
    body.extend_from_slice(&std::f64::consts::E.to_le_bytes());
    body.extend_from_slice(&(128u64 * 1024 * 1024).to_be_bytes()); // writer_memory_used
    body.extend_from_slice(&num_scopes.to_be_bytes());
    body.extend_from_slice(&num_vars.to_be_bytes()); // num_hierarchy_vars
    body.extend_from_slice(&num_vars.to_be_bytes()); // num_vars
    body.extend_from_slice(&num_vc_blocks.to_be_bytes());
    body.push((-9i8) as u8); // 1ns timescale (10^-9 s)

    let mut writer = [0u8; 128];
    let wbytes = b"InFormal";
    writer[..wbytes.len()].copy_from_slice(wbytes);
    body.extend_from_slice(&writer);

    let mut date = [0u8; 26];
    let dbytes = b"InFormal FST\n";
    date[..dbytes.len()].copy_from_slice(dbytes);
    body.extend_from_slice(&date);

    body.extend_from_slice(&[0u8; 93]); // reserved
    body.push(0u8); // FST_FT_VERILOG
    body.extend_from_slice(&0i64.to_be_bytes()); // timezero
    debug_assert_eq!(body.len(), 321);
    write_block(out, FST_BL_HDR, &body);
}

fn clock_bit(data: &TraceData, frame: usize) -> u8 {
    match data.vars[0]
        .bits
        .get(frame)
        .and_then(|b| b.first().copied().flatten())
    {
        Some(true) => b'1',
        Some(false) => b'0',
        None if data.half_cycle => {
            if frame % 2 == 1 {
                b'1'
            } else {
                b'0'
            }
        }
        None => b'1',
    }
}

fn var_bits_at(data: &TraceData, var_idx: usize, frame: usize, width: usize) -> Vec<u8> {
    if var_idx == 0 {
        return vec![clock_bit(data, frame)];
    }
    match data.vars[var_idx].bits.get(frame) {
        Some(bits) => bits_text(bits).into_bytes(),
        None => vec![b'x'; width],
    }
}

fn write_vc_block(
    out: &mut Vec<u8>,
    data: &TraceData,
    var_order: &[usize],
    widths: &[usize],
    f_start: usize,
    f_end: usize,
    is_last: bool,
) {
    let num_vars = var_order.len();
    let start_time = (10 * f_start) as u64;
    let end_time = if is_last {
        (10 * data.frames) as u64
    } else {
        (10 * f_end) as u64
    };

    // Build timestamp table for this block.
    let mut times: Vec<u64> = Vec::with_capacity((f_end - f_start) * 2 + 2);
    for f in f_start..f_end {
        times.push((10 * f) as u64);
        if !data.half_cycle {
            times.push((10 * f + 5) as u64);
        }
    }
    if is_last {
        let final_t = (10 * data.frames) as u64;
        if times.last().copied() != Some(final_t) {
            times.push(final_t);
        }
    }
    if times.is_empty() {
        times.push(start_time);
    }

    // Frame initial values:
    // - Block 0 starts from 'x' so frame 0 emits the initial dumpvars values.
    // - Subsequent blocks carry the exact value of every signal at `f_start - 1`
    //   so random-access readers seeking straight to block `bi` know every
    //   signal's state at `f_start` without touching earlier blocks!
    let mut frame_bits: Vec<u8> = Vec::with_capacity(widths.iter().sum());
    let mut prev_vals: Vec<Vec<u8>> = Vec::with_capacity(num_vars);
    for (hi, (&vi, &w)) in var_order.iter().zip(widths.iter()).enumerate() {
        let _ = hi;
        let init = if f_start == 0 {
            vec![b'x'; w]
        } else if vi == 0 && !data.half_cycle {
            // Clock fell to '0' at `10*(f_start-1) + 5` just before `10*f_start`.
            vec![b'0']
        } else {
            var_bits_at(data, vi, f_start - 1, w)
        };
        frame_bits.extend_from_slice(&init);
        prev_vals.push(init);
    }

    // Encode each signal's value-change stream within this block.
    let mut wave_streams: Vec<Vec<u8>> = Vec::with_capacity(num_vars);
    let mut total_wave_mem: u64 = 0;

    for (hi, (&vi, &w)) in var_order.iter().zip(widths.iter()).enumerate() {
        let mut raw = Vec::new();
        let mut prev_tidx: usize = 0;
        let mut last = prev_vals[hi].clone();

        if vi == 0 {
            // Clock lane: rises at 10*f, falls at 10*f + 5 (when !half_cycle).
            let mut tidx = 0usize;
            for f in f_start..f_end {
                let lv = clock_bit(data, f);
                if f_start == 0 && f == 0 || last[0] != lv {
                    let delta = (tidx - prev_tidx) as u64;
                    encode_1bit_change(&mut raw, delta, lv);
                    prev_tidx = tidx;
                    last[0] = lv;
                }
                tidx += 1;
                if !data.half_cycle {
                    if last[0] != b'0' {
                        let delta = (tidx - prev_tidx) as u64;
                        encode_1bit_change(&mut raw, delta, b'0');
                        prev_tidx = tidx;
                        last[0] = b'0';
                    }
                    tidx += 1;
                }
            }
        } else {
            let step = if data.half_cycle { 1 } else { 2 };
            for (k, f) in (f_start..f_end).enumerate() {
                let tidx = k * step;
                let cur = var_bits_at(data, vi, f, w);
                if (f_start == 0 && f == 0) || cur != last {
                    let delta = (tidx - prev_tidx) as u64;
                    if w == 1 {
                        encode_1bit_change(&mut raw, delta, cur[0]);
                    } else {
                        encode_vec_change(&mut raw, delta, &cur);
                    }
                    prev_tidx = tidx;
                    last = cur;
                }
            }
        }

        total_wave_mem += raw.len() as u64;
        // Pack stream: leading varint `uncomp_len` (0 means stored raw uncompressed).
        let mut packed = Vec::new();
        if raw.is_empty() {
            // Empty stream: zero length (position = 0 in position table).
        } else {
            let comp = lz4::compress(&raw);
            if comp.len() + 4 < raw.len() {
                write_uleb128(&mut packed, raw.len() as u64);
                packed.extend_from_slice(&comp);
            } else {
                packed.push(0u8); // 0 = uncompressed
                packed.extend_from_slice(&raw);
            }
        }
        wave_streams.push(packed);
    }

    // Build wave blob + signed-delta position table (FST_BL_VCDATA_DYN_ALIAS2).
    // In DYN_ALIAS2, position entry for handle i (when non-zero) is encoded as
    // `((delta_from_prev_pos) << 1) | 1`, where `prev_pos` starts at 0 and
    // streams are addressed by 1-based byte offset inside the wave payload.
    let mut wave_blob = Vec::new();
    let mut pos_table = Vec::new();
    let mut prev_pos: i64 = 0;
    let mut run_zeros: u64 = 0;

    let flush_zeros = |pt: &mut Vec<u8>, zeros: &mut u64| {
        if *zeros > 0 {
            write_sleb128(pt, (*zeros as i64) << 1);
            *zeros = 0;
        }
    };

    for stream in &wave_streams {
        if stream.is_empty() {
            run_zeros += 1;
            continue;
        }
        flush_zeros(&mut pos_table, &mut run_zeros);
        let cur_pos = (wave_blob.len() as i64) + 1;
        let delta = cur_pos - prev_pos;
        write_sleb128(&mut pos_table, (delta << 1) | 1);
        prev_pos = cur_pos;
        wave_blob.extend_from_slice(stream);
    }
    flush_zeros(&mut pos_table, &mut run_zeros);

    // Encode delta time table.
    let mut time_raw = Vec::with_capacity(times.len() * 2);
    let mut prev_t = 0u64;
    for &t in &times {
        write_uleb128(&mut time_raw, t - prev_t);
        prev_t = t;
    }
    let time_uncomp_len = time_raw.len() as u64;
    let time_comp = lz4::compress(&time_raw);
    let (time_bytes, time_comp_len) = if time_comp.len() + 8 < time_raw.len() {
        // Wait: GTKWave FST time tables use zlib (not LZ4) when comp < uncomp!
        // Storing time table uncompressed (`comp == uncomp`) is 100% standard
        // and universally decoded by GTKWave, Surfer, and FstReader with zero zlib dep.
        (time_raw.as_slice(), time_uncomp_len)
    } else {
        (time_raw.as_slice(), time_uncomp_len)
    };
    let _ = time_comp;

    // Frame bits: stored uncompressed (`bits_comp == bits_uncomp`) for zero-zlib
    // universal GTKWave/Surfer compatibility (each frame is tiny: total_bits bytes).
    let bits_uncomp_len = frame_bits.len() as u64;
    let bits_comp_len = bits_uncomp_len;

    let mut body = Vec::with_capacity(
        64 + frame_bits.len() + wave_blob.len() + pos_table.len() + time_bytes.len(),
    );
    body.extend_from_slice(&start_time.to_be_bytes());
    body.extend_from_slice(&end_time.to_be_bytes());
    body.extend_from_slice(&total_wave_mem.max(16).to_be_bytes());

    // Frame section
    write_uleb128(&mut body, bits_uncomp_len);
    write_uleb128(&mut body, bits_comp_len);
    write_uleb128(&mut body, num_vars as u64);
    body.extend_from_slice(&frame_bits);

    // Wave section
    write_uleb128(&mut body, num_vars as u64);
    body.push(b'4'); // pack_type = '4' (LZ4)
    body.extend_from_slice(&wave_blob);

    // Position table + 8-byte position_length
    body.extend_from_slice(&pos_table);
    body.extend_from_slice(&(pos_table.len() as u64).to_be_bytes());

    // Time table + 24-byte trailer (uncomp_len, comp_len, count)
    body.extend_from_slice(time_bytes);
    body.extend_from_slice(&time_uncomp_len.to_be_bytes());
    body.extend_from_slice(&time_comp_len.to_be_bytes());
    body.extend_from_slice(&(times.len() as u64).to_be_bytes());

    write_block(out, FST_BL_VCDATA_DYN_ALIAS2, &body);
}

/// Encode a 1-bit scalar value change in FST VC format:
/// - `'0'` / `'1'`: `(delta << 2) | (bit << 1) | 0`
/// - `'x'` / `'z'` / other: `(delta << 4) | (code << 1) | 1` (`x=0`, `z=1`)
fn encode_1bit_change(out: &mut Vec<u8>, delta: u64, bit: u8) {
    match bit {
        b'0' => write_uleb128(out, delta << 2),
        b'1' => write_uleb128(out, (delta << 2) | 2),
        b'x' | b'X' => write_uleb128(out, (delta << 4) | 1),
        b'z' | b'Z' => write_uleb128(out, (delta << 4) | 3),
        _ => write_uleb128(out, (delta << 4) | 1),
    }
}

/// Encode a multi-bit vector value change in FST VC format:
/// - Pure 2-state (`0`/`1`): `(delta << 1) | 0`, followed by `ceil(w/8)` MSB-first packed bytes.
/// - Contains `x`/`z`: `(delta << 1) | 1`, followed by `w` raw ASCII bytes.
fn encode_vec_change(out: &mut Vec<u8>, delta: u64, bits: &[u8]) {
    let is_two_state = bits.iter().all(|&b| b == b'0' || b == b'1');
    if is_two_state {
        write_uleb128(out, delta << 1);
        let nbytes = bits.len().div_ceil(8);
        for byte_idx in 0..nbytes {
            let mut acc = 0u8;
            for bit_in_byte in 0..8 {
                let idx = byte_idx * 8 + bit_in_byte;
                if idx < bits.len() && bits[idx] == b'1' {
                    acc |= 1 << (7 - bit_in_byte);
                }
            }
            out.push(acc);
        }
    } else {
        write_uleb128(out, (delta << 1) | 1);
        out.extend_from_slice(bits);
    }
}

fn write_geometry(out: &mut Vec<u8>, widths: &[usize]) {
    let mut raw = Vec::with_capacity(widths.len() * 2);
    for &w in widths {
        write_uleb128(&mut raw, w as u64);
    }
    let mut body = Vec::with_capacity(16 + raw.len());
    body.extend_from_slice(&(raw.len() as u64).to_be_bytes()); // uncompressed_len
    body.extend_from_slice(&(widths.len() as u64).to_be_bytes()); // count
    body.extend_from_slice(&raw); // uncompressed (`comp_len == uncomp_len`)
    write_block(out, FST_BL_GEOM, &body);
}

fn write_hierarchy_lz4(out: &mut Vec<u8>, raw: &[u8]) {
    let comp = lz4::compress(raw);
    let mut body = Vec::with_capacity(8 + comp.len());
    body.extend_from_slice(&(raw.len() as u64).to_be_bytes());
    body.extend_from_slice(&comp);
    write_block(out, FST_BL_HIER_LZ4, &body);
}

fn build_hierarchy(
    data: &TraceData,
    scopes: &[&[String]],
    var_order: &[usize],
    widths: &[usize],
) -> (Vec<u8>, usize) {
    let mut raw = Vec::with_capacity(256 + var_order.len() * 24);
    let unit = if data.half_cycle {
        "model steps"
    } else {
        "cycles"
    };
    let header_comment = format!("informal trace of {}: {} {unit}", data.prop, data.frames);
    push_comment(&mut raw, &header_comment);
    for note in &data.notes {
        push_comment(&mut raw, note);
    }

    let mut num_scopes = 0usize;
    let mut cursor = 0usize;
    for sc in scopes {
        for part in sc.iter() {
            raw.push(FST_ST_VCD_SCOPE);
            raw.push(FST_ST_VCD_MODULE);
            raw.extend_from_slice(part.as_bytes());
            raw.push(0);
            raw.push(0); // component name empty
            num_scopes += 1;
        }
        while cursor < var_order.len() && data.vars[var_order[cursor]].scope.as_slice() == *sc {
            let vi = var_order[cursor];
            let v = &data.vars[vi];
            let w = widths[cursor];
            let vtype = if vi == 0 {
                FST_VT_VCD_WIRE
            } else {
                FST_VT_VCD_REG
            };
            raw.push(vtype);
            raw.push(FST_VD_IMPLICIT);
            if w > 1 {
                let nm = format!("{} [{}:0]", v.name, w - 1);
                raw.extend_from_slice(nm.as_bytes());
            } else {
                raw.extend_from_slice(v.name.as_bytes());
            }
            raw.push(0);
            write_uleb128(&mut raw, w as u64);
            write_uleb128(&mut raw, 0); // alias = 0 -> assign next sequential handle
            cursor += 1;
        }
        raw.extend(std::iter::repeat_n(FST_ST_VCD_UPSCOPE, sc.len()));
    }
    (raw, num_scopes)
}

fn push_comment(out: &mut Vec<u8>, text: &str) {
    out.push(FST_HT_ATTRBEGIN);
    out.push(FST_AT_MISC);
    out.push(FST_MT_COMMENT);
    out.extend_from_slice(text.as_bytes());
    out.push(0);
    write_uleb128(out, 0);
}
