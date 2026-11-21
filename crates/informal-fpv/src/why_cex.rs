//! Dynamic Counterexample Root-Cause Slicing (`why_cex`).
//!
//! Given a falsified assertion (`cex`) or reached `cover` (`covered`) and its
//! replayed VCD trace + isolated Btor2 cone, evaluates every word-level Btor2
//! node concretely across frames `0..=T` and walks backward from the failing
//! `bad` node at frame `T` following **only active mux arms (`ite`),
//! controlling boolean operands (`and`/`or`/`implies`), and active register
//! transitions**, pruning untaken datapaths and idle hold cycles.

use informal_ir::{Model, Op};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::session::{Session, Status};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CausalEvent {
    pub frame: usize,
    pub signal: String,
    pub kind: &'static str,
    pub width: u32,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_value: Option<String>,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct WhyCexReport {
    pub property: String,
    pub status: String,
    pub failure_frame: usize,
    pub total_frames: usize,
    pub static_cone_signals: usize,
    pub active_slice_signals: usize,
    pub active_signals: Vec<String>,
    pub events: Vec<CausalEvent>,
}

struct ParsedVcd {
    frames: usize,
    /// Map from full VCD signal name -> per-frame LSB-first bits.
    by_name: HashMap<String, Vec<Vec<bool>>>,
    clocks: HashSet<String>,
}

fn parse_vcd_bits(vcd_text: &str) -> Result<ParsedVcd, String> {
    let mut scope: Vec<&str> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut widths: Vec<usize> = Vec::new();
    let mut by_code: HashMap<&str, usize> = HashMap::new();
    let mut clocks: HashSet<String> = HashSet::new();
    let mut declared_frames: Option<usize> = None;

    let mut lines = vcd_text.lines();
    for line in lines.by_ref() {
        let w: Vec<&str> = line.split_whitespace().collect();
        match w.first().copied() {
            Some("$comment") => {
                let body = w[1..w.len().saturating_sub(1)].join(" ");
                if let Some(rest) = body.strip_prefix("informal trace of ") {
                    let count = rest.rsplit(": ").next().unwrap_or("");
                    declared_frames = count.split(' ').next().and_then(|n| n.parse().ok());
                }
            }
            Some("$scope") => scope.push(w.get(2).copied().unwrap_or("")),
            Some("$upscope") => {
                scope.pop();
            }
            Some("$var") if w.len() >= 6 => {
                let width: usize = w[2].parse().map_err(|_| format!("bad $var line: {line}"))?;
                let full = scope
                    .iter()
                    .chain([&w[4]])
                    .copied()
                    .collect::<Vec<_>>()
                    .join(".");
                if names.is_empty() {
                    clocks.insert(full.clone());
                }
                by_code.insert(w[3], names.len());
                names.push(full);
                widths.push(width);
            }
            Some("$enddefinitions") => break,
            _ => {}
        }
    }

    let mut changes: Vec<(usize, usize, String)> = Vec::new();
    let mut cur_frame: Option<usize> = None;
    let mut max_frame = 0usize;
    for line in lines {
        let l = line.trim();
        if l.is_empty() || l.starts_with('$') {
            continue;
        }
        if let Some(ts) = l.strip_prefix('#') {
            if let Ok(t) = ts.parse::<usize>() {
                if t % 10 == 0 {
                    let f = t / 10;
                    cur_frame = Some(f);
                    max_frame = max_frame.max(f);
                } else {
                    cur_frame = None;
                }
            }
            continue;
        }
        let Some(f) = cur_frame else { continue };
        if let Some(rest) = l.strip_prefix('b').or_else(|| l.strip_prefix('B')) {
            if let Some((bits, code)) = rest.split_once(' ') {
                if let Some(&idx) = by_code.get(code.trim()) {
                    changes.push((f, idx, bits.to_string()));
                }
            }
        } else {
            let mut chars = l.chars();
            if let Some(bch) = chars.next() {
                let code = chars.as_str().trim();
                if let Some(&idx) = by_code.get(code) {
                    changes.push((f, idx, bch.to_string()));
                }
            }
        }
    }

    let total_frames = declared_frames.unwrap_or(max_frame + 1).max(max_frame + 1);
    let mut history: Vec<Vec<Vec<bool>>> = widths
        .iter()
        .map(|&w| vec![vec![false; w.max(1)]; total_frames])
        .collect();
    let mut cur: Vec<Vec<bool>> = widths.iter().map(|&w| vec![false; w.max(1)]).collect();
    let mut ptr = 0usize;
    for f in 0..total_frames {
        while ptr < changes.len() && changes[ptr].0 == f {
            let (_, idx, ref msb_str) = changes[ptr];
            cur[idx] = msb_str_to_lsb(msb_str, widths[idx].max(1));
            ptr += 1;
        }
        for (hist_sig, cur_sig) in history.iter_mut().zip(&cur) {
            hist_sig[f] = cur_sig.clone();
        }
    }

    let mut by_name = HashMap::with_capacity(names.len());
    for (name, hist) in names.into_iter().zip(history) {
        by_name.insert(name, hist);
    }
    Ok(ParsedVcd {
        frames: total_frames,
        by_name,
        clocks,
    })
}

fn msb_str_to_lsb(msb: &str, width: usize) -> Vec<bool> {
    let mut out = vec![false; width];
    let bytes = msb.as_bytes();
    for (bit_pos, slot) in out.iter_mut().enumerate() {
        if bit_pos < bytes.len() {
            *slot = bytes[bytes.len() - 1 - bit_pos] == b'1';
        }
    }
    out
}

fn format_bv(bits: &[bool]) -> String {
    if bits.len() <= 1 {
        return if bits.first().copied().unwrap_or(false) {
            "1'b1".into()
        } else {
            "1'b0".into()
        };
    }
    if bits.len() <= 64 {
        let mut v = 0u64;
        for (i, &b) in bits.iter().enumerate() {
            if b {
                v |= 1u64 << i;
            }
        }
        let nibbles = bits.len().div_ceil(4);
        return format!("{}'h{:0width$x}", bits.len(), v, width = nibbles);
    }
    let mut s = format!("{}'b", bits.len());
    for &b in bits.iter().rev() {
        s.push(if b { '1' } else { '0' });
    }
    s
}

fn bv_not(a: &[bool]) -> Vec<bool> {
    a.iter().map(|&b| !b).collect()
}

fn bv_u128(a: &[bool]) -> u128 {
    let mut v = 0u128;
    for (i, &b) in a.iter().take(128).enumerate() {
        if b {
            v |= 1u128 << i;
        }
    }
    v
}

fn bv_i128(a: &[bool]) -> i128 {
    let w = a.len();
    if w == 0 {
        return 0;
    }
    let u = bv_u128(a);
    if w < 128 && a[w - 1] {
        (u | (!0u128 << w)) as i128
    } else {
        u as i128
    }
}

fn u128_to_bv(v: u128, width: usize) -> Vec<bool> {
    (0..width).map(|i| i < 128 && ((v >> i) & 1) != 0).collect()
}

fn bv_add(a: &[bool], b: &[bool]) -> Vec<bool> {
    let w = a.len().max(b.len());
    let mut out = vec![false; w];
    let mut carry = false;
    for (i, slot) in out.iter_mut().enumerate() {
        let ai = a.get(i).copied().unwrap_or(false);
        let bi = b.get(i).copied().unwrap_or(false);
        *slot = ai ^ bi ^ carry;
        carry = (ai && bi) || (ai && carry) || (bi && carry);
    }
    out
}

fn bv_sub(a: &[bool], b: &[bool]) -> Vec<bool> {
    let nb = bv_not(b);
    let mut one = vec![false; b.len().max(1)];
    one[0] = true;
    let twos = bv_add(&nb, &one);
    bv_add(a, &twos)
}

fn parse_const_bits(op: Op, nums: &[String], width: usize) -> Vec<bool> {
    match op {
        Op::Zero => vec![false; width],
        Op::One => {
            let mut v = vec![false; width];
            if width > 0 {
                v[0] = true;
            }
            v
        }
        Op::Ones => vec![true; width],
        Op::Const => msb_str_to_lsb(nums.first().map_or("0", String::as_str), width),
        Op::Constd => {
            let raw = nums.first().map_or("0", String::as_str);
            let val: i128 = raw.parse().unwrap_or(0);
            u128_to_bv(val as u128, width)
        }
        Op::Consth => {
            let raw = nums.first().map_or("0", String::as_str);
            let val = u128::from_str_radix(raw, 16).unwrap_or(0);
            u128_to_bv(val, width)
        }
        _ => vec![false; width],
    }
}

fn eval_op(op: Op, width: usize, args: &[Vec<bool>], nums: &[String]) -> Vec<bool> {
    match op {
        Op::Zero | Op::One | Op::Ones | Op::Const | Op::Constd | Op::Consth => {
            parse_const_bits(op, nums, width)
        }
        Op::Not => bv_not(&args[0]),
        Op::Neg => {
            let inv = bv_not(&args[0]);
            let mut one = vec![false; width.max(1)];
            one[0] = true;
            bv_add(&inv, &one)
        }
        Op::Inc => {
            let mut one = vec![false; width.max(1)];
            one[0] = true;
            bv_add(&args[0], &one)
        }
        Op::Dec => {
            let ones = vec![true; width.max(1)];
            bv_add(&args[0], &ones)
        }
        Op::Redand => vec![args[0].iter().all(|&b| b)],
        Op::Redor => vec![args[0].iter().any(|&b| b)],
        Op::Redxor => vec![args[0].iter().fold(false, |acc, &b| acc ^ b)],
        Op::Uext => {
            let mut out = vec![false; width];
            for (i, &b) in args[0].iter().take(width).enumerate() {
                out[i] = b;
            }
            out
        }
        Op::Sext => {
            let sign = args[0].last().copied().unwrap_or(false);
            let mut out = vec![sign; width];
            for (i, &b) in args[0].iter().take(width).enumerate() {
                out[i] = b;
            }
            out
        }
        Op::Slice => {
            let hi: usize = nums.first().and_then(|s| s.parse().ok()).unwrap_or(0);
            let lo: usize = nums.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            (lo..=hi)
                .map(|i| args[0].get(i).copied().unwrap_or(false))
                .collect()
        }
        Op::Concat => {
            // Btor2 `concat <sort> <msb_arg> <lsb_arg>`
            let mut out = args[1].clone();
            out.extend_from_slice(&args[0]);
            out.truncate(width);
            out
        }
        Op::And => args[0]
            .iter()
            .zip(&args[1])
            .map(|(&a, &b)| a && b)
            .collect(),
        Op::Nand => args[0]
            .iter()
            .zip(&args[1])
            .map(|(&a, &b)| !(a && b))
            .collect(),
        Op::Or => args[0]
            .iter()
            .zip(&args[1])
            .map(|(&a, &b)| a || b)
            .collect(),
        Op::Nor => args[0]
            .iter()
            .zip(&args[1])
            .map(|(&a, &b)| !(a || b))
            .collect(),
        Op::Xor => args[0].iter().zip(&args[1]).map(|(&a, &b)| a ^ b).collect(),
        Op::Xnor | Op::Iff => args[0]
            .iter()
            .zip(&args[1])
            .map(|(&a, &b)| a == b)
            .collect(),
        Op::Implies => args[0]
            .iter()
            .zip(&args[1])
            .map(|(&a, &b)| !a || b)
            .collect(),
        Op::Eq => vec![args[0] == args[1]],
        Op::Neq => vec![args[0] != args[1]],
        Op::Ult => vec![bv_u128(&args[0]) < bv_u128(&args[1])],
        Op::Ulte => vec![bv_u128(&args[0]) <= bv_u128(&args[1])],
        Op::Ugt => vec![bv_u128(&args[0]) > bv_u128(&args[1])],
        Op::Ugte => vec![bv_u128(&args[0]) >= bv_u128(&args[1])],
        Op::Slt => vec![bv_i128(&args[0]) < bv_i128(&args[1])],
        Op::Slte => vec![bv_i128(&args[0]) <= bv_i128(&args[1])],
        Op::Sgt => vec![bv_i128(&args[0]) > bv_i128(&args[1])],
        Op::Sgte => vec![bv_i128(&args[0]) >= bv_i128(&args[1])],
        Op::Add => bv_add(&args[0], &args[1]),
        Op::Sub => bv_sub(&args[0], &args[1]),
        Op::Mul => u128_to_bv(bv_u128(&args[0]).wrapping_mul(bv_u128(&args[1])), width),
        Op::Udiv => u128_to_bv(
            bv_u128(&args[0])
                .checked_div(bv_u128(&args[1]))
                .unwrap_or(u128::MAX),
            width,
        ),
        Op::Urem => u128_to_bv(
            bv_u128(&args[0])
                .checked_rem(bv_u128(&args[1]))
                .unwrap_or_else(|| bv_u128(&args[0])),
            width,
        ),
        Op::Sll => {
            let sh = bv_u128(&args[1]) as usize;
            (0..width)
                .map(|i| i >= sh && args[0].get(i - sh).copied().unwrap_or(false))
                .collect()
        }
        Op::Srl => {
            let sh = bv_u128(&args[1]) as usize;
            (0..width)
                .map(|i| args[0].get(i.saturating_add(sh)).copied().unwrap_or(false))
                .collect()
        }
        Op::Sra => {
            let sh = bv_u128(&args[1]) as usize;
            let sign = args[0].last().copied().unwrap_or(false);
            (0..width)
                .map(|i| args[0].get(i.saturating_add(sh)).copied().unwrap_or(sign))
                .collect()
        }
        Op::Ite => {
            if args[0].first().copied().unwrap_or(false) {
                args[1].clone()
            } else {
                args[2].clone()
            }
        }
        _ => vec![false; width.max(1)],
    }
}

fn is_helper_name(name: &str) -> bool {
    let leaf = name.rsplit('.').next().unwrap_or(name);
    name.starts_with("informal.") || leaf.starts_with("informal_") || leaf.starts_with("_sva_")
}

fn lookup_vcd<'a>(vcd: &'a ParsedVcd, sym: &str, top_prefix: &str) -> Option<&'a Vec<Vec<bool>>> {
    vcd.by_name.get(sym).or_else(|| {
        if !top_prefix.is_empty() && !sym.contains('.') {
            vcd.by_name.get(&format!("{top_prefix}.{sym}"))
        } else {
            None
        }
    })
}

pub fn slice_property_cex(
    s: &Session,
    prop_name: &str,
    max_events: usize,
) -> Result<WhyCexReport, String> {
    let (resolved_name, resolved_status, trace_path, trace_btor, plain_btor) =
        if let Some(prop_idx) = s
            .props
            .iter()
            .position(|p| p.name == prop_name || p.name.ends_with(&format!("::{prop_name}")))
        {
            let p = &s.props[prop_idx];
            let Some(tp) = &p.trace else {
                return Err(format!(
                    "why_cex: '{}' has no counterexample/witness trace (status is {})",
                    p.name,
                    p.status.word()
                ));
            };
            (
                p.name.clone(),
                p.status.clone(),
                tp.clone(),
                s.workdir.join(format!("prop_{prop_idx}.trace.btor2")),
                s.workdir.join(format!("prop_{prop_idx}.btor2")),
            )
        } else if let Some(sec_chk) = s
            .sec
            .checks
            .iter()
            .find(|c| c.name == prop_name || c.imp_sig == prop_name)
        {
            let Some(tp) = &sec_chk.trace else {
                return Err(format!(
                    "why_cex: SEC check '{}' has no counterexample trace (status is {})",
                    sec_chk.name,
                    sec_chk.status.word()
                ));
            };
            let st = match sec_chk.status {
                crate::sec::SecStatus::Cex { depth } => Status::Cex {
                    engine: sec_chk.engine.clone().unwrap_or_default(),
                    depth,
                    ms: sec_chk.ms,
                },
                _ => Status::Undetermined {
                    reason: sec_chk.status.word().to_string(),
                },
            };
            (
                sec_chk.name.clone(),
                st,
                tp.clone(),
                s.workdir.join(format!("{}.trace.btor2", sec_chk.name)),
                s.workdir.join(format!("{}.btor2", sec_chk.name)),
            )
        } else {
            return Err(format!("why_cex: unknown property '{prop_name}'"));
        };
    let vcd_text = std::fs::read_to_string(&trace_path)
        .map_err(|e| format!("why_cex: cannot read '{trace_path}': {e}"))?;
    let vcd = parse_vcd_bits(&vcd_text)?;

    let btor_text = std::fs::read_to_string(&trace_btor)
        .or_else(|_| std::fs::read_to_string(&plain_btor))
        .map_err(|e| format!("why_cex: cannot read Btor2 model for '{resolved_name}': {e}"))?;
    let (core_btor, trace_sec) = informal_svlang::split_trace(&btor_text);
    let m = informal_ir::parse(core_btor).map_err(|e| format!("why_cex: {e}"))?;

    // Also parse `; informal-trace-outputs` alias `output <nid> <name>` lines.
    let mut wire_aliases: HashMap<u32, Vec<String>> = HashMap::new();
    for l in m.with_op(Op::Output) {
        if let (Some(&arg), Some(sym)) = (l.args.first(), &l.symbol) {
            if arg > 0 && !is_helper_name(sym) {
                wire_aliases
                    .entry(arg as u32)
                    .or_default()
                    .push(sym.clone());
            }
        }
    }
    for line in trace_sec.lines() {
        let w: Vec<&str> = line.split_whitespace().collect();
        if w.len() >= 4 && w[1] == "output" {
            if let Ok(arg) = w[2].parse::<i64>() {
                if arg > 0 && !is_helper_name(w[3]) {
                    wire_aliases
                        .entry(arg as u32)
                        .or_default()
                        .push(w[3].to_string());
                }
            }
        }
    }

    let top_prefix = s.design.top.as_deref().unwrap_or_default();
    analyze_model_and_vcd(
        &resolved_name,
        &resolved_status,
        &m,
        &vcd,
        &wire_aliases,
        top_prefix,
        max_events,
    )
}

fn analyze_model_and_vcd(
    prop_name: &str,
    status: &Status,
    m: &Model,
    vcd: &ParsedVcd,
    wire_aliases: &HashMap<u32, Vec<String>>,
    top_prefix: &str,
    max_events: usize,
) -> Result<WhyCexReport, String> {
    let target = m
        .lines()
        .find(|l| matches!(l.op, Op::Bad | Op::Justice))
        .ok_or("why_cex: model has no bad/justice target")?;
    let target_raw = *target.args.first().ok_or("why_cex: malformed target")?;

    let mut init_map: HashMap<u32, i64> = HashMap::new();
    for l in m.with_op(Op::Init) {
        if l.args.len() >= 2 {
            init_map.insert(l.args[0].unsigned_abs() as u32, l.args[1]);
        }
    }
    let mut next_map: HashMap<u32, i64> = HashMap::new();
    for l in m.with_op(Op::Next) {
        if l.args.len() >= 2 {
            next_map.insert(l.args[0].unsigned_abs() as u32, l.args[1]);
        }
    }

    // Static COI user signal count for comparison.
    let static_cone_signals =
        count_static_user_cone(m, target_raw.unsigned_abs() as u32, &next_map);

    // Concrete word-level evaluation across `0..vcd.frames`.
    let n_frames = vcd.frames.max(1);
    let mut frame_vals: Vec<HashMap<u32, Vec<bool>>> = vec![HashMap::new(); n_frames];
    let get_arg = |vals: &HashMap<u32, Vec<bool>>, arg: i64| -> Vec<bool> {
        let nid = arg.unsigned_abs() as u32;
        let base = vals.get(&nid).cloned().unwrap_or_else(|| vec![false; 1]);
        if arg < 0 {
            bv_not(&base)
        } else {
            base
        }
    };

    // Initialize states at frame 0 from VCD when present, else `Init` or 0.
    for f in 0..n_frames {
        for l in m.lines() {
            let w = l.sort.and_then(|sid| m.width(sid)).unwrap_or(1) as usize;
            match l.op {
                Op::Input => {
                    let bv = l
                        .symbol
                        .as_deref()
                        .and_then(|sym| lookup_vcd(vcd, sym, top_prefix))
                        .and_then(|hist| hist.get(f))
                        .cloned()
                        .unwrap_or_else(|| vec![false; w.max(1)]);
                    frame_vals[f].insert(l.id, bv);
                }
                Op::State => {
                    if f == 0 {
                        let bv = l
                            .symbol
                            .as_deref()
                            .and_then(|sym| lookup_vcd(vcd, sym, top_prefix))
                            .and_then(|hist| hist.first())
                            .cloned()
                            .or_else(|| init_map.get(&l.id).map(|&ia| get_arg(&frame_vals[0], ia)))
                            .unwrap_or_else(|| vec![false; w.max(1)]);
                        frame_vals[0].insert(l.id, bv);
                    } else if !frame_vals[f].contains_key(&l.id) {
                        let from_vcd = l
                            .symbol
                            .as_deref()
                            .and_then(|sym| lookup_vcd(vcd, sym, top_prefix))
                            .and_then(|hist| hist.get(f))
                            .cloned();
                        if let Some(bv) = from_vcd {
                            frame_vals[f].insert(l.id, bv);
                        }
                    }
                }
                Op::Init
                | Op::Next
                | Op::Bad
                | Op::Constraint
                | Op::Fair
                | Op::Justice
                | Op::Output => {}
                other => {
                    let arg_bvs: Vec<Vec<bool>> =
                        l.args.iter().map(|&a| get_arg(&frame_vals[f], a)).collect();
                    let res = eval_op(other, w.max(1), &arg_bvs, &l.nums);
                    frame_vals[f].insert(l.id, res);
                }
            }
        }
        // Step unnamed / helper states to `f + 1` via `Next`.
        if f + 1 < n_frames {
            for (&sid, &narg) in &next_map {
                let has_vcd = m
                    .line(sid)
                    .and_then(|l| l.symbol.as_deref())
                    .and_then(|sym| lookup_vcd(vcd, sym, top_prefix))
                    .is_some();
                if !has_vcd {
                    let nxt = get_arg(&frame_vals[f], narg);
                    frame_vals[f + 1].insert(sid, nxt);
                }
            }
        }
    }

    let reported_depth = match status {
        Status::Cex { depth, .. } | Status::Covered { depth, .. } => depth.map(|d| d as usize),
        _ => None,
    };
    let fail_frame = (0..n_frames)
        .find(|&f| {
            get_arg(&frame_vals[f], target_raw)
                .first()
                .copied()
                .unwrap_or(false)
        })
        .or_else(|| reported_depth.map(|d| d.min(n_frames - 1)))
        .unwrap_or(n_frames - 1);

    // Dynamic backward slice from `(fail_frame, abs(target_raw))`.
    let mut visited: HashSet<(usize, u32)> = HashSet::new();
    let mut queue: VecDeque<(usize, u32)> = VecDeque::new();
    let root_nid = target_raw.unsigned_abs() as u32;
    visited.insert((fail_frame, root_nid));
    queue.push_back((fail_frame, root_nid));

    let mut active_signals_set: BTreeMap<String, ()> = BTreeMap::new();
    let mut state_frames: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    let mut input_frames: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    let mut wire_hits: Vec<(usize, String, u32, Vec<bool>)> = Vec::new();

    while let Some((f, nid)) = queue.pop_front() {
        let Some(line) = m.line(nid) else { continue };
        if f == fail_frame {
            if let Some(aliases) = wire_aliases.get(&nid) {
                let w = line.sort.and_then(|s| m.width(s)).unwrap_or(1);
                let bv = frame_vals[f].get(&nid).cloned().unwrap_or_default();
                for alias in aliases {
                    active_signals_set.insert(alias.clone(), ());
                    wire_hits.push((f, alias.clone(), w, bv.clone()));
                }
            }
        }
        let mut push = |tf: usize, raw_arg: i64| {
            let cid = raw_arg.unsigned_abs() as u32;
            if visited.insert((tf, cid)) {
                queue.push_back((tf, cid));
            }
        };

        match line.op {
            Op::Input => {
                if let Some(sym) = &line.symbol {
                    let full_chk = if !top_prefix.is_empty() && !sym.contains('.') {
                        format!("{top_prefix}.{sym}")
                    } else {
                        sym.clone()
                    };
                    if !is_helper_name(sym)
                        && !vcd.clocks.contains(sym)
                        && !vcd.clocks.contains(&full_chk)
                    {
                        active_signals_set.insert(sym.clone(), ());
                        input_frames.entry(nid).or_default().push(f);
                    }
                }
            }
            Op::State => {
                if let Some(sym) = &line.symbol {
                    if !is_helper_name(sym) {
                        active_signals_set.insert(sym.clone(), ());
                        state_frames.entry(nid).or_default().push(f);
                    }
                }
                if f > 0 {
                    if let Some(&narg) = next_map.get(&nid) {
                        push(f - 1, narg);
                    }
                } else if let Some(&iarg) = init_map.get(&nid) {
                    push(0, iarg);
                }
            }
            Op::Ite if line.args.len() == 3 => {
                let cond_true = get_arg(&frame_vals[f], line.args[0])
                    .first()
                    .copied()
                    .unwrap_or(false);
                push(f, line.args[0]);
                if cond_true {
                    push(f, line.args[1]);
                } else {
                    push(f, line.args[2]);
                }
            }
            Op::And if line.args.len() == 2 => {
                let out_v = frame_vals[f].get(&nid).cloned().unwrap_or_default();
                if out_v.len() == 1 && !out_v[0] {
                    let a0 = get_arg(&frame_vals[f], line.args[0])[0];
                    let a1 = get_arg(&frame_vals[f], line.args[1])[0];
                    if !a0 && a1 {
                        push(f, line.args[0]);
                    } else if !a1 && a0 {
                        push(f, line.args[1]);
                    } else {
                        push(f, line.args[0]);
                        push(f, line.args[1]);
                    }
                } else {
                    push(f, line.args[0]);
                    push(f, line.args[1]);
                }
            }
            Op::Or if line.args.len() == 2 => {
                let out_v = frame_vals[f].get(&nid).cloned().unwrap_or_default();
                if out_v.len() == 1 && out_v[0] {
                    let a0 = get_arg(&frame_vals[f], line.args[0])[0];
                    let a1 = get_arg(&frame_vals[f], line.args[1])[0];
                    if a0 && !a1 {
                        push(f, line.args[0]);
                    } else if a1 && !a0 {
                        push(f, line.args[1]);
                    } else {
                        push(f, line.args[0]);
                        push(f, line.args[1]);
                    }
                } else {
                    push(f, line.args[0]);
                    push(f, line.args[1]);
                }
            }
            Op::Implies if line.args.len() == 2 => {
                let out_v = frame_vals[f].get(&nid).cloned().unwrap_or_default();
                if out_v.len() == 1 && out_v[0] {
                    let a0 = get_arg(&frame_vals[f], line.args[0])[0];
                    let a1 = get_arg(&frame_vals[f], line.args[1])[0];
                    if !a0 && !a1 {
                        push(f, line.args[0]);
                    } else if a0 && a1 {
                        push(f, line.args[1]);
                    } else {
                        push(f, line.args[0]);
                        push(f, line.args[1]);
                    }
                } else {
                    push(f, line.args[0]);
                    push(f, line.args[1]);
                }
            }
            _ => {
                for &arg in &line.args {
                    push(f, arg);
                }
            }
        }
    }

    let mut events: Vec<CausalEvent> = Vec::new();
    for (sid, mut frames) in state_frames {
        frames.sort_unstable();
        frames.dedup();
        let Some(line) = m.line(sid) else { continue };
        let Some(sym) = &line.symbol else { continue };
        let w = line.sort.and_then(|s| m.width(s)).unwrap_or(1);
        for &f in &frames {
            let cur_bv = frame_vals[f].get(&sid).cloned().unwrap_or_default();
            if f == 0 {
                events.push(CausalEvent {
                    frame: 0,
                    signal: sym.clone(),
                    kind: "register",
                    width: w,
                    value: format_bv(&cur_bv),
                    prev_value: None,
                    reason: "initial_state",
                });
            } else {
                let prev_bv = frame_vals[f - 1].get(&sid).cloned().unwrap_or_default();
                if cur_bv != prev_bv {
                    events.push(CausalEvent {
                        frame: f,
                        signal: sym.clone(),
                        kind: "register",
                        width: w,
                        value: format_bv(&cur_bv),
                        prev_value: Some(format_bv(&prev_bv)),
                        reason: "register_transition",
                    });
                } else if f == fail_frame {
                    events.push(CausalEvent {
                        frame: f,
                        signal: sym.clone(),
                        kind: "register",
                        width: w,
                        value: format_bv(&cur_bv),
                        prev_value: None,
                        reason: "failure_operand",
                    });
                }
            }
        }
    }

    for (iid, mut frames) in input_frames {
        frames.sort_unstable();
        frames.dedup();
        let Some(line) = m.line(iid) else { continue };
        let Some(sym) = &line.symbol else { continue };
        let w = line.sort.and_then(|s| m.width(s)).unwrap_or(1);
        let mut last_bv: Option<Vec<bool>> = None;
        for &f in &frames {
            let cur_bv = frame_vals[f].get(&iid).cloned().unwrap_or_default();
            if last_bv.as_ref() != Some(&cur_bv) || f == fail_frame {
                events.push(CausalEvent {
                    frame: f,
                    signal: sym.clone(),
                    kind: "input",
                    width: w,
                    value: format_bv(&cur_bv),
                    prev_value: None,
                    reason: if f == fail_frame {
                        "failure_trigger"
                    } else {
                        "active_input"
                    },
                });
                last_bv = Some(cur_bv);
            }
        }
    }

    let mut seen_wires = HashSet::new();
    for (f, alias, w, bv) in wire_hits {
        if seen_wires.insert((f, alias.clone())) {
            events.push(CausalEvent {
                frame: f,
                signal: alias,
                kind: "wire",
                width: w,
                value: format_bv(&bv),
                prev_value: None,
                reason: "failure_operand",
            });
        }
    }

    events.sort_by(|a, b| (a.frame, &a.signal, a.kind).cmp(&(b.frame, &b.signal, b.kind)));
    if max_events > 0 && events.len() > max_events {
        // Keep the tail leading directly into `fail_frame`.
        let skip = events.len() - max_events;
        events.drain(0..skip);
    }

    let active_signals: Vec<String> = active_signals_set.into_keys().collect();
    Ok(WhyCexReport {
        property: prop_name.to_string(),
        status: status.word().to_string(),
        failure_frame: fail_frame,
        total_frames: n_frames,
        static_cone_signals,
        active_slice_signals: active_signals.len(),
        active_signals,
        events,
    })
}

fn count_static_user_cone(m: &Model, target_nid: u32, next_map: &HashMap<u32, i64>) -> usize {
    let mut seen_nodes = HashSet::new();
    let mut stack = vec![target_nid];
    let mut sigs = HashSet::new();
    while let Some(nid) = stack.pop() {
        if !seen_nodes.insert(nid) {
            continue;
        }
        let Some(l) = m.line(nid) else { continue };
        match l.op {
            Op::Input | Op::State => {
                if let Some(sym) = &l.symbol {
                    if !is_helper_name(sym) {
                        sigs.insert(sym.clone());
                    }
                }
                if l.op == Op::State {
                    if let Some(&narg) = next_map.get(&nid) {
                        stack.push(narg.unsigned_abs() as u32);
                    }
                }
            }
            _ => {
                stack.extend(l.args.iter().map(|a| a.unsigned_abs() as u32));
            }
        }
    }
    sigs.len()
}

pub fn format_text(r: &WhyCexReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "why_cex '{}' ({}, failure at frame {}/{}, active slice {}/{} static COI signals):",
        r.property,
        r.status,
        r.failure_frame,
        r.total_frames.saturating_sub(1),
        r.active_slice_signals,
        r.static_cone_signals,
    );
    for ev in &r.events {
        match &ev.prev_value {
            Some(prev) => {
                let _ = writeln!(
                    out,
                    "  @{:<3} {:<8} {:<28} {} -> {} ({})",
                    ev.frame, ev.kind, ev.signal, prev, ev.value, ev.reason
                );
            }
            None => {
                let _ = writeln!(
                    out,
                    "  @{:<3} {:<8} {:<28} = {} ({})",
                    ev.frame, ev.kind, ev.signal, ev.value, ev.reason
                );
            }
        }
    }
    out
}
