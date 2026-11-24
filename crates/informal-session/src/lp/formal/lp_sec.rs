//! Power-aware Sequential Equivalence Checking (`check_lp -sec`) and
//! Power-aware Combinational Logic Equivalence Checking (`check_lp -lec`).
//!
//! - `-sec`: instruments `imp` (power-aware RTL + UPF) in `PaMode::SingleCopySec`
//!   (UPF isolation clamping, retention shadow flops, and power-off corruption of
//!   non-retained flops) and proves equivalence against golden non-PG `spec` via
//!   [`crate::sec::prove_sec_from_btor`].
//! - `-lec`: instruments `spec` (RTL + UPF) in `PaMode::IsoClampOnly` (UPF
//!   isolation clamping) and proves 1-cycle combinational equivalence against
//!   synthesized gate-level `imp` (with physical stdcell ISO/LS cells).

use std::sync::Arc;

use super::aag::push_formal_violation;
use super::iso_ret_models::{build_pa_model, PaMode};
use crate::lp::ensure_resolved_with_finder;
use crate::lp::finder_bridge::DesignFinder;
use crate::sec::{prove_sec_from_btor, SecCheck, SecStatus};
use crate::session::Session;

/// Runs power-aware Sequential Equivalence Checking (`check_lp -sec`).
pub fn run_lp_sec(s: &mut Session) -> Result<(), String> {
    run_lp_equiv(s, false)
}

/// Runs power-aware Combinational Equivalence Checking (`check_lp -lec`).
pub fn run_lp_lec(s: &mut Session) -> Result<(), String> {
    run_lp_equiv(s, true)
}

fn run_lp_equiv(s: &mut Session, lec: bool) -> Result<(), String> {
    let cmd = if lec { "check_lp -lec" } else { "check_lp -sec" };
    let mut setup = s
        .sec
        .setup
        .clone()
        .ok_or_else(|| format!("{cmd}: run `check_sec -setup ...` first"))?;
    if lec {
        setup.lec = true;
    }
    let spec_top = setup
        .spec_top
        .clone()
        .ok_or_else(|| format!("{cmd}: missing spec top module"))?;
    let imp_top = setup
        .imp_top
        .clone()
        .ok_or_else(|| format!("{cmd}: missing imp top module"))?;

    let clk_list: Vec<(String, u32, u32)> = match &setup.clock {
        Some(c) => vec![(c.clone(), 1, 0)],
        None => s
            .clocks
            .iter()
            .map(|c| (c.signal.clone(), c.factor, c.phase))
            .collect(),
    };
    let effective_reset = if setup.lec {
        None
    } else {
        setup.reset.clone().or_else(|| s.reset.clone())
    };
    let clk_sig = clk_list.first().map_or("clk", |c| c.0.as_str());
    let spec_props: Vec<(String, String)> = setup
        .assumptions
        .iter()
        .enumerate()
        .map(|(idx, expr)| {
            let trimmed = expr.trim();
            let full = if trimmed.starts_with("assume ") {
                trimmed.to_string()
            } else if trimmed.starts_with("@(") {
                format!("assume property ({trimmed})")
            } else {
                format!("assume property (@(posedge {clk_sig}) ({trimmed}))")
            };
            (format!("_sec_assume_{idx}"), full)
        })
        .collect();

    let base_spec = informal_svlang::Request {
        files: setup.spec_files.clone(),
        defines: setup.spec_defines.clone(),
        incdirs: setup.spec_incdirs.clone(),
        top: Some(spec_top.clone()),
        params: setup.spec_params.clone(),
        trace_outputs: true,
        clocks: clk_list.clone(),
        properties: spec_props,
        reset: effective_reset.clone(),
        skip_initial_checks: s.design.skip_initial_checks,
        isunknown_is_false: s.design.isunknown_is_false,
        ..Default::default()
    };
    let spec_cd = informal_svlang::compile_cached(s.sec.spec_compiled.as_ref(), &base_spec)
        .map_err(|e| format!("{cmd} (spec lower failed): {e}"))?;
    let l_spec = informal_svlang::lower_from_compiled(&spec_cd, &base_spec);
    if !l_spec.ok {
        return Err(format!(
            "{cmd} (spec lower failed): {}",
            l_spec.errors.join("; ")
        ));
    }
    s.sec.spec_compiled = Some(Arc::clone(&spec_cd));
    let d_spec = informal_svlang::design_from_compiled(&spec_cd, &base_spec);

    let reset_steps = if effective_reset.is_some() { 1 } else { 0 };
    let base_imp = informal_svlang::Request {
        files: setup.imp_files.clone(),
        defines: setup.imp_defines.clone(),
        incdirs: setup.imp_incdirs.clone(),
        top: Some(imp_top.clone()),
        params: setup.imp_params.clone(),
        trace_outputs: true,
        clocks: clk_list,
        reset: effective_reset,
        skip_initial_checks: s.design.skip_initial_checks,
        isunknown_is_false: s.design.isunknown_is_false,
        ..Default::default()
    };
    let imp_cd = informal_svlang::compile_cached(s.sec.imp_compiled.as_ref(), &base_imp)
        .map_err(|e| format!("{cmd} (imp lower failed): {e}"))?;
    let l_imp = informal_svlang::lower_from_compiled(&imp_cd, &base_imp);
    if !l_imp.ok {
        return Err(format!(
            "{cmd} (imp lower failed): {}",
            l_imp.errors.join("; ")
        ));
    }
    s.sec.imp_compiled = Some(Arc::clone(&imp_cd));
    let d_imp = informal_svlang::design_from_compiled(&imp_cd, &base_imp);

    let (spec_btor, imp_btor) = if lec {
        let finder = DesignFinder::from_compiled(Some(&spec_cd), Some(&spec_top));
        let resolved = ensure_resolved_with_finder(s, &finder)?;
        let spec_graph = spec_cd.struct_graph();
        let pa_spec = build_pa_model(
            &l_spec.btor2,
            spec_graph,
            &resolved,
            Some(&spec_top),
            PaMode::IsoClampOnly,
            0,
        )?;
        (pa_spec.btor2, l_imp.btor2)
    } else {
        let finder = DesignFinder::from_compiled(Some(&imp_cd), Some(&imp_top));
        let resolved = ensure_resolved_with_finder(s, &finder)?;
        let imp_graph = imp_cd.struct_graph();
        let pa_imp = build_pa_model(
            &l_imp.btor2,
            imp_graph,
            &resolved,
            Some(&imp_top),
            PaMode::SingleCopySec,
            reset_steps,
        )?;
        (l_spec.btor2, pa_imp.btor2)
    };

    let checks: Vec<SecCheck> = prove_sec_from_btor(
        s,
        &setup,
        &spec_top,
        &imp_top,
        &d_spec,
        &d_imp,
        &spec_btor,
        &imp_btor,
        None,
        None,
    )?;

    let rule_id = if lec {
        "LP_LEC_MISMATCH"
    } else {
        "LP_SEC_MISMATCH"
    };
    for c in &checks {
        if matches!(c.status, SecStatus::Cex { .. }) {
            push_formal_violation(
                s,
                rule_id,
                None,
                None,
                c.imp_sig.clone(),
                format!(
                    "{cmd}: equivalence mismatch on `{}` (`{}` vs `{}`)",
                    c.name, c.spec_sig, c.imp_sig
                ),
                Some(c.name.clone()),
                c.trace.clone(),
            );
        }
    }
    s.sec.setup = Some(setup);
    s.sec.checks = checks;
    if !s.lp.stages_run.iter().any(|st| st == "formal") {
        s.lp.stages_run.push("formal".to_string());
    }
    Ok(())
}
