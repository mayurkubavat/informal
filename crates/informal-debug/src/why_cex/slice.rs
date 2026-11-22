use super::format::format_bv;
use super::miter_slice::{is_helper_name, lookup_vcd, msb_str_to_lsb, ParsedVcd};
use super::{CausalEvent, WhyCexReport};
use crate::session::Status;
use informal_ir::{Model, Op};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

pub(super) fn bv_not(a: &[bool]) -> Vec<bool> {
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

pub(super) fn analyze_model_and_vcd(
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
