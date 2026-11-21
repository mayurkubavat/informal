//! Formal Security Path / Taint Non-Interference App (`check_spv`).
//!
//! Proves true multi-cycle non-interference via 2-safety self-composition
//! (`crate::miter`): Copy 0 and Copy 1 start from identical non-secret states
//! and share all non-secret inputs, differing ONLY on `-from <secret_sigs>`
//! while both copies satisfy `reset` and all active `assume` constraints.
//! `proven` certifies that `-from` can NEVER influence `-to` across any number
//! of clock cycles (including covert storage channels through registers/FIFOs);
//! `cex` witnesses an exact information-leakage path.

use crate::miter::{build_miter_btor2, parse_full_model, resolve_signal, MiterSpec};
use crate::session::Session;
use informal_engine::{portfolio, CheckRequest, ModelFormat, Verdict};
use informal_ir::Op;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

static SPV_SEQ: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SpvStatus {
    Unprocessed,
    Proven,
    Cex { depth: Option<u32> },
    Undetermined,
    Error(String),
}

impl SpvStatus {
    pub fn word(&self) -> &'static str {
        match self {
            Self::Unprocessed => "unprocessed",
            Self::Proven => "proven",
            Self::Cex { .. } => "cex",
            Self::Undetermined => "undetermined",
            Self::Error(_) => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SpvRule {
    pub name: String,
    pub task: String,
    pub from: Vec<String>,
    pub to: Vec<String>,
    /// Optional isolation condition (`-cond`): non-interference is checked
    /// whenever `cond` holds in both executions (e.g. `!key_export_en` or
    /// `priv_mode == USER`).
    pub cond: Option<String>,
    pub status: SpvStatus,
    pub engine: Option<String>,
    pub ms: u128,
}

pub fn prove_spv(
    s: &mut Session,
    only_name: Option<&str>,
    timeout_override: Option<Duration>,
) -> Result<(), String> {
    if !s.elaborated {
        s.elaborate(None).map_err(|e| format!("check_spv: {e:#}"))?;
    }
    let indices: Vec<usize> = s
        .spv
        .iter()
        .enumerate()
        .filter(|(_, r)| only_name.is_none_or(|n| r.name == n))
        .map(|(i, _)| i)
        .collect();
    if let Some(n) = only_name {
        if indices.is_empty() {
            return Err(format!("check_spv: no security rule named `{n}`"));
        }
    }
    let timeout = timeout_override.unwrap_or(s.time_limit);
    let skip_steps = if s.reset.is_some() {
        s.reset_step_count()
    } else {
        0
    };
    let top = s.design.top.clone();
    std::fs::create_dir_all(&s.workdir).map_err(|e| format!("check_spv: {e}"))?;

    for idx in indices {
        let rule = s.spv[idx].clone();
        // If `-cond` is an arbitrary SVA boolean expression (not just a single signal),
        // bind it temporarily as a cover check in `lower` or resolve directly as a signal!
        let guard_helper_name = format!("__spv_cond_{idx}");
        let temp_added = if let Some(cond_expr) = &rule.cond {
            // Adding a temporary cover property compiles arbitrary boolean expressions
            // (`!export_en && mode == 2'd0`) directly into a named `bad` line in Btor2!
            match s.add_property_in(
                &rule.task,
                crate::session::Kind::Cover,
                Some(guard_helper_name.clone()),
                cond_expr,
            ) {
                Ok(full) => Some(full),
                Err(e) => {
                    s.spv[idx].status = SpvStatus::Error(e);
                    continue;
                }
            }
        } else {
            None
        };

        let lowered = s.lower_for_coverage(&rule.task, false, false);
        if let Some(ref temp_full) = temp_added {
            s.props.retain(|p| &p.name != temp_full);
        }
        let (full_btor, _) = match lowered {
            Ok(v) => v,
            Err(e) => {
                s.spv[idx].status = SpvStatus::Error(e);
                continue;
            }
        };

        let m = match parse_full_model(&full_btor) {
            Ok(m) => m,
            Err(e) => {
                s.spv[idx].status = SpvStatus::Error(e);
                continue;
            }
        };

        let mut diverging_inputs: HashSet<u32> = HashSet::new();
        let mut diverging_nodes: HashMap<u32, String> = HashMap::new();
        let mut resolve_err = None;
        for src in &rule.from {
            match resolve_signal(&m, top.as_deref(), src) {
                Ok(r) => match r.direct_line {
                    Some((id, Op::Input)) => {
                        diverging_inputs.insert(id);
                    }
                    _ => {
                        diverging_nodes.insert(r.node_ref.unsigned_abs() as u32, r.name);
                    }
                },
                Err(e) => {
                    resolve_err = Some(e);
                    break;
                }
            }
        }
        let mut sinks = Vec::new();
        if resolve_err.is_none() {
            for dst in &rule.to {
                match resolve_signal(&m, top.as_deref(), dst) {
                    Ok(r) => sinks.push(r),
                    Err(e) => {
                        resolve_err = Some(e);
                        break;
                    }
                }
            }
        }
        if let Some(e) = resolve_err {
            s.spv[idx].status = SpvStatus::Error(e);
            continue;
        }

        // Locate the compiled guard `bad` node if `-cond` was provided!
        let guard_ref = if let Some(ref temp_full) = temp_added {
            let sym = crate::session::target_name(temp_full);
            m.lines()
                .find(|l| l.op == Op::Bad && l.symbol.as_deref() == Some(sym.as_str()))
                .and_then(|l| l.args.first().copied())
        } else {
            None
        };

        let engines = informal_bmc::select_for_btor(
            &s.active_engines(),
            informal_svlang::split_trace(&full_btor).0,
            false,
        );

        let mut worst_status = SpvStatus::Proven;
        let mut winner_eng: Option<String> = None;
        let mut total_ms = 0u128;

        for sink in sinks {
            let spec = MiterSpec {
                diverging_inputs: diverging_inputs.clone(),
                diverging_nodes: diverging_nodes.clone(),
                uninit_states_diverge: false,
                tied_uninit_states: HashSet::new(),
                skip_steps,
                guard_ref,
                target: (
                    sink.node_ref,
                    sink.width,
                    format!("spv_{}_{}", rule.name, sink.name),
                ),
            };
            let miter_btor = match build_miter_btor2(&m, &spec) {
                Ok(b) => b,
                Err(e) => {
                    worst_status = SpvStatus::Error(e);
                    break;
                }
            };
            let seq = SPV_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = s
                .workdir
                .join(format!("spv_{}_{seq}.btor2", std::process::id()));
            std::fs::write(&path, &miter_btor).map_err(|e| format!("check_spv: {e}"))?;
            let req = CheckRequest {
                model: path.clone(),
                format: ModelFormat::Btor2,
                timeout,
                effort: s.effort,
                max_depth: s.max_trace_length,
            };
            let run = portfolio::run_until(&engines, &req, None, Some(s.stop.as_ref()));
            let _ = std::fs::remove_file(&path);
            total_ms += run.wall_ms;
            if let Some(w) = &run.winner {
                winner_eng = Some(w.engine.clone());
            }
            match portfolio::verdict(&run) {
                Verdict::Safe => {}
                Verdict::Unsafe { depth } => {
                    worst_status = SpvStatus::Cex { depth };
                    break;
                }
                Verdict::Unknown { .. } => {
                    worst_status = SpvStatus::Undetermined;
                }
            }
        }

        s.spv[idx].status = worst_status;
        s.spv[idx].engine = winner_eng;
        s.spv[idx].ms = total_ms;
    }
    Ok(())
}

pub fn format_report(rules: &[SpvRule]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<22} {:<18} {:<18} {:<12} {:<14} detail",
        "name", "from (secret)", "to (observable)", "status", "engine"
    );
    let _ = writeln!(out, "{}", "-".repeat(96));
    for r in rules {
        let detail = match &r.status {
            SpvStatus::Proven => format!("{:.2} s (isolated)", r.ms as f64 / 1000.0),
            SpvStatus::Cex { depth } => format!(
                "leak at depth {} in {:.2} s",
                depth.map_or("?".into(), |d| d.to_string()),
                r.ms as f64 / 1000.0
            ),
            SpvStatus::Error(e) => e.clone(),
            _ => String::new(),
        };
        let _ = writeln!(
            out,
            "{:<22} {:<18} {:<18} {:<12} {:<14} {}",
            r.name,
            r.from.join(","),
            r.to.join(","),
            r.status.word(),
            r.engine.as_deref().unwrap_or("-"),
            detail
        );
    }
    out
}
