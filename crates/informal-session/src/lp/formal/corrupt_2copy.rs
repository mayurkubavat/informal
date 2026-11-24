//! Exact 2-copy self-composition corruption proofs (`check_lp -prove_corruption`).
//!
//! Instantiates `PaModel` in `PaMode::TwoCopy` and runs `build_miter_btor2` for
//! each top-level output observation sink. Passing outputs are certified free of
//! both isolation-corruption leakage and post-wakeup unretained-state X-leakage;
//! failing outputs register `LowPower::lp_corrupt_<out>` with `prop_{idx}.trace.btor2`
//! and VCD replay (`m0.*`/`m1.*` + `lp.pd.<dom>.*`) for `why_cex`.

use std::collections::{HashMap, HashSet, VecDeque};

use informal_ir::{Model, Op};
use informal_svlang::struct_graph::StructuralGraph;
use informal_upf::{PowerDomainId, ResolvedPowerIntent};

use super::aag::{
    push_formal_violation, register_and_prove_miter_prop, reset_low_power_task, LOW_POWER_TASK,
};
use super::iso_ret_models::{
    build_pa_model, domain_of_signal_path, strip_top_prefix, PaMode, PaModel,
};
use super::pa_instrument::{
    compile_bool_expr, domain_sig_by_id, resolve_btor_sig, sanitize_sym, BtorEmitter,
};
use super::power_expr::{extract_domain_controls, BoolExpr, DomainPowerControls};
use crate::lp::static_rtl::assign_cell_domains;
use crate::miter::{build_miter_btor2, parse_full_model, MiterSpec};
use crate::session::{Session, Status};

/// Runs exact 2-copy corruption proofs across all top-level outputs.
pub fn run_prove_corruption(
    s: &mut Session,
    graph: &StructuralGraph,
    resolved: &ResolvedPowerIntent,
    ensure_task: bool,
) -> Result<(), String> {
    if ensure_task && !s.tasks.iter().any(|t| t.name == LOW_POWER_TASK) {
        reset_low_power_task(s)?;
    }
    let task_name = if s.tasks.iter().any(|t| t.name == LOW_POWER_TASK) {
        LOW_POWER_TASK
    } else {
        s.current_task.as_str()
    };
    let (base_btor, _) = s.lower_for_coverage(task_name, false, false)?;
    let top = s.design.top.clone();
    let top_ref = top.as_deref();
    let base_model = parse_full_model(&base_btor)?;

    // Detect external-PMU domains (power switch control & isolation enable are both
    // unconstrained top-level primary inputs, with no retention strategy declared).
    let controls = extract_domain_controls(resolved);
    let ext_pmu_domains = detect_external_pmu_domains(&base_model, &controls, top_ref);

    let reset_steps = if s.reset.is_some() {
        s.reset_step_count()
    } else {
        0
    };
    let mut pa = build_pa_model(
        &base_btor,
        graph,
        resolved,
        top_ref,
        PaMode::TwoCopy,
        reset_steps,
    )?;

    // For external-PMU domains without retention/CSOA, assume the external PMU protocol
    // `(!pwr_on) |-> iso_en` and restrict corruption to combinational boundaries so
    // block-level unit tests without a wake-reset FSM don't report spurious state leaks.
    if !ext_pmu_domains.is_empty() {
        let ext_no_ret: HashSet<PowerDomainId> = ext_pmu_domains
            .iter()
            .copied()
            .filter(|&pd| {
                let c = controls.iter().find(|c| c.pd_id == pd);
                c.is_some_and(|c| {
                    c.ret_save_expr.is_none()
                        && c.ret_restore_expr.is_none()
                        && c.csoa_expr.is_none()
                })
            })
            .collect();

        let pm = parse_full_model(&pa.btor2)?;
        let mut em = BtorEmitter::new(&pm);
        for &pd in &ext_pmu_domains {
            if let Some(ctrl) = controls.iter().find(|c| c.pd_id == pd) {
                if let (Some(ref on_e), Some(ref iso_e)) = (&ctrl.on_expr, &ctrl.iso_active_expr) {
                    if let (Ok(on_n), Ok(iso_n)) = (
                        compile_bool_expr(&mut em, on_e, &ctrl.scope, top_ref),
                        compile_bool_expr(&mut em, iso_e, &ctrl.scope, top_ref),
                    ) {
                        let off_n = em.not1(on_n);
                        let imp = em.binary("implies", 1, off_n, iso_n);
                        em.emit_constraint(imp);
                    }
                }
            }
        }
        pa.btor2 = em.finish(&HashMap::new(), &HashMap::new(), false);

        if !ext_no_ret.is_empty() {
            let ext_dom_names: HashSet<String> = ext_no_ret
                .iter()
                .map(|pd| resolved.intent.domains[pd.0 as usize].name.clone())
                .collect();
            pa.cond_state_corruptions.retain(|cd| {
                pa.non_retained_states
                    .get(&cd.node_id)
                    .is_none_or(|(_, dname)| !ext_dom_names.contains(dname))
            });
        }
    }

    let pa_model = parse_full_model(&pa.btor2)?;
    let cell_domain = assign_cell_domains(graph, resolved);
    let top_outputs = collect_top_output_targets(s, &pa_model, top_ref);
    if top_outputs.is_empty() {
        return Ok(());
    }

    let skip_steps = if s.reset.is_some() {
        s.reset_step_count()
    } else {
        0
    };
    let active_engines = s.active_engines();
    let explicit_filter = !s.engine_filter.is_empty();

    for (obs_ref, obs_w, out_name) in top_outputs {
        // Guard output if the output port itself belongs to a switchable domain.
        let guard_ref = domain_of_signal_path(graph, &cell_domain, top_ref, &out_name)
            .and_then(|pd| domain_sig_by_id(&pa.domains, pd))
            .filter(|d| d.is_corruptible)
            .map(|d| d.on);

        let short = format!("lp_corrupt_{}", sanitize_sym(strip_top_prefix(&out_name, top_ref)));
        let full_name = format!("{LOW_POWER_TASK}::{short}");
        let spec = MiterSpec {
            diverging_inputs: HashSet::new(),
            diverging_nodes: HashMap::new(),
            conditional_diverging_nodes: pa.cond_divergences.clone(),
            conditional_state_corruptions: pa.cond_state_corruptions.clone(),
            uninit_states_diverge: false,
            tied_uninit_states: HashSet::new(),
            skip_steps,
            guard_ref,
            target: (obs_ref, obs_w, short.clone()),
            emit_trace_outputs: true,
        };
        let full_miter = build_miter_btor2(&pa_model, &spec)?;
        let prop_idx = register_and_prove_miter_prop(
            s,
            &full_name,
            format!("2-copy power-aware corruption proof on output `{out_name}`"),
            &full_miter,
            &active_engines,
            explicit_filter,
        )?;

        if matches!(s.props[prop_idx].status, Status::Cex { .. }) {
            let trace = s.props[prop_idx].trace.clone();
            let (rule_id, src_dom, dst_dom, detail) =
                classify_corruption_cex(&pa_model, &pa, obs_ref, &out_name);
            push_formal_violation(
                s,
                rule_id,
                src_dom,
                dst_dom,
                strip_top_prefix(&out_name, top_ref).to_string(),
                detail,
                Some(full_name),
                trace,
            );
        }
    }

    Ok(())
}

/// Identifies domains whose `on_expr` and `iso_active_expr` are both driven
/// solely by unconstrained top-level primary inputs (external PMU).
pub(crate) fn detect_external_pmu_domains(
    m: &Model,
    controls: &[DomainPowerControls],
    top: Option<&str>,
) -> HashSet<PowerDomainId> {
    let mut out = HashSet::new();
    for ctrl in controls {
        let (Some(ref on_e), Some(ref iso_e)) = (&ctrl.on_expr, &ctrl.iso_active_expr) else {
            continue;
        };
        if expr_uses_only_primary_inputs(m, on_e, &ctrl.scope, top)
            && expr_uses_only_primary_inputs(m, iso_e, &ctrl.scope, top)
        {
            out.insert(ctrl.pd_id);
        }
    }
    out
}

fn expr_uses_only_primary_inputs(
    m: &Model,
    expr: &BoolExpr,
    scope: &str,
    top: Option<&str>,
) -> bool {
    match expr {
        BoolExpr::ConstBool(_) | BoolExpr::Lit { .. } => true,
        BoolExpr::Sig(sig) => {
            let norm_scope = scope.trim_matches('/').replace('/', ".");
            let resolved = resolve_btor_sig(m, top, sig).or_else(|| {
                if !norm_scope.is_empty() && !sig.contains('.') && !sig.contains('/') {
                    resolve_btor_sig(m, top, &format!("{norm_scope}.{sig}"))
                } else {
                    None
                }
            });
            resolved.is_some_and(|(nref, _)| {
                m.line(nref.unsigned_abs() as u32)
                    .is_some_and(|l| l.op == Op::Input)
            })
        }
        BoolExpr::Not(x) => expr_uses_only_primary_inputs(m, x, scope, top),
        BoolExpr::And(a, b)
        | BoolExpr::Or(a, b)
        | BoolExpr::Xor(a, b)
        | BoolExpr::Eq(a, b)
        | BoolExpr::Neq(a, b) => {
            expr_uses_only_primary_inputs(m, a, scope, top)
                && expr_uses_only_primary_inputs(m, b, scope, top)
        }
    }
}

/// Collects all top-level output ports resolved to `(node_ref, width, full_name)`.
pub(crate) fn collect_top_output_targets(
    s: &Session,
    m: &Model,
    top: Option<&str>,
) -> Vec<(i64, u32, String)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let top_ports: Vec<String> = s
        .design
        .hierarchy
        .iter()
        .find(|sc| top.is_some_and(|t| sc.name == t || sc.module == t))
        .or_else(|| s.design.hierarchy.first())
        .map(|sc| {
            sc.signals
                .iter()
                .filter(|sig| {
                    sig.kind == "port" && matches!(sig.dir.as_deref(), Some("output" | "inout"))
                })
                .map(|sig| sig.name.clone())
                .collect()
        })
        .unwrap_or_default();

    for port in top_ports {
        if let Some((nref, w)) = resolve_btor_sig(m, top, &port) {
            if seen.insert(port.clone()) {
                out.push((nref, w, port));
            }
        }
    }
    if out.is_empty() {
        let top_prefix = top.map(|t| format!("{t}.")).unwrap_or_default();
        for l in m.with_op(Op::Output) {
            let (Some(&arg), Some(sym)) = (l.args.first(), l.symbol.as_deref()) else {
                continue;
            };
            if sym.starts_with("lp.") || sym.starts_with("informal") {
                continue;
            }
            let is_top = match sym.strip_prefix(&top_prefix) {
                Some(rest) => !rest.contains('.'),
                None => !sym.contains('.'),
            };
            if !is_top {
                continue;
            }
            if let Some(w) = m
                .line(arg.unsigned_abs() as u32)
                .and_then(|b| b.sort)
                .and_then(|sid| m.width(sid))
            {
                if seen.insert(sym.to_string()) {
                    out.push((arg, w, sym.to_string()));
                }
            }
        }
    }
    out
}

/// Computes the set of Btor2 node IDs in the backward structural cone of `root_ref`
/// (traversing through `Op::Next` of reachable states to capture sequential reachability).
pub(crate) fn backward_cone_nodes(m: &Model, root_ref: i64) -> HashSet<u32> {
    let mut state_next: HashMap<u32, i64> = HashMap::new();
    for l in m.with_op(Op::Next) {
        if l.args.len() >= 2 {
            state_next.insert(l.args[0].unsigned_abs() as u32, l.args[1]);
        }
    }
    let mut vis = HashSet::new();
    let mut q = VecDeque::new();
    let root_id = root_ref.unsigned_abs() as u32;
    vis.insert(root_id);
    q.push_back(root_id);
    while let Some(nid) = q.pop_front() {
        let Some(l) = m.line(nid) else { continue };
        for &a in &l.args {
            let aid = a.unsigned_abs() as u32;
            if vis.insert(aid) {
                q.push_back(aid);
            }
        }
        if l.op == Op::State {
            if let Some(&nxt) = state_next.get(&nid) {
                let nid2 = nxt.unsigned_abs() as u32;
                if vis.insert(nid2) {
                    q.push_back(nid2);
                }
            }
        }
    }
    vis
}

fn classify_corruption_cex(
    pa_model: &Model,
    pa: &PaModel,
    obs_ref: i64,
    out_name: &str,
) -> (&'static str, Option<String>, Option<String>, String) {
    let cone = backward_cone_nodes(pa_model, obs_ref);
    if let Some(c) = pa.crossings.iter().find(|c| {
        !c.has_iso
            && c.divergence_node
                .is_some_and(|dn| cone.contains(&dn))
    }) {
        return (
            "ISO_CORRUPTION_LEAK",
            Some(c.src_domain_name.clone()),
            Some(c.dst_domain_name.clone()),
            format!(
                "power-off corruption from unisolated crossing `{}` ({} -> {}) propagates to output `{out_name}`",
                c.signal, c.src_domain_name, c.dst_domain_name
            ),
        );
    }
    if let Some((_, (reg, dname))) = pa
        .non_retained_states
        .iter()
        .find(|(&st_id, _)| cone.contains(&st_id))
    {
        return (
            "RET_POST_WAKEUP_X",
            Some(dname.clone()),
            None,
            format!(
                "unretained register `{reg}` in domain `{dname}` leaks post-wakeup corruption to output `{out_name}`"
            ),
        );
    }
    let src_d = pa
        .crossings
        .iter()
        .find(|c| c.divergence_node.is_some_and(|dn| cone.contains(&dn)))
        .map(|c| c.src_domain_name.clone());
    (
        "ISO_CORRUPTION_LEAK",
        src_d,
        None,
        format!("power-aware 2-copy corruption propagates to output `{out_name}`"),
    )
}
