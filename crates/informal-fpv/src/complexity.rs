//! Formal COI Complexity Analysis, Blackbox Assistant, Setup Lint, & Counter Abstraction
//! (`get_fv_complexity`, `blackbox_assistant`, `check_fv_setup`, `report_fv_setup`,
//! `get_reset_info`, and `abstract -counter` / `report_abstractions` / `set_abstractions`).

use crate::miter::parse_full_model;
use crate::session::{Kind, Session, Status};
use informal_ir::{Entry, Model, Op, Sort};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CounterAbstraction {
    /// Signal path relative to `top` (e.g. `cnt` or `u_tmr.cnt`).
    pub signal: String,
    /// Optional threshold `K`: when set, the counter may jump forward to `K - 1`
    /// whenever `cnt < K - 1`; when `None`, the counter may jump to any width-valid value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstanceComplexity {
    pub instance: String,
    pub module: String,
    pub flops: usize,
    pub flop_bits: u64,
    pub memories: usize,
    pub memory_bits: u64,
    pub wide_arith: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct PropertyComplexity {
    pub property: String,
    pub flops: usize,
    pub flop_bits: u64,
    pub inputs: usize,
    pub input_bits: u64,
    pub memories: usize,
    pub memory_bits: u64,
    pub wide_arith: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComplexityReport {
    pub top: String,
    pub total_flops: usize,
    pub total_flop_bits: u64,
    pub total_inputs: usize,
    pub total_input_bits: u64,
    pub total_memories: usize,
    pub total_memory_bits: u64,
    pub wide_arith_ops: usize,
    pub max_wide_arith_width: u32,
    pub max_memory_words: u64,
    pub instances: Vec<InstanceComplexity>,
    pub properties: Vec<PropertyComplexity>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BboxSuggestion {
    pub kind: &'static str,
    pub target: String,
    pub command: String,
    pub reason: String,
    pub impact_bits: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BboxAdvice {
    pub threshold_bits: u64,
    pub applied: bool,
    pub suggestions: Vec<BboxSuggestion>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetupFinding {
    pub severity: &'static str,
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResetSummary {
    pub configured_reset: Option<String>,
    pub rtl_resets: Vec<informal_svlang::ResetInfo>,
    pub resetless_flops: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetupReport {
    pub elaborated: bool,
    pub top: Option<String>,
    pub configured_clocks: Vec<String>,
    pub rtl_clocks: Vec<informal_svlang::ClockInfo>,
    pub reset: ResetSummary,
    pub undriven_signals: usize,
    pub black_boxes: Vec<String>,
    pub cut_points: Vec<String>,
    pub counter_abstractions: Vec<CounterAbstraction>,
    pub helper_assumptions: Vec<String>,
    pub unsupported_properties: Vec<String>,
    pub findings: Vec<SetupFinding>,
}

fn is_helper_sym(sym: &str) -> bool {
    let leaf = sym.rsplit('.').next().unwrap_or(sym);
    sym.starts_with("informal_")
        || leaf.starts_with("informal_")
        || leaf.starts_with("_sva_")
        || leaf.starts_with("_past_")
        || leaf.starts_with("_was_")
        || leaf.starts_with("_clk_")
}

fn is_wide_arith(op: Op) -> bool {
    matches!(
        op,
        Op::Mul | Op::Udiv | Op::Sdiv | Op::Urem | Op::Srem | Op::Smod
    )
}

fn array_shape(m: &Model, sort_id: u32) -> Option<(u64, u32)> {
    match m.sort(sort_id)? {
        Sort::Array { index, element } => {
            let aw = m.width(index)?;
            let dw = m.width(element)?;
            let words = 1u64.checked_shl(aw).unwrap_or(u64::MAX);
            Some((words, dw))
        }
        Sort::BitVec(_) => None,
    }
}

fn collect_coi_nodes(m: &Model, roots: &[u32], next_map: &HashMap<u32, Vec<u32>>) -> HashSet<u32> {
    let mut seen: HashSet<u32> = HashSet::new();
    let mut stack: Vec<u32> = roots.to_vec();
    // Also include active constraints so COI reflects environment fan-in.
    for c in m.with_op(Op::Constraint) {
        if let Some(&arg) = c.args.first() {
            stack.push(arg.unsigned_abs() as u32);
        }
    }
    while let Some(nid) = stack.pop() {
        if !seen.insert(nid) {
            continue;
        }
        let Some(l) = m.line(nid) else { continue };
        for &a in &l.args {
            stack.push(a.unsigned_abs() as u32);
        }
        if l.op == Op::State {
            if let Some(nxts) = next_map.get(&nid) {
                stack.extend(nxts.iter().copied());
            }
        }
    }
    seen
}

fn owning_instance<'a>(sym: &'a str, inst_paths: &[&'a str], top: &'a str) -> &'a str {
    let mut best = top;
    for &ip in inst_paths {
        if sym.strip_prefix(ip).is_some_and(|r| r.starts_with('.')) && ip.len() > best.len() {
            best = ip;
        }
    }
    best
}

/// Computes COI complexity metrics across the design (or for a single `prop_filter`).
pub fn fv_complexity(
    s: &mut Session,
    prop_filter: Option<&str>,
) -> Result<ComplexityReport, String> {
    if !s.elaborated {
        s.elaborate(None)
            .map_err(|e| format!("get_fv_complexity: {e:#}"))?;
    }
    let top = s.design.top.clone().unwrap_or_default();
    let task = s.current_task.clone();
    let (full_btor, _) = s.lower_for_coverage(&task, false, false)?;
    let m = parse_full_model(&full_btor)?;

    let mut next_map: HashMap<u32, Vec<u32>> = HashMap::new();
    for l in m.lines().filter(|l| matches!(l.op, Op::Next | Op::Init)) {
        if l.args.len() >= 2 {
            next_map
                .entry(l.args[0].unsigned_abs() as u32)
                .or_default()
                .push(l.args[1].unsigned_abs() as u32);
        }
    }

    // Collect property target roots (`bad` + `justice`, skipping `:precondition` / `:witness`).
    let mut prop_targets: Vec<(String, u32)> = Vec::new();
    for l in m.lines().filter(|l| matches!(l.op, Op::Bad | Op::Justice)) {
        let Some(sym) = l.symbol.as_deref() else {
            continue;
        };
        if sym.contains(':') {
            continue;
        }
        if let Some(pat) = prop_filter {
            if !crate::session::glob(pat, sym) && !sym.ends_with(&format!(".{pat}")) && sym != pat {
                continue;
            }
        }
        if let Some(&arg) = l.args.first() {
            prop_targets.push((sym.to_string(), arg.unsigned_abs() as u32));
        }
    }
    if prop_filter.is_some() && prop_targets.is_empty() {
        return Err(format!(
            "get_fv_complexity: no compiled property matching `{}`",
            prop_filter.unwrap_or_default()
        ));
    }

    let inst_map: BTreeMap<String, String> = if s.hierarchy.is_empty() {
        [(top.clone(), top.clone())].into_iter().collect()
    } else {
        s.hierarchy
            .iter()
            .map(|i| (i.path.clone(), i.module.clone()))
            .collect()
    };
    let inst_paths: Vec<&str> = inst_map.keys().map(String::as_str).collect();

    // Map node -> nearest symbolic output/state owner for attributing anonymous wide-arith ops.
    let mut node_inst: HashMap<u32, &str> = HashMap::new();
    for l in m.lines() {
        if matches!(l.op, Op::State | Op::Input) {
            if let Some(sym) = l.symbol.as_deref() {
                if !is_helper_sym(sym) {
                    let owner = owning_instance(sym, &inst_paths, &top);
                    node_inst.insert(l.id, owner);
                }
            }
        } else if l.op == Op::Output {
            if let (Some(&arg), Some(sym)) = (l.args.first(), l.symbol.as_deref()) {
                if !is_helper_sym(sym) {
                    let owner = owning_instance(sym, &inst_paths, &top);
                    node_inst.entry(arg.unsigned_abs() as u32).or_insert(owner);
                }
            }
        }
    }
    // Also attribute anonymous `Next` RHS cones (like `x * y` feeding `u_mul.prod_q`) to the register's instance.
    for l in m.with_op(Op::Next) {
        if l.args.len() >= 2 {
            let st_id = l.args[0].unsigned_abs() as u32;
            let nxt_id = l.args[1].unsigned_abs() as u32;
            if let Some(&own) = node_inst.get(&st_id) {
                if own != top {
                    node_inst.entry(nxt_id).or_insert(own);
                }
            }
        }
    }
    // Propagate instance ownership across unnamed internal nodes.
    for entry in m.entries().iter().rev() {
        let Entry::Line(l) = entry else { continue };
        if let Some(&own) = node_inst.get(&l.id) {
            if own != top {
                for &a in &l.args {
                    node_inst.entry(a.unsigned_abs() as u32).or_insert(own);
                }
            }
        }
    }
    for l in m.lines() {
        if !node_inst.contains_key(&l.id) {
            for &a in &l.args {
                if let Some(&own) = node_inst.get(&(a.unsigned_abs() as u32)) {
                    if own != top {
                        node_inst.insert(l.id, own);
                        break;
                    }
                }
            }
        }
    }

    let summarize_cone =
        |cone: &HashSet<u32>| -> (usize, u64, usize, u64, usize, u64, usize, u32, u64) {
            let mut flops = 0usize;
            let mut flop_bits = 0u64;
            let mut inputs = 0usize;
            let mut input_bits = 0u64;
            let mut memories = 0usize;
            let mut memory_bits = 0u64;
            let mut wide_arith = 0usize;
            let mut max_wide_w = 0u32;
            let mut max_mem_words = 0u64;

            for &nid in cone {
                let Some(l) = m.line(nid) else { continue };
                match l.op {
                    Op::State => {
                        let sym = l.symbol.as_deref().unwrap_or("");
                        if is_helper_sym(sym) {
                            continue;
                        }
                        if let Some(sid) = l.sort {
                            if let Some(w) = m.width(sid) {
                                flops += 1;
                                flop_bits += u64::from(w);
                            } else if let Some((words, dw)) = array_shape(&m, sid) {
                                memories += 1;
                                memory_bits =
                                    memory_bits.saturating_add(words.saturating_mul(u64::from(dw)));
                                max_mem_words = max_mem_words.max(words);
                            }
                        }
                    }
                    Op::Input => {
                        let sym = l.symbol.as_deref().unwrap_or("");
                        if is_helper_sym(sym) {
                            continue;
                        }
                        if let Some(w) = l.sort.and_then(|sid| m.width(sid)) {
                            inputs += 1;
                            input_bits += u64::from(w);
                        }
                    }
                    op if is_wide_arith(op) => {
                        if let Some(w) = l.sort.and_then(|sid| m.width(sid)) {
                            if w >= 16 {
                                wide_arith += 1;
                                max_wide_w = max_wide_w.max(w);
                            }
                        }
                    }
                    _ => {}
                }
            }
            (
                flops,
                flop_bits,
                inputs,
                input_bits,
                memories,
                memory_bits,
                wide_arith,
                max_wide_w,
                max_mem_words,
            )
        };

    let mut prop_reports = Vec::with_capacity(prop_targets.len());
    for (pname, root) in &prop_targets {
        let cone = collect_coi_nodes(&m, &[*root], &next_map);
        let (flops, flop_bits, inputs, input_bits, memories, memory_bits, wide_arith, _, _) =
            summarize_cone(&cone);
        prop_reports.push(PropertyComplexity {
            property: pname.clone(),
            flops,
            flop_bits,
            inputs,
            input_bits,
            memories,
            memory_bits,
            wide_arith,
        });
    }

    // Overall COI: union of selected properties (or all lines when design has no properties yet).
    let active_nodes: HashSet<u32> = if prop_targets.is_empty() {
        m.lines().map(|l| l.id).collect()
    } else {
        let roots: Vec<u32> = prop_targets.iter().map(|(_, r)| *r).collect();
        collect_coi_nodes(&m, &roots, &next_map)
    };

    let (
        total_flops,
        total_flop_bits,
        total_inputs,
        total_input_bits,
        total_memories,
        total_memory_bits,
        wide_arith_ops,
        max_wide_arith_width,
        max_memory_words,
    ) = summarize_cone(&active_nodes);

    let mut inst_acc: BTreeMap<&str, InstanceComplexity> = inst_map
        .iter()
        .map(|(p, mname)| {
            (
                p.as_str(),
                InstanceComplexity {
                    instance: p.clone(),
                    module: mname.clone(),
                    flops: 0,
                    flop_bits: 0,
                    memories: 0,
                    memory_bits: 0,
                    wide_arith: 0,
                },
            )
        })
        .collect();

    for &nid in &active_nodes {
        let Some(l) = m.line(nid) else { continue };
        let owner = node_inst.get(&nid).copied().unwrap_or(top.as_str());
        let Some(entry) = inst_acc.get_mut(owner) else {
            continue;
        };
        match l.op {
            Op::State => {
                let sym = l.symbol.as_deref().unwrap_or("");
                if is_helper_sym(sym) {
                    continue;
                }
                if let Some(sid) = l.sort {
                    if let Some(w) = m.width(sid) {
                        entry.flops += 1;
                        entry.flop_bits += u64::from(w);
                    } else if let Some((words, dw)) = array_shape(&m, sid) {
                        entry.memories += 1;
                        entry.memory_bits = entry
                            .memory_bits
                            .saturating_add(words.saturating_mul(u64::from(dw)));
                    }
                }
            }
            op if is_wide_arith(op) => {
                if let Some(w) = l.sort.and_then(|sid| m.width(sid)) {
                    if w >= 16 {
                        entry.wide_arith += 1;
                    }
                }
            }
            _ => {}
        }
    }

    let mut instances: Vec<InstanceComplexity> = inst_acc.into_values().collect();
    instances.sort_by(|a, b| {
        (b.flop_bits + b.memory_bits, b.wide_arith)
            .cmp(&(a.flop_bits + a.memory_bits, a.wide_arith))
    });

    Ok(ComplexityReport {
        top,
        total_flops,
        total_flop_bits,
        total_inputs,
        total_input_bits,
        total_memories,
        total_memory_bits,
        wide_arith_ops,
        max_wide_arith_width,
        max_memory_words,
        instances,
        properties: prop_reports,
    })
}

pub fn format_complexity(r: &ComplexityReport) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "COI complexity ({}): {} flops ({} bits), {} inputs ({} bits), {} memories ({} bits), {} wide arith ops (>=16b)",
        r.top,
        r.total_flops,
        r.total_flop_bits,
        r.total_inputs,
        r.total_input_bits,
        r.total_memories,
        r.total_memory_bits,
        r.wide_arith_ops
    );
    let _ = writeln!(
        out,
        "{:<32} {:<18} {:>7} {:>10} {:>6} {:>10} {:>10}",
        "instance", "module", "flops", "flop_bits", "mems", "mem_bits", "wide_arith"
    );
    let _ = writeln!(out, "{}", "-".repeat(98));
    for inst in &r.instances {
        let _ = writeln!(
            out,
            "{:<32} {:<18} {:>7} {:>10} {:>6} {:>10} {:>10}",
            inst.instance,
            inst.module,
            inst.flops,
            inst.flop_bits,
            inst.memories,
            inst.memory_bits,
            inst.wide_arith
        );
    }
    if !r.properties.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "{:<32} {:>7} {:>10} {:>7} {:>10} {:>6} {:>10}",
            "property", "flops", "flop_bits", "inputs", "in_bits", "mems", "wide_arith"
        );
        let _ = writeln!(out, "{}", "-".repeat(88));
        for p in &r.properties {
            let _ = writeln!(
                out,
                "{:<32} {:>7} {:>10} {:>7} {:>10} {:>6} {:>10}",
                p.property, p.flops, p.flop_bits, p.inputs, p.input_bits, p.memories, p.wide_arith
            );
        }
    }
    out
}

/// Recommends (and optionally applies) instance blackboxing and multiplier/memory auto-boxing.
pub fn blackbox_assistant(
    s: &mut Session,
    threshold_bits: u64,
    apply: bool,
) -> Result<BboxAdvice, String> {
    let rep = fv_complexity(s, None)?;
    let top_prefix = format!("{}.", rep.top);
    let mut suggestions: Vec<BboxSuggestion> = Vec::new();

    // 1. Sub-instances exceeding `threshold_bits` or containing wide datapath multipliers.
    for inst in &rep.instances {
        if inst.instance == rep.top {
            continue;
        }
        let impact = inst.flop_bits.saturating_add(inst.memory_bits);
        if impact >= threshold_bits || inst.wide_arith > 0 {
            let rel = inst
                .instance
                .strip_prefix(&top_prefix)
                .unwrap_or(&inst.instance)
                .to_string();
            // Do not auto-box an instance that owns active RTL assertions unless user manually boxes it.
            let owns_assert = s.props.iter().any(|p| {
                p.rtl
                    && p.enabled
                    && p.kind == Kind::Assert
                    && p.name.starts_with(&format!("{}.", inst.instance))
            });
            let reason = format!(
                "{} ({}) contributes {} flop bits, {} memory bits, and {} wide arithmetic ops{}",
                rel,
                inst.module,
                inst.flop_bits,
                inst.memory_bits,
                inst.wide_arith,
                if owns_assert {
                    " [contains RTL assertions: review before boxing]"
                } else {
                    ""
                }
            );
            suggestions.push(BboxSuggestion {
                kind: if owns_assert {
                    "instance_review"
                } else {
                    "instance"
                },
                command: format!("elaborate -bbox_i {{{rel}}}"),
                target: rel,
                reason,
                impact_bits: impact.max(inst.wide_arith as u64 * 16),
            });
        }
    }

    // 2. Wide arithmetic (`-bbox_mul`).
    if rep.wide_arith_ops > 0 && s.design.bbox_mul.is_none() {
        suggestions.push(BboxSuggestion {
            kind: "multiplier",
            target: "16".into(),
            command: "elaborate -bbox_mul 16".into(),
            reason: format!(
                "{} wide arithmetic operator(s) (up to {} bits) in COI",
                rep.wide_arith_ops, rep.max_wide_arith_width
            ),
            impact_bits: u64::from(rep.max_wide_arith_width) * rep.wide_arith_ops as u64,
        });
    }

    // 3. Large memories (`-bbox_a`).
    if rep.total_memories > 0
        && (rep.total_memory_bits >= threshold_bits || rep.max_memory_words >= 64)
        && s.design.bbox_a.is_none()
    {
        let min_words = if let Some(d) = &s.elab {
            d.stats
                .as_ref()
                .and_then(|st| st.memories.iter().map(|m| m.words).min())
                .unwrap_or(rep.max_memory_words)
        } else {
            rep.max_memory_words
        }
        .max(1);
        suggestions.push(BboxSuggestion {
            kind: "memory",
            target: min_words.to_string(),
            command: format!("elaborate -bbox_a {min_words}"),
            reason: format!(
                "{} memory array(s) ({} total bits, up to {} words) in COI",
                rep.total_memories, rep.total_memory_bits, rep.max_memory_words
            ),
            impact_bits: rep.total_memory_bits,
        });
    }

    suggestions.sort_by_key(|a| std::cmp::Reverse(a.impact_bits));

    if apply && !suggestions.is_empty() {
        let mut changed = false;
        for sug in &suggestions {
            match sug.kind {
                "instance" => {
                    if !s.design.bbox_instances.contains(&sug.target) {
                        s.design.bbox_instances.push(sug.target.clone());
                        changed = true;
                    }
                }
                "multiplier" if s.design.bbox_mul.is_none() => {
                    s.design.bbox_mul = Some(16);
                    changed = true;
                }
                "memory" if s.design.bbox_a.is_none() => {
                    if let Ok(n) = sug.target.parse::<usize>() {
                        s.design.bbox_a = Some(n);
                        changed = true;
                    }
                }
                _ => {}
            }
        }
        if changed {
            s.elaborate(None)
                .map_err(|e| format!("blackbox_assistant -apply: {e:#}"))?;
        }
    }

    Ok(BboxAdvice {
        threshold_bits,
        applied: apply,
        suggestions,
    })
}

pub fn format_bbox_advice(adv: &BboxAdvice) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "blackbox_assistant (threshold={} bits, applied={}): {} recommendation(s)",
        adv.threshold_bits,
        adv.applied,
        adv.suggestions.len()
    );
    for s in &adv.suggestions {
        let _ = writeln!(out, "  [{}] {}  -- {}", s.kind, s.command, s.reason);
    }
    out
}

/// Audits formal setup completeness (`check_fv_setup`, `report_fv_setup`, `get_reset_info`).
pub fn check_fv_setup(s: &Session) -> SetupReport {
    let configured_clocks: Vec<String> = s.clocks.iter().map(|c| c.signal.clone()).collect();
    let stats = s.elab.as_ref().and_then(|d| d.stats.as_ref());
    let rtl_clocks = stats.map(|st| st.clocks.clone()).unwrap_or_default();
    let rtl_resets = stats.map(|st| st.resets.clone()).unwrap_or_default();
    let total_flops = stats.map_or(0, |st| st.flops);
    let reset_regs: usize = rtl_resets.iter().map(|r| r.registers).sum();
    let resetless_flops = total_flops.saturating_sub(reset_regs);
    let undriven_signals = stats.map_or(0, |st| st.undriven);

    let unsupported_properties: Vec<String> = s
        .elab
        .as_ref()
        .map(|d| {
            d.properties
                .iter()
                .filter_map(|p| {
                    p.unsupported
                        .as_ref()
                        .or(p.error.as_ref())
                        .map(|why| format!("{}: {why}", p.name))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut findings = Vec::new();
    if !s.elaborated {
        findings.push(SetupFinding {
            severity: "warning",
            code: "SETUP-NO-ELAB",
            message: "design has not been elaborated yet (`elaborate` / `read_file`)".into(),
        });
    }
    if total_flops > 0 && configured_clocks.is_empty() {
        findings.push(SetupFinding {
            severity: "warning",
            code: "SETUP-NO-CLOCK",
            message: format!(
                "design has {total_flops} flop(s) (RTL clocks: {}) but no `clock` command was configured",
                rtl_clocks
                    .iter()
                    .map(|c| c.signal.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }
    if total_flops > 0 && s.reset.is_none() {
        findings.push(SetupFinding {
            severity: "warning",
            code: "SETUP-NO-RESET",
            message:
                "sequential design has no `reset` configured (`reset -none` or `reset <expr>`)"
                    .into(),
        });
    }
    if undriven_signals > 0 {
        findings.push(SetupFinding {
            severity: "info",
            code: "SETUP-UNDRIVEN",
            message: format!(
                "{undriven_signals} internal signal range(s) have no driver and act as free inputs"
            ),
        });
    }
    if resetless_flops > 0 {
        findings.push(SetupFinding {
            severity: "info",
            code: "SETUP-RESETLESS-FLOPS",
            message: format!(
                "{resetless_flops} of {total_flops} flop(s) have no asynchronous reset (may have sync reset or start uninitialized; run `check_xprop` to audit)"
            ),
        });
    }
    if !s.assumed_from_assert.is_empty() {
        findings.push(SetupFinding {
            severity: "info",
            code: "SETUP-ASSUME-FROM-ASSERT",
            message: format!(
                "{} helper assumption(s) promoted via `assume -from_assert`: {} (discharge original assertions independently for sound assume-guarantee)",
                s.assumed_from_assert.len(),
                s.assumed_from_assert.join(", ")
            ),
        });
    }
    if !s.counter_abstractions.is_empty() {
        findings.push(SetupFinding {
            severity: "info",
            code: "SETUP-COUNTER-ABSTRACT",
            message: format!(
                "{} over-approximate counter abstraction(s) active (`proven` is sound; `cex` may use an abstract counter jump)",
                s.counter_abstractions.len()
            ),
        });
    }
    for u in &unsupported_properties {
        findings.push(SetupFinding {
            severity: "warning",
            code: "SETUP-UNSUPPORTED-PROP",
            message: u.clone(),
        });
    }
    for p in &s.props {
        if let Status::Error { message } = &p.status {
            findings.push(SetupFinding {
                severity: "error",
                code: "SETUP-PROP-ERROR",
                message: format!("{}: {message}", p.name),
            });
        }
    }

    let mut black_boxes = s.design.bbox.clone();
    black_boxes.extend(s.design.bbox_instances.iter().cloned());
    let cut_points = s.stopats_of(&s.current_task).to_vec();

    SetupReport {
        elaborated: s.elaborated,
        top: s.design.top.clone(),
        configured_clocks,
        rtl_clocks,
        reset: ResetSummary {
            configured_reset: s.reset.clone(),
            rtl_resets,
            resetless_flops,
        },
        undriven_signals,
        black_boxes,
        cut_points,
        counter_abstractions: s.counter_abstractions.clone(),
        helper_assumptions: s.assumed_from_assert.clone(),
        unsupported_properties,
        findings,
    }
}

pub fn format_setup_report(r: &SetupReport) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "FV setup report (elaborated={}, top={}):",
        r.elaborated,
        r.top.as_deref().unwrap_or("-")
    );
    let _ = writeln!(
        out,
        "  clocks (configured): {}",
        if r.configured_clocks.is_empty() {
            "none".into()
        } else {
            r.configured_clocks.join(", ")
        }
    );
    let _ = writeln!(
        out,
        "  reset  (configured): {} (resetless flops: {})",
        r.reset.configured_reset.as_deref().unwrap_or("none"),
        r.reset.resetless_flops
    );
    let _ = writeln!(out, "  undriven signals   : {}", r.undriven_signals);
    let _ = writeln!(
        out,
        "  black boxes        : {}",
        if r.black_boxes.is_empty() {
            "none".into()
        } else {
            r.black_boxes.join(", ")
        }
    );
    let _ = writeln!(
        out,
        "  cut points         : {}",
        if r.cut_points.is_empty() {
            "none".into()
        } else {
            r.cut_points.join(", ")
        }
    );
    let _ = writeln!(
        out,
        "  abstractions       : {}",
        if r.counter_abstractions.is_empty() {
            "none".into()
        } else {
            format_abstractions(&r.counter_abstractions)
        }
    );
    let _ = writeln!(
        out,
        "  helper assumes     : {}",
        if r.helper_assumptions.is_empty() {
            "none".into()
        } else {
            r.helper_assumptions.join(", ")
        }
    );
    if r.findings.is_empty() {
        let _ = writeln!(out, "  status             : clean (0 findings)");
    } else {
        let _ = writeln!(out, "  findings ({}):", r.findings.len());
        for f in &r.findings {
            let _ = writeln!(out, "    [{}] {}: {}", f.severity, f.code, f.message);
        }
    }
    out
}

pub fn format_abstractions(abs: &[CounterAbstraction]) -> String {
    if abs.is_empty() {
        return String::new();
    }
    abs.iter()
        .map(|a| match a.threshold {
            Some(k) => format!("counter {} (threshold={k})", a.signal),
            None => format!("counter {} (free)", a.signal),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn bits_of_u64_msb_first(val: u64, width: u32) -> String {
    (0..width)
        .rev()
        .map(|bit| {
            if bit < 64 && ((val >> bit) & 1) == 1 {
                '1'
            } else {
                '0'
            }
        })
        .collect()
}

/// Applies sound over-approximate counter jump abstractions (`abstract -counter <sig> [-threshold K]`)
/// to a lowered Btor2 model (including any `; informal-trace-outputs` section).
pub fn apply_counter_abstractions(
    full_btor: &str,
    abstractions: &[CounterAbstraction],
    top: Option<&str>,
) -> Result<String, String> {
    if abstractions.is_empty() {
        return Ok(full_btor.to_string());
    }
    let m = parse_full_model(full_btor)?;

    // Resolve each abstraction's target `Op::State` line ID and BitVec width.
    let mut state_abs: HashMap<u32, &CounterAbstraction> = HashMap::new();
    for abs in abstractions {
        let mut cands = vec![abs.signal.clone()];
        if let Some(t) = top.filter(|t| !t.is_empty()) {
            if let Some(rest) = abs.signal.strip_prefix(t).and_then(|r| r.strip_prefix('.')) {
                cands.push(rest.to_string());
            } else {
                cands.push(format!("{t}.{}", abs.signal));
            }
        }
        let found = m.with_op(Op::State).find(|l| {
            l.symbol
                .as_deref()
                .is_some_and(|sym| cands.iter().any(|c| c == sym))
        });
        let Some(st_line) = found else {
            return Err(format!(
                "abstract -counter: state register `{}` not found in lowered model",
                abs.signal
            ));
        };
        let Some(_w) = st_line.sort.and_then(|sid| m.width(sid)) else {
            return Err(format!(
                "abstract -counter: `{}` is not a bit-vector register",
                abs.signal
            ));
        };
        state_abs.insert(st_line.id, abs);
    }

    let mut out = String::new();
    let mut next_id: u32 = 1;
    let mut alloc = || -> u32 {
        let id = next_id;
        next_id += 1;
        id
    };

    let mut sort_map: HashMap<u32, u32> = HashMap::new();
    let mut sort1: Option<u32> = None;
    for entry in m.entries() {
        if let Entry::Sort { id, sort, symbol } = entry {
            let sid = alloc();
            match *sort {
                Sort::BitVec(w) => {
                    let _ = write!(out, "{sid} sort bitvec {w}");
                    if w == 1 && sort1.is_none() {
                        sort1 = Some(sid);
                    }
                }
                Sort::Array { index, element } => {
                    let ix = sort_map[&index];
                    let el = sort_map[&element];
                    let _ = write!(out, "{sid} sort array {ix} {el}");
                }
            }
            if let Some(sym) = symbol {
                let _ = write!(out, " {sym}");
            }
            out.push('\n');
            sort_map.insert(*id, sid);
        }
    }
    let sort1 = match sort1 {
        Some(s1) => s1,
        None => {
            let s1 = alloc();
            let _ = writeln!(out, "{s1} sort bitvec 1");
            s1
        }
    };

    let mut node_map: HashMap<u32, u32> = HashMap::new();
    let mut trace_outputs: Vec<(i64, Option<String>)> = Vec::new();
    let tr = |arg: i64, map: &HashMap<u32, u32>| -> i64 {
        let base = arg.unsigned_abs() as u32;
        let mapped = map[&base] as i64;
        if arg < 0 {
            -mapped
        } else {
            mapped
        }
    };

    for l in m.lines() {
        if l.op == Op::Output {
            let mapped = tr(l.args[0], &node_map);
            trace_outputs.push((mapped, l.symbol.clone()));
            continue;
        }
        if l.op == Op::Next {
            let orig_st = l.args[0].unsigned_abs() as u32;
            if let Some(abs) = state_abs.get(&orig_st) {
                let orig_srt = l.sort.unwrap();
                let w = m.width(orig_srt).unwrap_or(1);
                let srt = sort_map[&orig_srt];
                let st_new = node_map[&orig_st];
                let orig_nxt = tr(l.args[1], &node_map);
                let clean: String = abs
                    .signal
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                    .collect();

                let jump_en = alloc();
                let _ = writeln!(out, "{jump_en} input {sort1} informal_abs_jump_{clean}");
                let mux_nxt = match abs.threshold {
                    Some(k) => {
                        let target_val = k.saturating_sub(1);
                        let bits = bits_of_u64_msb_first(target_val, w);
                        let c_id = alloc();
                        let _ = writeln!(out, "{c_id} const {srt} {bits}");
                        let below = alloc();
                        let _ = writeln!(out, "{below} ult {sort1} {st_new} {c_id}");
                        let do_jump = alloc();
                        let _ = writeln!(out, "{do_jump} and {sort1} {jump_en} {below}");
                        let mux = alloc();
                        let _ = writeln!(out, "{mux} ite {srt} {do_jump} {c_id} {orig_nxt}");
                        mux
                    }
                    None => {
                        let free_val = alloc();
                        let _ = writeln!(out, "{free_val} input {srt} informal_abs_val_{clean}");
                        let mux = alloc();
                        let _ = writeln!(out, "{mux} ite {srt} {jump_en} {free_val} {orig_nxt}");
                        mux
                    }
                };
                let nid = alloc();
                let _ = write!(out, "{nid} next {srt} {st_new} {mux_nxt}");
                if let Some(sym) = &l.symbol {
                    let _ = write!(out, " {sym}");
                }
                out.push('\n');
                node_map.insert(l.id, nid);
                continue;
            }
        }

        let nid = alloc();
        let _ = write!(out, "{nid} {}", l.op.name());
        if let Some(s) = l.sort {
            let _ = write!(out, " {}", sort_map[&s]);
        }
        if l.op == Op::Justice {
            let _ = write!(out, " {}", l.args.len());
        }
        for &a in &l.args {
            let _ = write!(out, " {}", tr(a, &node_map));
        }
        for n in &l.nums {
            let _ = write!(out, " {n}");
        }
        if let Some(sym) = &l.symbol {
            let _ = write!(out, " {sym}");
        }
        out.push('\n');
        node_map.insert(l.id, nid);
    }

    if !trace_outputs.is_empty() && full_btor.contains(informal_svlang::TRACE_MARKER) {
        let _ = writeln!(out, "{}", informal_svlang::TRACE_MARKER);
        for (arg, sym) in trace_outputs {
            let oid = alloc();
            let _ = write!(out, "{oid} output {arg}");
            if let Some(s) = sym {
                let _ = write!(out, " {s}");
            }
            out.push('\n');
        }
    }

    Ok(out)
}
