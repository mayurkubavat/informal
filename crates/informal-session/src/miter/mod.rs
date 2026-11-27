//! Two-safety self-composition miter builder over `informal_ir::Model`.
//!
//! Powers both `check_spv` (formal non-interference / security path verification)
//! and `check_xprop` (exact 2-copy X-propagation verification without ternary
//! X-pessimism or X-optimism). All auxiliary latches use constant `0`/`1`
//! initializers so IC3/PDR, `Ic3Mt`, `KindIc3`, k-induction, and BMC all run
//! natively at full speed.

use informal_ir::{Line, Model, Op};
use std::collections::{HashMap, HashSet};

mod builder;
mod cond_corrupt;
mod replay;

pub use builder::build_miter_btor2;
pub use replay::{replay_miter_cex, sanitize_tag};

#[derive(Debug, Clone)]
pub struct ResolvedSig {
    /// Signed node reference (`+id` or `-id`).
    pub node_ref: i64,
    /// Bit width of the signal (`>= 1`).
    pub width: u32,
    /// Underlying positive line ID when directly naming an `Input` or `State`.
    pub direct_line: Option<(u32, Op)>,
    /// Canonical signal name.
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct CondDivergence {
    pub node_id: u32,
    /// 1-bit signed node ref in the original model; when true in copy k,
    /// copy k sees an independent fresh input instead of the functional node.
    pub corrupt_cond: i64,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct MiterSpec {
    /// Primary/cut/X `Op::Input` line IDs allowed to differ between Copy 0 and Copy 1.
    pub diverging_inputs: HashSet<u32>,
    /// Internal/register node IDs replaced in Copy 1 by a fresh unconstrained `input`
    /// of identical width (used when `check_spv -from` names an internal wire or register).
    pub diverging_nodes: HashMap<u32, String>,
    /// Nodes that become independent fresh inputs in copy k whenever `corrupt_cond` is 1 in copy k.
    pub conditional_diverging_nodes: Vec<CondDivergence>,
    /// `Op::State` nodes corrupted both combinationally (`q_obs`) and sequentially (`next`)
    /// whenever `corrupt_cond` is 1 in copy k, preserving post-wakeup divergence until overwritten/reset.
    pub conditional_state_corruptions: Vec<CondDivergence>,
    /// When `false` (`check_spv`), uninitialized registers without `init` are constrained
    /// equal at step 0 (`r0(0) == r1(0)`) so power-on unknowns do not masquerade as secret
    /// taint; when `true` (`check_xprop`), uninitialized registers start independently in
    /// Copy 0 and Copy 1 (`r0(0)` vs `r1(0)`), except for `tied_uninit_states`.
    pub uninit_states_diverge: bool,
    /// Specific uninitialized `Op::State` IDs forced equal at step 0 even when
    /// `uninit_states_diverge` is `true` (used by `check_xprop` waivers on source flops).
    pub tied_uninit_states: HashSet<u32>,
    /// Model steps from 0 to skip before asserting `obs_0 == obs_1` (e.g. reset window).
    pub skip_steps: u32,
    /// Optional 1-bit enabling guard (signed node ref in Copy 0, required true in both copies).
    pub guard_ref: Option<i64>,
    /// Target observation `(node_ref, width, target_symbol)` checked for `obs_0 != obs_1`.
    pub target: (i64, u32, String),
    /// Append `; informal-trace-outputs` with `m0.*` / `m1.*` outputs for VCD replay and `why_cex`.
    pub emit_trace_outputs: bool,
}

/// Parses `full_btor` (including `; informal-trace-outputs` `output` lines) into `Model`.
pub fn parse_full_model(full_btor: &str) -> Result<Model, String> {
    informal_ir::parse(full_btor).map_err(|e| format!("miter parse: {e}"))
}

/// Resolves a user signal name (`u_sub.sig` or `top.u_sub.sig`) in `m`.
pub fn resolve_signal(m: &Model, top: Option<&str>, raw_name: &str) -> Result<ResolvedSig, String> {
    let trimmed = raw_name.trim();
    let mut candidates = vec![trimmed.to_string()];
    if let Some(t) = top.filter(|t| !t.is_empty()) {
        if let Some(rest) = trimmed.strip_prefix(t).and_then(|r| r.strip_prefix('.')) {
            candidates.push(rest.to_string());
        } else {
            candidates.push(format!("{t}.{trimmed}"));
        }
    }

    let width_of_line = |line: &Line| -> Option<u32> { line.sort.and_then(|sid| m.width(sid)) };

    for cand in &candidates {
        for l in m.lines() {
            if matches!(l.op, Op::Input | Op::State) && l.symbol.as_deref() == Some(cand.as_str()) {
                if let Some(w) = width_of_line(l) {
                    return Ok(ResolvedSig {
                        node_ref: l.id as i64,
                        width: w,
                        direct_line: Some((l.id, l.op)),
                        name: cand.clone(),
                    });
                }
            }
        }
        for l in m.lines() {
            if l.op == Op::Output && l.symbol.as_deref() == Some(cand.as_str()) {
                if let Some(&arg) = l.args.first() {
                    let base = m
                        .line(arg.unsigned_abs() as u32)
                        .ok_or_else(|| format!("dangling output node on `{cand}`"))?;
                    if let Some(w) = width_of_line(base) {
                        let direct = (matches!(base.op, Op::Input | Op::State) && arg > 0)
                            .then_some((base.id, base.op));
                        return Ok(ResolvedSig {
                            node_ref: arg,
                            width: w,
                            direct_line: direct,
                            name: cand.clone(),
                        });
                    }
                }
            }
        }
    }
    Err(format!("signal `{trimmed}` not found in lowered design"))
}

/// Lists default X-prop observation sinks in `m` according to `categories`
/// (`outputs`, `control`, `memories`, `assertions`, `all`).
pub fn collect_xprop_sinks(
    m: &Model,
    top: Option<&str>,
    top_output_ports: &[String],
    categories: &[String],
) -> Vec<(i64, u32, String, &'static str)> {
    let all = categories.is_empty() || categories.iter().any(|c| c == "all");
    let want_out = all || categories.iter().any(|c| c == "outputs" || c == "output");
    let want_ctrl = all || categories.iter().any(|c| c == "control" || c == "mux");
    let want_mem = all || categories.iter().any(|c| c == "memories" || c == "memory");
    let want_assert = categories
        .iter()
        .any(|c| c == "assertions" || c == "assert");

    let top_prefix = top.map(|t| format!("{t}.")).unwrap_or_default();
    let mut sinks: Vec<(i64, u32, String, &'static str)> = Vec::new();
    let mut seen_refs: HashSet<(i64, &'static str)> = HashSet::new();
    let mut seen_out_names: HashSet<String> = HashSet::new();

    if want_out {
        if !top_output_ports.is_empty() {
            for port in top_output_ports {
                if let (Ok(r_oe), Ok(r_do)) = (
                    resolve_signal(m, top, &format!("{port}$oe")),
                    resolve_signal(m, top, &format!("{port}$out")),
                ) {
                    for r in [r_oe, r_do] {
                        if seen_out_names.insert(r.name.clone()) {
                            sinks.push((r.node_ref, r.width, r.name, "output"));
                        }
                    }
                    continue;
                }
                if let Ok(r) = resolve_signal(m, top, port) {
                    if seen_out_names.insert(r.name.clone()) {
                        sinks.push((r.node_ref, r.width, r.name, "output"));
                    }
                }
            }
        } else {
            // Fallback when scope hierarchy is unavailable: top-level `output` lines in trace section.
            let all_out_syms: HashSet<&str> = m
                .lines()
                .filter(|l| l.op == Op::Output)
                .filter_map(|l| l.symbol.as_deref())
                .collect();
            for l in m.lines().filter(|l| l.op == Op::Output) {
                let Some(sym) = l.symbol.as_deref() else {
                    continue;
                };
                if all_out_syms.contains(format!("{sym}$oe").as_str()) {
                    continue;
                }
                let is_top_sig = match sym.strip_prefix(&top_prefix) {
                    Some(rest) => !rest.contains('.'),
                    None => !sym.contains('.'),
                };
                if !is_top_sig {
                    continue;
                };
                let Some(&arg) = l.args.first() else { continue };
                let Some(base) = m.line(arg.unsigned_abs() as u32) else {
                    continue;
                };
                let Some(w) = base.sort.and_then(|s| m.width(s)) else {
                    continue;
                };
                if seen_out_names.insert(sym.to_string()) {
                    sinks.push((arg, w, sym.to_string(), "output"));
                }
            }
        }
    }

    if want_ctrl {
        // Every non-trivial 1-bit `Ite` condition on bit-vectors is a mux/branch/clock-enable control!
        let mut ctrl_idx = 0usize;
        for l in m.lines().filter(|l| l.op == Op::Ite) {
            if l.sort.and_then(|s| m.width(s)).is_none() {
                continue;
            }
            let Some(&cond) = l.args.first() else {
                continue;
            };
            let Some(base) = m.line(cond.unsigned_abs() as u32) else {
                continue;
            };
            if matches!(
                base.op,
                Op::Const | Op::Constd | Op::Consth | Op::Zero | Op::One | Op::Ones
            ) {
                continue;
            }
            if base
                .symbol
                .as_deref()
                .is_some_and(|s| s.starts_with("informal_"))
            {
                continue;
            }
            if seen_refs.insert((cond.abs(), "control")) {
                let name = base
                    .symbol
                    .clone()
                    .unwrap_or_else(|| format!("{top_prefix}_xprop_ctrl_{ctrl_idx}"));
                ctrl_idx += 1;
                sinks.push((cond.abs(), 1, name, "control"));
            }
        }
    }

    if want_mem {
        let mut mem_idx = 0usize;
        for l in m.lines().filter(|l| l.op == Op::Write) {
            if l.args.len() >= 2 {
                let addr = l.args[1];
                if let Some(base) = m.line(addr.unsigned_abs() as u32) {
                    if let Some(w) = base.sort.and_then(|s| m.width(s)) {
                        if seen_refs.insert((addr, "memory")) {
                            sinks.push((
                                addr,
                                w,
                                format!("{top_prefix}_xprop_mem_addr_{mem_idx}"),
                                "memory",
                            ));
                            mem_idx += 1;
                        }
                    }
                }
            }
        }
    }

    if want_assert {
        for l in m.lines().filter(|l| l.op == Op::Bad) {
            let Some(sym) = l.symbol.as_deref() else {
                continue;
            };
            if sym.contains(':') {
                continue;
            }
            if let Some(&arg) = l.args.first() {
                if seen_refs.insert((arg, "assert")) {
                    sinks.push((arg, 1, sym.to_string(), "assert"));
                }
            }
        }
    }

    sinks
}

pub(super) fn sanitize_btor_sym(sym: &str) -> String {
    let s: String = sym
        .chars()
        .map(|c| {
            if c.is_whitespace() || c == ';' {
                '_'
            } else {
                c
            }
        })
        .collect();
    if s.is_empty() {
        "sig".to_string()
    } else {
        s
    }
}

pub(super) fn is_internal_helper(sym: &str) -> bool {
    let leaf = sym.rsplit('.').next().unwrap_or(sym);
    sym.starts_with("informal.")
        || leaf.starts_with("informal_")
        || leaf.starts_with("_sva_")
        || leaf.starts_with("miter_")
}

pub(super) fn is_valued_op(op: Op) -> bool {
    !matches!(
        op,
        Op::Init | Op::Next | Op::Bad | Op::Constraint | Op::Fair | Op::Justice | Op::Output
    )
}
