//! Stage 3 (Post-Synthesis Gate Netlist + Liberty) & Stage 4 (Physical PG Netlist)
//! static low-power verification entry points.

pub mod cells_aon;
pub mod ls_ret;
pub mod macro_pins;
pub mod pg_ctx;
pub mod pg_nets;

use std::time::Instant;

use informal_liberty::LibertyDb;
use informal_svlang::struct_graph::StructuralGraph;
use informal_upf::PstSat;

use crate::lp::finder_bridge::DesignFinder;
use crate::lp::{ensure_resolved_with_finder, static_upf, CheckLpOpts, LpBenchStats, LpViolation};
use crate::session::Session;

/// Returns `true` if the elaborated structural graph instantiates at least one
/// cell present in `liberty`.
pub fn has_liberty_instances(graph: &StructuralGraph, liberty: &LibertyDb) -> bool {
    graph
        .cells
        .iter()
        .any(|c| !c.is_top && liberty.contains_cell(&c.module))
}

/// Runs Stage 3 (`-stage netlist`) checks over the elaborated gate netlist + Liberty DB.
pub fn run_stage3_netlist(s: &mut Session, opts: &CheckLpOpts) -> Result<(), String> {
    run_gate_pg_stages(s, opts, false, true, false)
}

/// Runs Stage 4 (`-stage pg`) checks over the elaborated PG netlist + Liberty DB.
pub fn run_stage4_pg(s: &mut Session, opts: &CheckLpOpts) -> Result<(), String> {
    run_gate_pg_stages(s, opts, false, false, true)
}

/// Runs Stage 1 + Stage 3 (+ Stage 4 if PG pins are present) for `-stage design`
/// or `-stage all` when a gate-level netlist and Liberty DB are loaded.
pub fn run_design_gate_pg(s: &mut Session, opts: &CheckLpOpts) -> Result<(), String> {
    run_gate_pg_stages(s, opts, true, true, true)
}

fn run_gate_pg_stages(
    s: &mut Session,
    opts: &CheckLpOpts,
    include_stage1: bool,
    run_netlist: bool,
    run_pg: bool,
) -> Result<(), String> {
    if !s.elaborated {
        return Err("check_lp: elaborate design first".to_string());
    }
    let liberty = s.lp.liberty.clone().ok_or_else(|| {
        "check_lp: no Liberty library loaded; run `read_liberty <file.lib>` first".to_string()
    })?;
    if s.struct_graph().is_none() {
        return Err("check_lp: no structural graph available; elaborate design first".to_string());
    }

    let t0 = Instant::now();
    let finder = DesignFinder::from_session(s);
    let resolved = ensure_resolved_with_finder(s, &finder)?;
    let t_sat = Instant::now();
    let mut sat = PstSat::build_with_width_provider(&resolved, opts.tol_uv, &finder);
    let pst_sat_build_ms = t_sat.elapsed().as_secs_f64() * 1000.0;
    let (cnf_vars, cnf_clauses) = sat.cnf_stats();

    let mut violations: Vec<LpViolation> = Vec::new();
    let stage1_ms = if include_stage1 {
        let t1 = Instant::now();
        violations.extend(static_upf::run_stage1(&resolved, &mut sat));
        t1.elapsed().as_secs_f64() * 1000.0
    } else {
        0.0
    };

    let graph = s
        .struct_graph()
        .ok_or_else(|| "check_lp: structural graph unavailable".to_string())?;
    let mut ctx = pg_ctx::GatePgCtx::new(graph, &resolved, &liberty, &mut sat);

    if run_netlist {
        cells_aon::run_stage3_cell_checks(&mut ctx, &mut sat, opts.tol_uv, &mut violations);
        macro_pins::check_macro_pin_crossings(&mut ctx, &mut sat, opts.tol_uv, &mut violations);
    }
    if run_pg && (!include_stage1 || ctx.any_pg_connected) {
        pg_nets::run_stage4_checks(&mut ctx, &mut sat, &mut violations);
    }

    for (idx, v) in violations.iter_mut().enumerate() {
        v.id = idx + 1;
    }

    let total_ms = t0.elapsed().as_secs_f64() * 1000.0;
    s.lp.pst_consistent = Some(sat.is_state_space_consistent());
    s.lp.logic_constraints = sat.export_logic_constraints();
    s.lp.resolved = Some(resolved);
    s.lp.violations = violations;

    let mut stages = Vec::new();
    if include_stage1 {
        stages.push("upf".to_string());
    }
    if run_netlist {
        stages.push("netlist".to_string());
    }
    if run_pg {
        stages.push("pg".to_string());
    }
    s.lp.stages_run = stages;
    s.lp.last_stats = Some(LpBenchStats {
        struct_graph_build_ms: s.fast_gate.as_ref().map_or(0.0, |fg| fg.build_ms),
        pst_sat_build_ms,
        stage1_ms,
        stage2_ms: total_ms,
        cnf_vars,
        cnf_clauses,
        ..LpBenchStats::default()
    });
    s.lp.reapply_waivers();
    Ok(())
}
