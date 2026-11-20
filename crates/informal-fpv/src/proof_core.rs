//! Minimal proof-core & needed-assumption extraction
//! (`prove -with_proof_core`, `get_property_info -proof_core`).
//!
//! Given a `Proven` assertion (or `Unreachable` cover), identifies:
//! 1. Which named assumptions (`assume` / RTL `assume property`) are actually
//!    needed for the proof vs. redundant (`unused_assumptions`), via structural
//!    target-cone pruning followed by greedy assumption-deletion checks.
//! 2. The tight state and input support of `(target + needed_assumptions)`.

use crate::session::{drop_assumptions, only_target, Session};
use informal_engine::{portfolio, CheckRequest, ModelFormat, Verdict};
use informal_ir::Op;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

static PC_SEQ: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProofCore {
    pub states: Vec<String>,
    pub inputs: Vec<String>,
    pub needed_assumptions: Vec<String>,
    pub unused_assumptions: Vec<String>,
}

/// Computes the backward cone of `(roots)` in `m`, returning `(reached_state_syms, reached_input_syms, reached_node_ids)`.
fn cone_from_roots(
    m: &informal_ir::Model,
    roots: &[u32],
    state_deps: &HashMap<u32, Vec<u32>>,
) -> (Vec<String>, Vec<String>, HashSet<u32>) {
    let mut todo: Vec<u32> = roots.to_vec();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut states = Vec::new();
    let mut inputs = Vec::new();
    while let Some(id) = todo.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(l) = m.line(id) else { continue };
        match l.op {
            Op::State => {
                if let Some(sym) = &l.symbol {
                    if !sym.starts_with("informal_") && !states.contains(sym) {
                        states.push(sym.clone());
                    }
                }
                todo.extend(state_deps.get(&id).into_iter().flatten().copied());
            }
            Op::Input => {
                if let Some(sym) = &l.symbol {
                    if !sym.starts_with("informal_") && !inputs.contains(sym) {
                        inputs.push(sym.clone());
                    }
                }
            }
            _ => todo.extend(l.args.iter().map(|a| a.unsigned_abs() as u32)),
        }
    }
    states.sort();
    inputs.sort();
    (states, inputs, seen)
}

pub fn compute_proof_core(s: &Session, prop_idx: usize) -> Result<ProofCore, String> {
    let p = s
        .props
        .get(prop_idx)
        .ok_or_else(|| "invalid property index".to_string())?;
    let task = &p.task;
    let check_sym = s.check_name(prop_idx);
    let btor = s.task_model(task)?;
    let isolated = only_target(&btor, &check_sym);
    let m = informal_ir::parse(&isolated).map_err(|e| format!("proof_core parse: {e}"))?;

    let mut state_deps: HashMap<u32, Vec<u32>> = HashMap::new();
    for l in m.lines().filter(|l| matches!(l.op, Op::Next | Op::Init)) {
        if let [st, rest @ ..] = l.args.as_slice() {
            state_deps
                .entry(st.unsigned_abs() as u32)
                .or_default()
                .extend(rest.iter().map(|a| a.unsigned_abs() as u32));
        }
    }

    let mut target_roots: Vec<u32> = Vec::new();
    let mut user_constraints: Vec<(String, u32)> = Vec::new();
    let mut system_constraint_roots: Vec<u32> = Vec::new();

    for l in m.lines() {
        match l.op {
            Op::Bad | Op::Justice => {
                target_roots.extend(l.args.iter().map(|a| a.unsigned_abs() as u32));
            }
            Op::Constraint | Op::Fair => {
                let arg = l.args.first().map(|a| a.unsigned_abs() as u32).unwrap_or(0);
                match l.symbol.as_deref() {
                    Some(sym) if !sym.starts_with("informal_") => {
                        if !user_constraints.iter().any(|(n, _)| n == sym) {
                            user_constraints.push((sym.to_string(), arg));
                        }
                    }
                    _ => system_constraint_roots.push(arg),
                }
            }
            _ => {}
        }
    }

    // 1. Target + system (reset/clock) cone without user assumptions.
    let mut base_roots = target_roots.clone();
    base_roots.extend(&system_constraint_roots);
    let (_, _, target_cone_nodes) = cone_from_roots(&m, &base_roots, &state_deps);

    let mut unused_assumptions: Vec<String> = Vec::new();
    let mut candidates: Vec<(String, u32)> = Vec::new();
    for (sym, arg) in user_constraints {
        let (_, _, assump_nodes) = cone_from_roots(&m, &[arg], &state_deps);
        let intersects = assump_nodes.iter().any(|id| target_cone_nodes.contains(id));
        if !intersects {
            unused_assumptions.push(sym);
        } else {
            candidates.push((sym, arg));
        }
    }

    // 2. Greedy deletion check on intersecting user assumptions.
    let mut dropped: Vec<String> = unused_assumptions.clone();
    let mut needed_assumptions: Vec<String> = Vec::new();
    let mut needed_roots: Vec<u32> = Vec::new();

    if !candidates.is_empty() {
        let engines = informal_bmc::select_for_btor(
            &s.active_engines(),
            &isolated,
            !s.engine_filter.is_empty(),
        );
        let trial_timeout = s.time_limit.min(Duration::from_secs(3));
        let _ = std::fs::create_dir_all(&s.workdir);

        for (sym, arg) in candidates {
            let mut trial_drop = dropped.clone();
            trial_drop.push(sym.clone());
            let reduced_btor = drop_assumptions(&isolated, &trial_drop);
            let seq = PC_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = s
                .workdir
                .join(format!("pcore_{}_{seq}.btor2", std::process::id()));
            if std::fs::write(&path, &reduced_btor).is_ok() {
                let req = CheckRequest {
                    model: path.clone(),
                    format: ModelFormat::Btor2,
                    timeout: trial_timeout,
                };
                let run = portfolio::run_until(&engines, &req, None, Some(s.stop.as_ref()));
                let _ = std::fs::remove_file(&path);
                if matches!(portfolio::verdict(&run), Verdict::Safe) {
                    dropped.push(sym.clone());
                    unused_assumptions.push(sym);
                    continue;
                }
            }
            needed_assumptions.push(sym);
            needed_roots.push(arg);
        }
    }

    // 3. Tight cone of `(target + system constraints + needed assumptions)`.
    base_roots.extend(needed_roots);
    let (states, inputs, _) = cone_from_roots(&m, &base_roots, &state_deps);

    let top_prefix = s
        .design
        .top
        .as_deref()
        .map(|t| format!("{t}."))
        .unwrap_or_default();
    let clocks: HashSet<String> = s
        .clocks
        .iter()
        .flat_map(|c| [c.signal.clone(), format!("{top_prefix}{}", c.signal)])
        .collect();
    let states = states
        .into_iter()
        .filter(|st| {
            !st.contains("._sva_")
                && !st.contains("._past_")
                && !st.contains("._was_")
                && !st.contains("._clk_")
        })
        .collect();
    let inputs = inputs.into_iter().filter(|i| !clocks.contains(i)).collect();

    Ok(ProofCore {
        states,
        inputs,
        needed_assumptions,
        unused_assumptions,
    })
}
