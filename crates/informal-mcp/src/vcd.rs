//! A counterexample or cover trace as data: the VCD the session wrote
//! (`informal_fpv::trace`, docs/ui/trace-format.md), read back into one
//! value per signal and frame.

use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

/// Lightweight trace metadata returned by `read_trace_meta` before streaming
/// any waveform value-change windows.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TraceMeta {
    pub frames: usize,
    pub steps: bool,
    pub format: &'static str,
    pub block_count: usize,
    pub signals: Vec<TraceSignalMeta>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TraceSignalMeta {
    pub name: String,
    pub path: String,
    pub width: usize,
    pub clock: bool,
}

/// Resolve the `.fst` sibling for `path` if it exists on disk.
pub fn fst_for(path: &Path) -> Option<std::path::PathBuf> {
    let cand = informal_fpv::trace::fst_sibling(path);
    cand.is_file().then_some(cand)
}

/// Read trace metadata without loading value changes (uses FST header/hierarchy
/// when `.fst` is present, else scans the VCD header).
pub fn read_meta(path: &Path) -> Result<TraceMeta, String> {
    if let Some(fst) = fst_for(path) {
        let reader =
            informal_fpv::trace::fst_read::FstReader::open(&fst).map_err(|e| format!("{e:#}"))?;
        let m = reader.meta();
        let signals = m
            .signals
            .iter()
            .map(|s| TraceSignalMeta {
                name: s.name.rsplit('.').next().unwrap_or(&s.name).to_string(),
                path: s.name.clone(),
                width: s.width,
                clock: s.clock,
            })
            .collect();
        return Ok(TraceMeta {
            frames: m.frames,
            steps: m.steps,
            format: "fst",
            block_count: m.block_count,
            signals,
        });
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let t = parse(&text)?;
    let signals = t
        .signals
        .iter()
        .map(|s| TraceSignalMeta {
            name: s.name.rsplit('.').next().unwrap_or(&s.name).to_string(),
            path: s.name.clone(),
            width: s.width,
            clock: s.clock,
        })
        .collect();
    Ok(TraceMeta {
        frames: t.frames,
        steps: t.steps,
        format: "vcd",
        block_count: 1,
        signals,
    })
}

/// Encode a full trace into binary `IFTR` v1, preferring the streamed `.fst`
/// sibling when present so VCD text is never parsed for large traces.
pub fn encode_iftr_from_path(path: &Path) -> Result<Vec<u8>, String> {
    if let Some(fst) = fst_for(path) {
        let mut reader =
            informal_fpv::trace::fst_read::FstReader::open(&fst).map_err(|e| format!("{e:#}"))?;
        let last = reader.meta().frames.saturating_sub(1);
        return reader
            .encode_window_iftr(0, last, None)
            .map_err(|e| format!("{e:#}"));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    encode_iftr(&text)
}

/// Stream ONLY `[first_frame..=last_frame]` (and optional signal subset) as
/// binary `IFTR` v1 bytes. Uses O(window) random-access FST block reads when
/// `.fst` is present, falling back to VCD slicing otherwise.
pub fn encode_window_iftr_from_path(
    path: &Path,
    first_frame: usize,
    last_frame: usize,
    signals: Option<&[String]>,
) -> Result<Vec<u8>, String> {
    if let Some(fst) = fst_for(path) {
        let mut reader =
            informal_fpv::trace::fst_read::FstReader::open(&fst).map_err(|e| format!("{e:#}"))?;
        let selected: Option<Vec<usize>> = signals.map(|wanted| {
            reader
                .meta()
                .signals
                .iter()
                .enumerate()
                .filter(|(_, s)| s.clock || wanted.iter().any(|w| w == &s.name))
                .map(|(idx, _)| idx)
                .collect()
        });
        return reader
            .encode_window_iftr(first_frame, last_frame, selected.as_deref())
            .map_err(|e| format!("{e:#}"));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    encode_window_iftr_from_vcd(&text, first_frame, last_frame, signals)
}

fn encode_window_iftr_from_vcd(
    text: &str,
    first_frame: usize,
    last_frame: usize,
    signals: Option<&[String]>,
) -> Result<Vec<u8>, String> {
    let full = decode_iftr(&encode_iftr(text)?, false)?;
    let max_f = full.frames.saturating_sub(1);
    let first = first_frame.min(max_f);
    let last = last_frame.min(max_f).max(first);
    let mut win_sigs = Vec::new();
    for s in &full.signals {
        if let Some(wanted) = signals {
            if !s.clock && !wanted.iter().any(|w| w == &s.name) {
                continue;
            }
        }
        let mut runs = Vec::new();
        let mut prev: Option<&str> = None;
        for (f, val) in s.values.iter().enumerate().take(last + 1).skip(first) {
            let v = val.as_str();
            if prev != Some(v) {
                runs.push((f, v.to_string()));
                prev = Some(v);
            }
        }
        win_sigs.push(informal_fpv::trace::fst_read::WindowSignal {
            name: s.name.clone(),
            width: s.width,
            clock: s.clock,
            runs,
        });
    }
    let win = informal_fpv::trace::fst_read::TraceWindow {
        total_frames: full.frames,
        steps: full.steps,
        first_frame: first,
        last_frame: last,
        signals: win_sigs,
        notes: full.notes,
        blocks_read: 1,
    };
    Ok(informal_fpv::trace::fst_read::encode_trace_window_iftr(
        &win,
    ))
}

/// One signal of a trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    /// Full name, scopes joined with dots (`fifo.u_wp.p`).
    pub name: String,
    pub width: usize,
    /// The clock lane (the trace's first variable).
    pub clock: bool,
    /// One value per frame: bits, most significant first; `x` when the
    /// trace gives none.
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trace {
    /// Clock cycles, or model steps when `steps` is set.
    pub frames: usize,
    /// One frame is one model step (half a period of the fastest clock):
    /// a design on both clock edges or with several clocks.
    pub steps: bool,
    pub signals: Vec<Signal>,
    /// What the trace says about itself beyond the values (the loop of a
    /// liveness counterexample).
    pub notes: Vec<String>,
}

/// Read a VCD written by informal: values change at multiples of 10 time
/// units, one frame each; the clock's fall at 10f + 5 is not a frame.
pub fn parse(text: &str) -> Result<Trace, String> {
    let mut scope: Vec<&str> = Vec::new();
    let mut signals: Vec<Signal> = Vec::new();
    let mut by_id: HashMap<&str, usize> = HashMap::new();
    let mut notes = Vec::new();
    let mut frames = None;
    let mut steps = false;
    let mut lines = text.lines();
    for l in lines.by_ref() {
        let w: Vec<&str> = l.split_whitespace().collect();
        match w.first().copied() {
            Some("$comment") => {
                let body = w[1..w.len().saturating_sub(1)].join(" ");
                match body.strip_prefix("informal trace of ") {
                    // "<prop>: <n> cycles" or "<prop>: <n> model steps"
                    Some(rest) => {
                        let count = rest.rsplit(": ").next().unwrap_or("");
                        frames = count.split(' ').next().and_then(|n| n.parse().ok());
                        steps = count.ends_with("model steps");
                    }
                    None => notes.push(body),
                }
            }
            Some("$scope") => scope.push(w.get(2).copied().unwrap_or("")),
            Some("$upscope") => {
                scope.pop();
            }
            Some("$var") if w.len() >= 6 => {
                let width: usize = w[2].parse().map_err(|_| format!("bad $var: {l}"))?;
                by_id.insert(w[3], signals.len());
                signals.push(Signal {
                    name: scope
                        .iter()
                        .chain([&w[4]])
                        .copied()
                        .collect::<Vec<_>>()
                        .join("."),
                    width,
                    clock: signals.is_empty(),
                    values: Vec::new(),
                });
            }
            Some("$enddefinitions") => break,
            _ => {}
        }
    }
    // (frame, signal, bits) in file order.
    let mut changes: Vec<(usize, usize, String)> = Vec::new();
    let mut frame = None;
    let mut last = 0;
    for l in lines {
        if let Some(t) = l.strip_prefix('#') {
            let t: usize = t.trim().parse().map_err(|_| format!("bad time: {l}"))?;
            frame = t.is_multiple_of(10).then_some(t / 10);
            continue;
        }
        let Some(f) = frame else { continue };
        let (bits, id) = match l.strip_prefix('b') {
            Some(rest) => match rest.split_once(' ') {
                Some((bits, id)) => (bits, id.trim()),
                None => continue,
            },
            None if l.starts_with(['0', '1', 'x', 'z']) => l.split_at(1),
            None => continue,
        };
        if let Some(&i) = by_id.get(id) {
            changes.push((f, i, bits.to_string()));
            last = last.max(f);
        }
    }
    if signals.is_empty() {
        return Err("no signals in the trace".into());
    }
    let frames = frames.unwrap_or(last + 1);
    for s in &mut signals {
        s.values = vec!["x".repeat(s.width); frames];
    }
    // Carry each value forward to the frames after its change.
    for (f, i, bits) in changes {
        let s = &mut signals[i];
        let bits = format!("{bits:0>width$}", width = s.width);
        for v in s.values.iter_mut().skip(f) {
            v.clone_from(&bits);
        }
    }
    // A clock that rises with every cycle has nothing to say per frame.
    if !steps {
        signals.retain(|s| !s.clock);
    }
    Ok(Trace {
        frames,
        steps,
        signals,
        notes,
    })
}

/// Bits (most significant first) as the replies give them: one bit as it
/// is, a word in hex. A digit with an unknown bit is `x`.
pub fn value(bits: &str) -> String {
    if bits.len() == 1 {
        return bits.to_string();
    }
    let pad = (4 - bits.len() % 4) % 4;
    let padded = format!("{}{bits}", "0".repeat(pad));
    padded
        .as_bytes()
        .chunks(4)
        .map(|c| {
            let digit = c.iter().try_fold(0u32, |acc, b| match b {
                b'0' => Some(acc * 2),
                b'1' => Some(acc * 2 + 1),
                _ => None,
            });
            digit.and_then(|d| char::from_digit(d, 16)).unwrap_or('x')
        })
        .collect()
}

/// Compact binary run-length waveform wire format (`IFTR` v1) for zero-JSON
/// Tauri IPC (`tauri::ipc::Response` -> `Uint8Array`).
///
/// Layout (all integers little-endian):
/// - `[0..4]`: `"IFTR"`
/// - `[4]`: `version = 1`
/// - `[5]`: `flags` (`bit 0 = steps`)
/// - `[6..10]`: `u32` `frames`
/// - `[10..14]`: `u32` `signal_count`
/// - Per signal:
///   - `u16` `name_len`, `[u8; name_len]` UTF-8 path
///   - `u16` `width`
///   - `u8` `sig_flags` (`bit 0 = clock`)
///   - `u32` `run_count`
///   - Per run: `u32` `start_frame`, `u16` `bits_len`, `[u8; bits_len]` MSB-first bit string
pub fn encode_iftr(text: &str) -> Result<Vec<u8>, String> {
    let mut scope: Vec<&str> = Vec::new();
    struct RawSig {
        name: String,
        width: usize,
        clock: bool,
        runs: Vec<(u32, String)>,
    }
    let mut signals: Vec<RawSig> = Vec::new();
    let mut by_id: HashMap<&str, usize> = HashMap::new();
    let mut frames_comment: Option<u32> = None;
    let mut steps = false;
    let mut lines = text.lines();
    for l in lines.by_ref() {
        let w: Vec<&str> = l.split_whitespace().collect();
        match w.first().copied() {
            Some("$comment") => {
                let body = w[1..w.len().saturating_sub(1)].join(" ");
                if let Some(rest) = body.strip_prefix("informal trace of ") {
                    let count = rest.rsplit(": ").next().unwrap_or("");
                    frames_comment = count.split(' ').next().and_then(|n| n.parse().ok());
                    steps = count.ends_with("model steps");
                }
            }
            Some("$scope") => scope.push(w.get(2).copied().unwrap_or("")),
            Some("$upscope") => {
                scope.pop();
            }
            Some("$var") if w.len() >= 6 => {
                let width: usize = w[2].parse().map_err(|_| format!("bad $var: {l}"))?;
                by_id.insert(w[3], signals.len());
                signals.push(RawSig {
                    name: scope
                        .iter()
                        .chain([&w[4]])
                        .copied()
                        .collect::<Vec<_>>()
                        .join("."),
                    width,
                    clock: signals.is_empty(),
                    runs: Vec::new(),
                });
            }
            Some("$enddefinitions") => break,
            _ => {}
        }
    }
    if signals.is_empty() {
        return Err("no signals in the trace".into());
    }
    let mut frame: Option<u32> = None;
    let mut last_frame = 0u32;
    for l in lines {
        if let Some(t) = l.strip_prefix('#') {
            let t: usize = t.trim().parse().map_err(|_| format!("bad time: {l}"))?;
            frame = t.is_multiple_of(10).then_some((t / 10) as u32);
            continue;
        }
        let Some(f) = frame else { continue };
        let (bits, id) = match l.strip_prefix('b') {
            Some(rest) => match rest.split_once(' ') {
                Some((bits, id)) => (bits, id.trim()),
                None => continue,
            },
            None if l.starts_with(['0', '1', 'x', 'z']) => l.split_at(1),
            None => continue,
        };
        if let Some(&i) = by_id.get(id) {
            let s = &mut signals[i];
            let padded = format!("{bits:0>width$}", width = s.width);
            match s.runs.last_mut() {
                Some((prev_f, prev_v)) if *prev_f == f => *prev_v = padded,
                Some((_, prev_v)) if *prev_v == padded => {}
                _ => s.runs.push((f, padded)),
            }
            last_frame = last_frame.max(f);
        }
    }
    let total_frames = frames_comment.unwrap_or(last_frame + 1).max(1);
    for s in &mut signals {
        if s.runs.first().is_none_or(|(f, _)| *f > 0) {
            s.runs.insert(0, (0, "x".repeat(s.width)));
        }
    }

    let mut buf = Vec::with_capacity(64 + signals.len() * 32);
    buf.extend_from_slice(b"IFTR");
    buf.push(1u8);
    buf.push(u8::from(steps));
    buf.extend_from_slice(&total_frames.to_le_bytes());
    buf.extend_from_slice(&(signals.len() as u32).to_le_bytes());
    for s in &signals {
        let nb = s.name.as_bytes();
        buf.extend_from_slice(&(nb.len() as u16).to_le_bytes());
        buf.extend_from_slice(nb);
        buf.extend_from_slice(&(s.width as u16).to_le_bytes());
        buf.push(u8::from(s.clock));
        buf.extend_from_slice(&(s.runs.len() as u32).to_le_bytes());
        for (start, val) in &s.runs {
            buf.extend_from_slice(&start.to_le_bytes());
            let vb = val.as_bytes();
            buf.extend_from_slice(&(vb.len() as u16).to_le_bytes());
            buf.extend_from_slice(vb);
        }
    }
    Ok(buf)
}

/// Decode an `IFTR` v1 binary buffer back into a [`Trace`].
pub fn decode_iftr(bytes: &[u8], drop_clock: bool) -> Result<Trace, String> {
    if bytes.len() < 14 || &bytes[0..4] != b"IFTR" {
        return Err("invalid IFTR header".into());
    }
    if bytes[4] != 1 {
        return Err(format!("unsupported IFTR version {}", bytes[4]));
    }
    let steps = (bytes[5] & 1) != 0;
    let frames = u32::from_le_bytes(bytes[6..10].try_into().unwrap()) as usize;
    let sig_count = u32::from_le_bytes(bytes[10..14].try_into().unwrap()) as usize;
    let mut off = 14usize;
    let read_u16 = |b: &[u8], o: &mut usize| -> Result<u16, String> {
        if *o + 2 > b.len() {
            return Err("truncated IFTR u16".into());
        }
        let v = u16::from_le_bytes(b[*o..*o + 2].try_into().unwrap());
        *o += 2;
        Ok(v)
    };
    let read_u32 = |b: &[u8], o: &mut usize| -> Result<u32, String> {
        if *o + 4 > b.len() {
            return Err("truncated IFTR u32".into());
        }
        let v = u32::from_le_bytes(b[*o..*o + 4].try_into().unwrap());
        *o += 4;
        Ok(v)
    };

    let mut signals = Vec::with_capacity(sig_count);
    for _ in 0..sig_count {
        let nlen = read_u16(bytes, &mut off)? as usize;
        if off + nlen + 3 > bytes.len() {
            return Err("truncated IFTR signal".into());
        }
        let name = std::str::from_utf8(&bytes[off..off + nlen])
            .map_err(|e| e.to_string())?
            .to_string();
        off += nlen;
        let width = read_u16(bytes, &mut off)? as usize;
        let clock = (bytes[off] & 1) != 0;
        off += 1;
        let run_count = read_u32(bytes, &mut off)? as usize;
        let mut runs: Vec<(usize, String)> = Vec::with_capacity(run_count);
        for _ in 0..run_count {
            let start = read_u32(bytes, &mut off)? as usize;
            let vlen = read_u16(bytes, &mut off)? as usize;
            if off + vlen > bytes.len() {
                return Err("truncated IFTR run".into());
            }
            let val = std::str::from_utf8(&bytes[off..off + vlen])
                .map_err(|e| e.to_string())?
                .to_string();
            off += vlen;
            runs.push((start, val));
        }
        if drop_clock && !steps && clock {
            continue;
        }
        let mut values = vec!["x".repeat(width); frames];
        for (idx, (start, val)) in runs.iter().enumerate() {
            let end = runs
                .get(idx + 1)
                .map_or(frames, |(nf, _)| (*nf).min(frames));
            for slot in values.iter_mut().take(end).skip((*start).min(frames)) {
                slot.clone_from(val);
            }
        }
        signals.push(Signal {
            name,
            width,
            clock,
            values,
        });
    }
    Ok(Trace {
        frames,
        steps,
        signals,
        notes: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const VCD: &str = "\
$comment informal trace of a: 3 cycles $end
$timescale 1ns $end
$scope module top $end
$var wire 1 ! clk $end
$var reg 1 \" en $end
$var reg 4 # q [3:0] $end
$upscope $end
$scope module informal $end
$var reg 1 $ a $end
$upscope $end
$enddefinitions $end
#0
$dumpvars
1!
1\"
b1111 #
0$
$end
#5
0!
#10
1!
b0 #
#15
0!
#20
1!
0\"
b1010 #
1$
#25
0!
#30
";

    #[test]
    fn reads_frames_and_carries_values() {
        let t = parse(VCD).unwrap();
        assert_eq!((t.frames, t.steps), (3, false));
        let names: Vec<&str> = t.signals.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["top.en", "top.q", "informal.a"]);
        assert_eq!(t.signals[0].values, ["1", "1", "0"]);
        assert_eq!(t.signals[1].values, ["1111", "0000", "1010"]);
        assert_eq!(t.signals[2].values, ["0", "0", "1"]);
    }

    #[test]
    fn values_in_hex() {
        assert_eq!(value("1"), "1");
        assert_eq!(value("1010"), "a");
        assert_eq!(value("101111"), "2f");
        assert_eq!(value("1x0000"), "x0");
    }

    #[test]
    fn iftr_binary_roundtrip_matches_parsed_vcd() {
        let expected = parse(VCD).unwrap();
        let bin = encode_iftr(VCD).unwrap();
        assert_eq!(&bin[0..4], b"IFTR");
        let got = decode_iftr(&bin, true).unwrap();
        assert_eq!(got.frames, expected.frames);
        assert_eq!(got.steps, expected.steps);
        assert_eq!(got.signals, expected.signals);
    }

    #[test]
    fn fst_sibling_streams_meta_and_window_iftr() {
        let dir = std::env::temp_dir().join(format!("informal-mcp-fst-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let vcd_path = dir.join("prop.vcd");
        let fst_path = dir.join("prop.fst");
        std::fs::write(&vcd_path, VCD).unwrap();
        let data = informal_fpv::trace::TraceData {
            prop: "a".into(),
            frames: 3,
            half_cycle: false,
            vars: vec![
                informal_fpv::trace::TraceVar {
                    scope: vec!["top".into()],
                    name: "clk".into(),
                    bits: Vec::new(),
                },
                informal_fpv::trace::TraceVar {
                    scope: vec!["top".into()],
                    name: "en".into(),
                    bits: vec![vec![Some(true)], vec![Some(true)], vec![Some(false)]],
                },
                informal_fpv::trace::TraceVar {
                    scope: vec!["top".into()],
                    name: "q".into(),
                    bits: vec![
                        vec![Some(true), Some(true), Some(true), Some(true)],
                        vec![Some(false), Some(false), Some(false), Some(false)],
                        vec![Some(false), Some(true), Some(false), Some(true)],
                    ],
                },
                informal_fpv::trace::TraceVar {
                    scope: vec!["informal".into()],
                    name: "a".into(),
                    bits: vec![vec![Some(false)], vec![Some(false)], vec![Some(true)]],
                },
            ],
            notes: Vec::new(),
        };
        std::fs::write(&fst_path, data.to_fst()).unwrap();

        let meta = read_meta(&vcd_path).unwrap();
        assert_eq!(meta.format, "fst");
        assert_eq!(meta.frames, 3);
        assert_eq!(meta.signals.len(), 4);

        let full_bin = encode_iftr_from_path(&vcd_path).unwrap();
        let got = decode_iftr(&full_bin, true).unwrap();
        assert_eq!(got.signals[0].name, "top.en");
        assert_eq!(got.signals[0].values, ["1", "1", "0"]);
        assert_eq!(got.signals[1].name, "top.q");
        assert_eq!(got.signals[1].values, ["1111", "0000", "1010"]);
        assert_eq!(got.signals[2].name, "informal.a");
        assert_eq!(got.signals[2].values, ["0", "0", "1"]);

        let win_bin =
            encode_window_iftr_from_path(&vcd_path, 1, 2, Some(&["top.q".into()])).unwrap();
        let win = decode_iftr(&win_bin, true).unwrap();
        assert_eq!(win.signals.len(), 1);
        assert_eq!(win.signals[0].name, "top.q");
        assert_eq!(win.signals[0].values[1], "0000");
        assert_eq!(win.signals[0].values[2], "1010");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
