//! Sound closed-loop certified auto-waiver engine (`check_lp -prove_waivers`).
//!
//! For each `Open` static crossing violation `v` (skipping manual `Waived`
//! items), builds an exact 2-copy miter observing every top-level output in
//! `v`'s fanout cone while simultaneously injecting `CondDivergence` on ALL
//! open/corruptible crossings (`pa.cond_divergences`). Simultaneous corruption
//! prevents unsound circular mutual-masking waivers (e.g., two unisolated
//! switchable domains masking each other), while fanout-cone observation keeps
//! independent masked crossings decoupled from unrelated leaks.
//!
//! Outcomes per violation:
//! - All fanout outputs proven `Verdict::Safe` -> `LpStatus::FormallyWaived`
//!   with full [`LpAutoFormalMeta`] (engine, time, assumptions, stopats, reset,
//!   task); automatically invalidated back to `Open` on any assumption/stopat/
//!   reset/design mutation.
//! - Any fanout output falsified -> `LpStatus::FormalCex` with VCD trace and
//!   registered `LowPower::lp_waive_*` property for `why_cex` / `lp_diagnose`.
//! - Solver `Unknown` -> remains `LpStatus::Open` (sound over-approximation).

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use informal_engine::{portfolio, CheckRequest, ModelFormat, Verdict};
use informal_svlang::struct_graph::StructuralGraph;
use informal_upf::ResolvedPowerIntent;

use super::aag::{register_and_prove_miter_prop, LOW_POWER_TASK};
use super::corrupt_2copy::{backward_cone_nodes, collect_top_output_targets};
use super::iso_ret_models::{build_pa_model, strip_top_prefix, CrossingBoundaryInfo, PaMode};
use super::pa_instrument::sanitize_sym;
use crate::lp::{LpAutoFormalMeta, LpStatus, LpViolation};
use crate::miter::{build_miter_btor2, parse_full_model, MiterSpec};
use crate::session::{Kind, Session, Status};

fn is_waivable_crossing_rule(rule_id: &str) -> bool {
    matches!(
        rule_id,
        "ISO_MISSING"
            | "ISO_CTRL_NOT_AON"
            | "ISO_CTRL_UNDRIVEN"
            | "ISO_CTRL_POLARITY_MISMATCH"
            | "ISO_SUPPLY_NOT_AON"
            | "ISO_FEEDTHROUGH_UNSAFE"
            | "ISO_CLAMP_MISMATCH"
            | "ISO_LOCATION_INVALID"
    )
}

/// Runs certified 2-copy auto-waiver analysis over all currently `Open`
/// crossing violations in `s.lp.violations`.
pub fn run_prove_waivers(
    s: &mut Session,
    graph: &StructuralGraph,
    resolved: &ResolvedPowerIntent,
) -> Result<(), String> {
    let open_indices: Vec<usize> = s
        .lp
        .violations
        .iter()
        .enumerate()
        .filter(|(_, v)| v.status == LpStatus::Open && is_waivable_crossing_rule(&v.rule_id))
        .map(|(i, _)| i)
        .collect();
    if open_indices.is_empty() {
        return Ok(());
    }

    let task_name = s.current_task.clone();
    let (base_btor, _) = s.lower_for_coverage(&task_name, false, false)?;
    let top = s.design.top.clone();
    let top_ref = top.as_deref();
    let skip_steps = if s.reset.is_some() {
        s.reset_step_count()
    } else {
        0
    };
    let pa = build_pa_model(
        &base_btor,
        graph,
        resolved,
        top_ref,
        PaMode::TwoCopy,
        skip_steps,
    )?;
    let pa_model = parse_full_model(&pa.btor2)?;

    let top_outputs = collect_top_output_targets(s, &pa_model, top_ref);
    let output_cones: Vec<(i64, u32, String, HashSet<u32>)> = top_outputs
        .into_iter()
        .map(|(r, w, name)| {
            let cone = backward_cone_nodes(&pa_model, r);
            (r, w, name, cone)
        })
        .collect();

    let assumptions: Vec<String> = s
        .props
        .iter()
        .filter(|p| p.task == task_name && p.kind == Kind::Assume && p.enabled)
        .map(|p| p.name.clone())
        .collect();
    let stopats = s.stopats_of(&task_name).to_vec();
    let reset = s.reset.clone();
    let active_engines = s.active_engines();
    let explicit_filter = !s.engine_filter.is_empty();
    std::fs::create_dir_all(&s.workdir).map_err(|e| format!("check_lp -prove_waivers: {e}"))?;

    for vi in open_indices {
        let v_snap = s.lp.violations[vi].clone();
        let matched = match_crossings_for_violation(&pa.crossings, graph, &v_snap, top_ref);
        let target_div_nodes: HashSet<u32> = matched
            .iter()
            .filter_map(|c| c.divergence_node)
            .collect();

        if target_div_nodes.is_empty() {
            continue;
        }

        // Observe ONLY outputs whose cone depends on this violation's crossing(s),
        // while keeping ALL `pa.cond_divergences` active so mutual-masking across
        // unisolated crossings cannot create an unsound waiver.
        let obs_outputs: Vec<(i64, u32, String)> = output_cones
            .iter()
            .filter(|(_, _, _, cone)| target_div_nodes.iter().any(|dn| cone.contains(dn)))
            .map(|(r, w, name, _)| (*r, *w, name.clone()))
            .collect();

        if obs_outputs.is_empty() {
            let v = &mut s.lp.violations[vi];
            v.status = LpStatus::FormallyWaived;
            v.auto_formal_meta = Some(LpAutoFormalMeta {
                engine: "structural_cone".to_string(),
                ms: 0,
                assumptions: assumptions.clone(),
                stopats: stopats.clone(),
                reset: reset.clone(),
                task: task_name.clone(),
            });
            continue;
        }

        let t0 = Instant::now();
        let mut all_safe = true;
        let mut last_engine = String::from("portfolio");
        let mut cex_found: Option<(String, Option<String>)> = None;

        for (obs_ref, obs_w, out_name) in obs_outputs {
            let short = format!(
                "lp_waive_{}_{}",
                sanitize_sym(&v_snap.signal),
                sanitize_sym(strip_top_prefix(&out_name, top_ref))
            );
            let full_name = format!("{LOW_POWER_TASK}::{short}");
            let spec = MiterSpec {
                diverging_inputs: HashSet::new(),
                diverging_nodes: HashMap::new(),
                conditional_diverging_nodes: pa.cond_divergences.clone(),
                conditional_state_corruptions: Vec::new(),
                uninit_states_diverge: false,
                tied_uninit_states: HashSet::new(),
                skip_steps,
                guard_ref: None,
                target: (obs_ref, obs_w, short.clone()),
                emit_trace_outputs: true,
            };
            let full_miter = build_miter_btor2(&pa_model, &spec)?;
            let (engine_btor, _) = informal_svlang::split_trace(&full_miter);
            let tmp_path = s.workdir.join(format!("{short}.tmp.btor2"));
            std::fs::write(&tmp_path, engine_btor)
                .map_err(|e| format!("check_lp -prove_waivers: {e}"))?;

            let chosen =
                informal_bmc::select_for_btor(&active_engines, engine_btor, explicit_filter);
            let req = CheckRequest {
                model: tmp_path.clone(),
                format: ModelFormat::Btor2,
                timeout: s.time_limit,
                effort: s.effort,
                max_depth: s.max_trace_length,
            };
            let run = portfolio::run_until(&chosen, &req, None, Some(s.stop.as_ref()));
            let _ = std::fs::remove_file(&tmp_path);
            if let Some(ref w) = run.winner {
                last_engine = w.engine.clone();
            }

            match portfolio::verdict(&run) {
                Verdict::Safe => {}
                Verdict::Unsafe { .. } => {
                    all_safe = false;
                    let prop_idx = register_and_prove_miter_prop(
                        s,
                        &full_name,
                        format!(
                            "certified auto-waiver check for {} on `{}` -> `{out_name}`",
                            v_snap.rule_id, v_snap.signal
                        ),
                        &full_miter,
                        &active_engines,
                        explicit_filter,
                    )?;
                    if matches!(s.props[prop_idx].status, Status::Cex { .. }) {
                        cex_found = Some((full_name, s.props[prop_idx].trace.clone()));
                    }
                    break;
                }
                Verdict::Unknown { .. } => {
                    all_safe = false;
                    break;
                }
            }
        }

        let elapsed_ms = t0.elapsed().as_millis() as u64;
        if let Some((prop_name, trace_path)) = cex_found {
            let v = &mut s.lp.violations[vi];
            v.status = LpStatus::FormalCex;
            v.formal_prop = Some(prop_name);
            v.trace = trace_path;
            v.auto_formal_meta = None;
        } else if all_safe {
            let v = &mut s.lp.violations[vi];
            v.status = LpStatus::FormallyWaived;
            v.auto_formal_meta = Some(LpAutoFormalMeta {
                engine: last_engine,
                ms: elapsed_ms,
                assumptions: assumptions.clone(),
                stopats: stopats.clone(),
                reset: reset.clone(),
                task: task_name.clone(),
            });
        }
    }

    Ok(())
}

fn match_crossings_for_violation<'a>(
    crossings: &'a [CrossingBoundaryInfo],
    graph: &StructuralGraph,
    v: &LpViolation,
    top: Option<&str>,
) -> Vec<&'a CrossingBoundaryInfo> {
    let vsig_norm = normalize_sig_for_match(&v.signal, top);
    let mut out = Vec::new();
    for c in crossings {
        if let Some(ref sd) = v.domain_src {
            if &c.src_domain_name != sd {
                continue;
            }
        }
        if let Some(ref dd) = v.domain_dst {
            if &c.dst_domain_name != dd {
                continue;
            }
        }
        let c_sig = normalize_sig_for_match(&c.signal, top);
        let src_full = normalize_sig_for_match(&graph.net(c.src_net).full_path, top);
        let dst_full = normalize_sig_for_match(&graph.net(c.dst_net).full_path, top);
        let src_upf = normalize_sig_for_match(&graph.net(c.src_net).upf_path, top);
        let dst_upf = normalize_sig_for_match(&graph.net(c.dst_net).upf_path, top);
        if c_sig == vsig_norm
            || src_full == vsig_norm
            || dst_full == vsig_norm
            || src_upf == vsig_norm
            || dst_upf == vsig_norm
            || vsig_norm.ends_with(&format!(".{c_sig}"))
            || c_sig.ends_with(&format!(".{vsig_norm}"))
        {
            out.push(c);
        }
    }
    if out.is_empty() {
        // Fallback: match by domain pair for unisolated crossings if signal carries slice notation.
        let base_vsig = vsig_norm.split('[').next().unwrap_or(&vsig_norm);
        for c in crossings {
            if v.domain_src.as_deref() == Some(c.src_domain_name.as_str())
                && v.domain_dst.as_deref() == Some(c.dst_domain_name.as_str())
                && (normalize_sig_for_match(&c.signal, top).contains(base_vsig)
                    || normalize_sig_for_match(&graph.net(c.dst_net).full_path, top)
                        .contains(base_vsig))
            {
                out.push(c);
            }
        }
    }
    out
}

fn normalize_sig_for_match(raw: &str, top: Option<&str>) -> String {
    let trimmed = raw
        .trim()
        .split(' ')
        .next()
        .unwrap_or("")
        .split('[')
        .next()
        .unwrap_or("")
        .trim_start_matches('/')
        .replace('/', ".");
    strip_top_prefix(&trimmed, top).to_string()
}
