//! Formal Low-Power Verification (Stage 5, WS4):
//! - `power_expr`: UPF boolean/power-control expression parsing & SVA lowering.
//! - `pa_instrument`: power-aware Btor2 emitter, IEEE 1801 `SIMSTATE`
//!   (`CORRUPT`, `CORRUPT_ON_ACTIVITY`, `CORRUPT_STATE_ON_ACTIVITY`), and
//!   `lp.pd.<domain>.{on,iso,ret,corrupt}` VCD swimlanes.
//! - `iso_ret_models`: UPF isolation clamping (`0`/`1`/`latch`), retention shadow
//!   flops, and 2-copy / single-copy corruption modeling.
//! - `aag`: `LowPower` task AAG assertions, PST covers, and `lp_ret_value_preserved_*`.
//! - `corrupt_2copy`: exact 2-copy corruption proofs (`-prove_corruption`).
//! - `auto_waiver`: sound closed-loop certified auto-waivers (`-prove_waivers`).
//! - `lp_sec`: power-aware SEC (`-sec`) and LP-LEC (`-lec`).

pub mod aag;
pub mod auto_waiver;
pub mod btor_emit;
pub mod corrupt_2copy;
pub mod iso_ret_models;
pub mod lp_sec;
pub mod pa_instrument;
pub mod power_expr;

use crate::lp::finder_bridge::DesignFinder;
use crate::lp::{ensure_resolved_with_finder, static_rtl, CheckLpOpts, LpStage};
use crate::session::Session;

/// Entry point for all Formal Low-Power Verification modes (`-stage formal`,
/// `check_lpv`, `-prove_corruption`, `-prove_waivers`, `-sec`, `-lec`).
pub fn run_formal_lp(s: &mut Session, opts: &CheckLpOpts) -> Result<(), String> {
    if opts.sec {
        return lp_sec::run_lp_sec(s);
    }
    if opts.lec {
        return lp_sec::run_lp_lec(s);
    }

    if !s.elaborated {
        return Err("check_lp: elaborate design first".to_string());
    }

    // Decide whether Stage 2 static checks should run first:
    // - Always run Stage 2 if `-stage formal` or `-stage all` (unless `-prove_waivers`
    //   is invoked standalone on already-computed Stage 2 results, preserving manual waivers).
    let stage2_already_ran = s.lp.stages_run.iter().any(|st| st == "rtl");
    let standalone_waivers =
        opts.prove_waivers && !opts.prove_corruption && opts.stage != LpStage::Formal;
    if !standalone_waivers || !stage2_already_ran {
        static_rtl::run_stage2(s, opts)?;
    }

    let compiled = s
        .compiled
        .clone()
        .ok_or_else(|| "check_lp: elaborate design first (no compiled design)".to_string())?;
    let graph = compiled.struct_graph();
    let finder = DesignFinder::from_compiled(Some(&compiled), s.design.top.as_deref());
    let resolved = ensure_resolved_with_finder(s, &finder)?;
    s.lp.resolved = Some(resolved.clone());

    let run_aag_phase = opts.stage == LpStage::Formal
        || (opts.stage == LpStage::All && opts.prove_corruption);
    if run_aag_phase {
        aag::run_aag(s, graph, &resolved)?;
    }

    if opts.prove_corruption {
        corrupt_2copy::run_prove_corruption(s, graph, &resolved, !run_aag_phase)?;
    }

    if opts.prove_waivers {
        auto_waiver::run_prove_waivers(s, graph, &resolved)?;
    }

    if !s.lp.stages_run.iter().any(|st| st == "formal") {
        s.lp.stages_run.push("formal".to_string());
    }
    Ok(())
}
