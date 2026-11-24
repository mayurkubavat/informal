//! Automatic Assertion Generation (AAG) for Formal Low-Power Verification
//! (`check_lp -stage formal` / `check_lpv`).
//!
//! Populates the `LowPower` task (inheriting `<embedded>` assumptions and
//! `stopat` cuts), generates PMU sequencing assertions, retention async-reset
//! checks, PST illegal-state safety & legal-state reachability covers, proves
//! them through the session portfolio, enriches CEX/cover traces with
//! `lp.pd.<domain>.{on,iso,ret,corrupt}` VCD swimlanes, and verifies 2-copy
//! `lp_ret_value_preserved_<reg>` proofs on the power-aware model.

use std::collections::HashSet;

use informal_engine::{portfolio, CheckRequest, ModelFormat, Verdict};
use informal_svlang::struct_graph::StructuralGraph;
use informal_upf::{PowerDomainId, ResolvedPowerIntent};

use super::iso_ret_models::{build_pa_model, PaMode, PaModel};
use super::pa_instrument::{inject_lp_domain_trace_signals, sanitize_sym};
use super::power_expr::{extract_domain_controls, normalize_sva_signal, BoolExpr};
use crate::lp::rules::lookup_rule;
use crate::lp::static_rtl::assign_cell_domains;
use crate::lp::{LpSeverity, LpStatus, LpViolation};
use crate::miter::{build_miter_btor2, parse_full_model, MiterSpec};
use crate::session::{Kind, Property, Session, Status, EMBEDDED};

pub const LOW_POWER_TASK: &str = "LowPower";

/// Ensures the `LowPower` task exists cleanly and inherits all enabled
/// `<embedded>` assumptions and cut points (`stopat`).
pub fn reset_low_power_task(s: &mut Session) -> Result<(), String> {
    if s.tasks.iter().any(|t| t.name == LOW_POWER_TASK) {
        s.delete_task(LOW_POWER_TASK)?;
    }
    s.create_task(LOW_POWER_TASK)?;
    let emb_assumes: Vec<usize> = (0..s.props.len())
        .filter(|&i| {
            let p = &s.props[i];
            p.task == EMBEDDED && p.kind == Kind::Assume && p.enabled
        })
        .collect();
    s.copy_into(LOW_POWER_TASK, &emb_assumes)?;
    s.copy_stopats(EMBEDDED, LOW_POWER_TASK)?;
    Ok(())
}

/// Runs AAG assertion/cover generation + proving in task `LowPower`, followed by
/// power-aware `lp_ret_value_preserved_<reg>` proofs.
pub fn run_aag(
    s: &mut Session,
    graph: &StructuralGraph,
    resolved: &ResolvedPowerIntent,
) -> Result<(), String> {
    reset_low_power_task(s)?;

    let top = s.design.top.clone();
    let top_ref = top.as_deref();
    let controls = extract_domain_controls(resolved);
    let cell_domain = assign_cell_domains(graph, resolved);
    let top_input_ports: HashSet<String> = s
        .design
        .hierarchy
        .iter()
        .find(|sc| top_ref.is_some_and(|t| sc.name == t || sc.module == t))
        .or_else(|| s.design.hierarchy.first())
        .map(|sc| {
            sc.signals
                .iter()
                .filter(|sig| sig.kind == "port" && sig.dir.as_deref() == Some("input"))
                .map(|sig| sig.name.clone())
                .collect()
        })
        .unwrap_or_default();

    let mut added_indices: Vec<usize> = Vec::new();
    let mut try_add = |sess: &mut Session, kind: Kind, name: String, expr: String| {
        if let Ok(full_name) = sess.add_property_in(LOW_POWER_TASK, kind, Some(name), &expr) {
            if let Some(idx) = sess.props.iter().position(|p| p.name == full_name) {
                if kind != Kind::Assume {
                    added_indices.push(idx);
                }
            }
        }
    };

    // 1. Per-domain PMU sequencing & retention ordering / async-reset assertions.
    for ctrl in &controls {
        let Some(ref on_expr) = ctrl.on_expr else {
            continue;
        };
        let dom = sanitize_sym(&ctrl.name);
        let on_sva = on_expr.to_sva(&ctrl.scope, top_ref);
        let iso_sva = ctrl
            .iso_active_expr
            .as_ref()
            .map(|e| e.to_sva(&ctrl.scope, top_ref));
        let ack_sva = ctrl
            .ack_sig
            .as_deref()
            .map(|sig| normalize_sva_signal(sig, &ctrl.scope, top_ref));

        let is_ext_pmu = expr_is_top_primary_inputs(on_expr, &ctrl.scope, top_ref, &top_input_ports)
            && ctrl
                .iso_active_expr
                .as_ref()
                .is_some_and(|e| expr_is_top_primary_inputs(e, &ctrl.scope, top_ref, &top_input_ports));

        if let Some(ref iso) = iso_sva {
            if is_ext_pmu {
                // External-PMU block-level verification: assume external PMU protocol.
                try_add(
                    s,
                    Kind::Assume,
                    format!("lp_ext_pmu_iso_before_pwroff_{dom}"),
                    format!("(!({on_sva})) |-> ({iso})"),
                );
            } else {
                // Isolation must be asserted whenever primary power is OFF.
                try_add(
                    s,
                    Kind::Assert,
                    format!("lp_iso_before_pwroff_{dom}"),
                    format!("(!({on_sva})) |-> ({iso})"),
                );

                // Isolation must remain asserted until power switch acknowledge is high.
                let hold_expr = if let Some(ref ack) = ack_sva {
                    format!("((!({ack})) || (!({on_sva}))) |-> ({iso})")
                } else {
                    format!("($rose({on_sva})) |-> ({iso})")
                };
                try_add(s, Kind::Assert, format!("lp_iso_hold_after_pwron_{dom}"), hold_expr);
            }
        }

        let save_sva = ctrl
            .ret_save_expr
            .as_ref()
            .map(|e| e.to_sva(&ctrl.scope, top_ref));
        let restore_sva = ctrl
            .ret_restore_expr
            .as_ref()
            .map(|e| e.to_sva(&ctrl.scope, top_ref));

        if let Some(ref save) = save_sva {
            let cond = match &iso_sva {
                Some(iso) => format!("(({on_sva}) && ({iso}))"),
                None => format!("({on_sva})"),
            };
            try_add(
                s,
                Kind::Assert,
                format!("lp_ret_save_order_{dom}"),
                format!("({save}) |-> {cond}"),
            );
        }

        if let Some(ref restore) = restore_sva {
            let mut reqs = vec![format!("({on_sva})")];
            if let Some(ref ack) = ack_sva {
                reqs.push(format!("({ack})"));
            }
            if let Some(ref iso) = iso_sva {
                reqs.push(format!("({iso})"));
            }
            try_add(
                s,
                Kind::Assert,
                format!("lp_ret_restore_order_{dom}"),
                format!("({restore}) |-> ({})", reqs.join(" && ")),
            );
        }

        if let (Some(ref save), Some(ref restore)) = (&save_sva, &restore_sva) {
            try_add(
                s,
                Kind::Assert,
                format!("lp_ret_save_restore_mutex_{dom}"),
                format!("!(({save}) && ({restore}))"),
            );
        }

        // Async reset cleanliness for retained registers in this domain.
        if save_sva.is_some() || restore_sva.is_some() {
            if let Some(rst_active_sva) =
                collect_domain_async_reset_sva(graph, &cell_domain, ctrl.pd_id, top_ref)
            {
                try_add(
                    s,
                    Kind::Assert,
                    format!("lp_ret_async_rst_clean_{dom}"),
                    format!("(!({on_sva})) |-> (!({rst_active_sva}))"),
                );
            }
        }
    }

    // 2. PST illegal-state safety assertions & legal-state reachability covers.
    let mut emitted_illegal_assert = false;
    for (ss_idx, states) in resolved.ss_states.iter().enumerate() {
        let ss_name = sanitize_sym(&resolved.intent.supply_sets[ss_idx].name);
        let ss_scope = &resolved.intent.supply_sets[ss_idx].scope;
        let mut legal_exprs = Vec::new();

        for st in states {
            let Some(ref raw_expr) = st.logic_expr else {
                continue;
            };
            let Ok(parsed) = BoolExpr::parse(raw_expr) else {
                continue;
            };
            let sva = parsed.to_sva(ss_scope, top_ref);
            let st_name = sanitize_sym(&st.state_name);
            if st.illegal {
                emitted_illegal_assert = true;
                try_add(
                    s,
                    Kind::Assert,
                    format!("lp_pst_no_illegal_{ss_name}_{st_name}"),
                    format!("!({sva})"),
                );
            } else {
                legal_exprs.push(sva.clone());
                try_add(
                    s,
                    Kind::Cover,
                    format!("lp_pst_reach_{ss_name}_{st_name}"),
                    sva,
                );
            }
        }

        if !emitted_illegal_assert && legal_exprs.len() >= 2 {
            emitted_illegal_assert = true;
            try_add(
                s,
                Kind::Assert,
                format!("lp_pst_no_illegal_{ss_name}"),
                format!("({})", legal_exprs.join(" || ")),
            );
        }
    }

    if !emitted_illegal_assert {
        for ctrl in &controls {
            if let (Some(ref on_e), Some(ref iso_e)) = (&ctrl.on_expr, &ctrl.iso_active_expr) {
                let dom = sanitize_sym(&ctrl.name);
                let on_sva = on_e.to_sva(&ctrl.scope, top_ref);
                let iso_sva = iso_e.to_sva(&ctrl.scope, top_ref);
                try_add(
                    s,
                    Kind::Assert,
                    format!("lp_pst_no_illegal_{dom}"),
                    format!("!((!({on_sva})) && (!({iso_sva})))"),
                );
            }
        }
    }

    // 3. Prove SVA AAG properties in `LowPower`.
    if !added_indices.is_empty() {
        s.prove(&added_indices)
            .map_err(|e| format!("check_lp -stage formal: {e:#}"))?;

        // Enrich any CEX/Cover traces with `lp.pd.<domain>.{on,iso,ret,corrupt}` VCD swimlanes
        // and record formal LP violations for failing AAG assertions.
        for &idx in &added_indices {
            let (prop_name, kind, status_clone, depth_opt) = {
                let p = &s.props[idx];
                let d = match p.status {
                    Status::Cex { depth, .. } | Status::Covered { depth, .. } => depth,
                    _ => None,
                };
                (p.name.clone(), p.kind, p.status.clone(), d)
            };

            if matches!(status_clone, Status::Cex { .. } | Status::Covered { .. }) {
                let prop_btor = s.workdir.join(format!("prop_{idx}.btor2"));
                let prop_trace_btor = s.workdir.join(format!("prop_{idx}.trace.btor2"));
                if let Ok(raw_trace) = std::fs::read_to_string(&prop_trace_btor) {
                    if let Ok(aug) = inject_lp_domain_trace_signals(&raw_trace, graph, resolved, top_ref) {
                        if std::fs::write(&prop_trace_btor, &aug).is_ok() {
                            if let Ok((vcd_path, _)) =
                                s.write_trace(&prop_name, &prop_btor, None, depth_opt)
                            {
                                s.props[idx].trace = Some(vcd_path);
                            }
                        }
                    }
                }
            }

            if kind == Kind::Assert && matches!(status_clone, Status::Cex { .. }) {
                let rule_id = if prop_name.contains("lp_pst_no_illegal_") {
                    "LP_PST_ILLEGAL_REACHED"
                } else {
                    "LP_PMU_SEQ_FAIL"
                };
                let trace = s.props[idx].trace.clone();
                push_formal_violation(
                    s,
                    rule_id,
                    None,
                    None,
                    prop_name.clone(),
                    format!("formal LP property `{prop_name}` falsified"),
                    Some(prop_name),
                    trace,
                );
            }
        }
    }

    // 4. Prove power-aware retention preservation (`lp_ret_value_preserved_<reg>`).
    let (base_btor, _) = s.lower_for_coverage(LOW_POWER_TASK, false, false)?;
    let reset_steps = if s.reset.is_some() {
        s.reset_step_count()
    } else {
        0
    };
    let pa = build_pa_model(&base_btor, graph, resolved, top_ref, PaMode::TwoCopy, reset_steps)?;
    prove_retention_preservation(s, &pa)?;

    Ok(())
}

fn prove_retention_preservation(s: &mut Session, pa: &PaModel) -> Result<(), String> {
    if pa.retained_regs.is_empty() {
        return Ok(());
    }
    let pa_model = parse_full_model(&pa.btor2)?;
    let skip_steps = if s.reset.is_some() {
        s.reset_step_count()
    } else {
        0
    };
    let active_engines = s.active_engines();
    let explicit_filter = !s.engine_filter.is_empty();
    std::fs::create_dir_all(&s.workdir).map_err(|e| format!("check_lp -stage formal: {e}"))?;

    for reg in &pa.retained_regs {
        // Guard: domain is ON and isolation is NOT active (normal post-wakeup operation).
        let (guard_btor, guard_ref) = {
            let mut em = super::pa_instrument::BtorEmitter::new(&pa_model);
            let not_iso = em.not1(reg.domain_iso);
            let g = em.and1(reg.domain_on, not_iso);
            let b = em.finish(
                &std::collections::HashMap::new(),
                &std::collections::HashMap::new(),
                false,
            );
            (b, g)
        };
        let gm = parse_full_model(&guard_btor)?;
        let short = format!("lp_ret_value_preserved_{}", reg.short_name);
        let full_name = format!("{LOW_POWER_TASK}::{short}");

        let spec = MiterSpec {
            diverging_inputs: HashSet::new(),
            diverging_nodes: std::collections::HashMap::new(),
            conditional_diverging_nodes: pa.cond_divergences.clone(),
            conditional_state_corruptions: pa.cond_state_corruptions.clone(),
            uninit_states_diverge: false,
            tied_uninit_states: HashSet::new(),
            skip_steps,
            guard_ref: Some(guard_ref),
            target: (reg.state_id as i64, reg.width, short.clone()),
            emit_trace_outputs: true,
        };
        let full_miter = build_miter_btor2(&gm, &spec)?;
        let prop_idx = register_and_prove_miter_prop(
            s,
            &full_name,
            format!("2-copy retention value preservation on {}", reg.reg_path),
            &full_miter,
            &active_engines,
            explicit_filter,
        )?;

        if matches!(s.props[prop_idx].status, Status::Cex { .. }) {
            let trace = s.props[prop_idx].trace.clone();
            push_formal_violation(
                s,
                "LP_RET_VALUE_CORRUPTED",
                Some(reg.domain_name.clone()),
                Some(reg.domain_name.clone()),
                reg.reg_path.clone(),
                format!(
                    "retained register `{}` in domain `{}` corrupted across sleep/wakeup cycle",
                    reg.reg_path, reg.domain_name
                ),
                Some(full_name),
                trace,
            );
        }
    }
    Ok(())
}

/// Registers a miter-backed property in `LowPower`, writes `prop_{idx}.btor2`
/// and `prop_{idx}.trace.btor2` (so `why_cex`, `trace_get`, `get_status`,
/// `get_stats`, and `report -csv -task LowPower` work natively), runs the
/// portfolio, and replays VCD on CEX.
pub(crate) fn register_and_prove_miter_prop(
    s: &mut Session,
    full_name: &str,
    expr_desc: String,
    full_miter_btor: &str,
    active_engines: &[std::sync::Arc<dyn informal_engine::Engine>],
    explicit_filter: bool,
) -> Result<usize, String> {
    if !s.tasks.iter().any(|t| t.name == LOW_POWER_TASK) {
        reset_low_power_task(s)?;
    }
    let prop_idx = if let Some(idx) = s.props.iter().position(|p| p.name == full_name) {
        s.props[idx].forget_result();
        s.props[idx].expr = expr_desc;
        idx
    } else {
        s.props.push(Property {
            name: full_name.to_string(),
            kind: Kind::Assert,
            expr: expr_desc,
            enabled: true,
            status: Status::Unprocessed,
            trace: None,
            source: None,
            rtl: false,
            engines: Vec::new(),
            ai: false,
            under_ai: s.ai_in_force_for(LOW_POWER_TASK),
            vacuity: None,
            witness_trace: None,
            task: LOW_POWER_TASK.to_string(),
            copy_of: None,
            bound: None,
            proof_core: None,
            from_assert: None,
            rtl_from_assert: false,
            stats: None,
        });
        s.props.len() - 1
    };

    std::fs::create_dir_all(&s.workdir).map_err(|e| format!("LP formal workdir: {e}"))?;
    let (engine_btor, _) = informal_svlang::split_trace(full_miter_btor);
    let btor_path = s.workdir.join(format!("prop_{prop_idx}.btor2"));
    let trace_btor_path = s.workdir.join(format!("prop_{prop_idx}.trace.btor2"));
    std::fs::write(&btor_path, engine_btor).map_err(|e| format!("LP formal write btor2: {e}"))?;
    std::fs::write(&trace_btor_path, full_miter_btor)
        .map_err(|e| format!("LP formal write trace.btor2: {e}"))?;

    let chosen = informal_bmc::select_for_btor(active_engines, engine_btor, explicit_filter);
    let req = CheckRequest {
        model: btor_path.clone(),
        format: ModelFormat::Btor2,
        timeout: s.time_limit,
        effort: s.effort,
        max_depth: s.max_trace_length,
    };
    let before = crate::stats::Usage::now();
    let run = portfolio::run_until(&chosen, &req, None, Some(s.stop.as_ref()));
    let usage = before.zip(crate::stats::Usage::now());
    let eng = run
        .winner
        .as_ref()
        .map(|w| w.engine.clone())
        .unwrap_or_else(|| "portfolio".to_string());
    let ms = run.wall_ms;

    let mut trace_path = None;
    let status = match portfolio::verdict(&run) {
        Verdict::Safe => Status::Proven { engine: eng, ms },
        Verdict::Unsafe { mut depth } => {
            let witness = run.winner.as_ref().and_then(|w| w.witness.as_deref());
            match s.write_trace(full_name, &btor_path, witness, depth) {
                Ok((path, d)) => {
                    depth.get_or_insert(d as u32);
                    trace_path = Some(path);
                    Status::Cex {
                        engine: eng,
                        depth,
                        ms,
                    }
                }
                Err(Some(why)) => Status::Undetermined {
                    reason: format!("counterexample did not replay: {why}"),
                },
                Err(None) => Status::Cex {
                    engine: eng,
                    depth,
                    ms,
                },
            }
        }
        Verdict::Unknown { reason } => Status::Undetermined { reason },
    };

    let stats = crate::stats::ProofStats::of_run(&status, &run.profile, ms, None, usage);
    let p = &mut s.props[prop_idx];
    p.status = status;
    p.trace = trace_path;
    p.engines = run.profile;
    p.stats = Some(stats);
    Ok(prop_idx)
}

fn expr_is_top_primary_inputs(
    expr: &BoolExpr,
    scope: &str,
    top: Option<&str>,
    top_inputs: &HashSet<String>,
) -> bool {
    match expr {
        BoolExpr::ConstBool(_) | BoolExpr::Lit { .. } => true,
        BoolExpr::Sig(sig) => {
            let norm = normalize_sva_signal(sig, scope, top);
            top_inputs.contains(&norm)
        }
        BoolExpr::Not(x) => expr_is_top_primary_inputs(x, scope, top, top_inputs),
        BoolExpr::And(a, b)
        | BoolExpr::Or(a, b)
        | BoolExpr::Xor(a, b)
        | BoolExpr::Eq(a, b)
        | BoolExpr::Neq(a, b) => {
            expr_is_top_primary_inputs(a, scope, top, top_inputs)
                && expr_is_top_primary_inputs(b, scope, top, top_inputs)
        }
    }
}

fn collect_domain_async_reset_sva(
    graph: &StructuralGraph,
    cell_domain: &[Option<PowerDomainId>],
    pd_id: PowerDomainId,
    top: Option<&str>,
) -> Option<String> {
    let mut terms = Vec::new();
    let mut seen = HashSet::new();
    for cell in &graph.cells {
        if cell_domain[cell.id.0 as usize] != Some(pd_id) {
            continue;
        }
        for seq in cell.flops.iter().chain(cell.latches.iter()) {
            let Some(rst_nid) = seq.async_reset else {
                continue;
            };
            let active_high = seq.async_reset_active_high.unwrap_or(true);
            let rst_path = &graph.net(rst_nid).full_path;
            let sva_sig = normalize_sva_signal(rst_path, "", top);
            let key = (sva_sig.clone(), active_high);
            if seen.insert(key) {
                if active_high {
                    terms.push(format!("({sva_sig})"));
                } else {
                    terms.push(format!("(!({sva_sig}))"));
                }
            }
        }
    }
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" || "))
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn push_formal_violation(
    s: &mut Session,
    rule_id: &str,
    domain_src: Option<String>,
    domain_dst: Option<String>,
    signal: String,
    message: String,
    formal_prop: Option<String>,
    trace: Option<String>,
) {
    let dup = s.lp.violations.iter().any(|v| {
        v.rule_id == rule_id
            && v.signal == signal
            && v.status == LpStatus::FormalCex
            && v.formal_prop == formal_prop
    });
    if dup {
        return;
    }
    let meta = lookup_rule(rule_id);
    let severity = meta.map_or(LpSeverity::Error, |m| m.default_severity);
    let next_id = s.lp.violations.iter().map(|v| v.id).max().unwrap_or(0) + 1;
    s.lp.violations.push(LpViolation {
        id: next_id,
        rule_id: rule_id.to_string(),
        severity,
        stage: "formal".to_string(),
        domain_src,
        domain_dst,
        signal,
        width_bits: 1,
        raw_crossings: 1,
        message,
        upf_loc: None,
        rtl_loc: None,
        pst_witness: None,
        status: LpStatus::FormalCex,
        waiver_id: None,
        formal_prop,
        trace,
        auto_formal_meta: None,
    });
}
