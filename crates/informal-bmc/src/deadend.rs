//! Counterexample-guided 2QBF dead-end / trap-state detector
//! (`check_assumptions -dead_end`).
//!
//! Decides whether there exists a reachable state `s_k` (`0 <= k <= max_depth`)
//! after `k` legal steps satisfying all `constraint`s from which **every**
//! primary input assignment violates at least one `constraint`:
//!
//! `exists s_0, u_0, ..., u_{k-1}, s_k . Init(s_0) /\ /\_{t < k} (T /\ C) /\ forall u_k . !C(s_k, u_k)`
//!
//! Uses exact 2QBF counterexample-guided instantiation over two incremental SAT
//! solvers (`E` and `A`): whenever `A` finds a continuation input `u_hat` valid
//! from candidate `s_k`, `E` is strengthened with `!C(L_k, u_hat)`, pruning the
//! entire state subspace for which `u_hat` is a legal next input.

use informal_aig::{is_neg, var, Aig, Init, Lit, Node};
use informal_sat::{Answer, Sat};
use serde::Serialize;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeadEnd {
    /// Step `k` at which the environment becomes trapped (`0` = initial state).
    pub depth: u32,
    /// Minimal core of `(signal_name, value)` state bits that trap all next inputs.
    pub trap_state: Vec<(String, bool)>,
}

fn make_solver(deadline: Instant) -> Box<dyn Sat> {
    let mut s: Box<dyn Sat> = match informal_sat::Cadical::new() {
        Ok(c) => Box::new(c),
        Err(_) => Box::new(informal_sat::Mini::with_seed(0)),
    };
    s.set_terminate(Box::new(move || {
        Instant::now() >= deadline || informal_engine::memory::exceeded()
    }));
    s
}

#[inline]
fn apply_sign(sv: i32, aig_lit: Lit) -> i32 {
    if is_neg(aig_lit) {
        -sv
    } else {
        sv
    }
}

/// Encodes one full frame of `g` into `sat` given pre-assigned positive solver
/// variables for latches (`latch_vars`, indexed by latch index in `g.latches`).
/// Returns `(var_map, next_lit_of_each_latch, constraint_lits)`.
fn encode_frame(
    g: &Aig,
    sat: &mut dyn Sat,
    next_var: &mut i32,
    latch_vars: &[i32],
    fixed_inputs: Option<&[bool]>,
) -> (Vec<i32>, Vec<i32>, Vec<i32>) {
    let n = g.nodes.len();
    // Reserve enough variables up front for Const + Inputs + And gates.
    let needed = (n as i32) + 4;
    sat.reserve(*next_var + needed);

    let mut vmap = vec![0i32; n];
    let false_v = *next_var;
    *next_var += 1;
    sat.add_clause(&[-false_v]);
    vmap[0] = false_v;

    let mut latch_pos = std::collections::HashMap::with_capacity(g.latches.len());
    for (li, l) in g.latches.iter().enumerate() {
        latch_pos.insert(var(l.lit), latch_vars[li]);
    }
    let mut input_idx = std::collections::HashMap::with_capacity(g.inputs.len());
    for (ui, &inp) in g.inputs.iter().enumerate() {
        input_idx.insert(var(inp), ui);
    }

    for v in 1..n {
        match g.nodes[v] {
            Node::Const => vmap[v] = false_v,
            Node::Latch => {
                vmap[v] = latch_pos[&(v as u32)];
            }
            Node::Input => {
                let sv = *next_var;
                *next_var += 1;
                if let Some(vals) = fixed_inputs {
                    let b = vals[input_idx[&(v as u32)]];
                    sat.add_clause(&[if b { sv } else { -sv }]);
                }
                vmap[v] = sv;
            }
            Node::And(a, b) => {
                let sv = *next_var;
                *next_var += 1;
                let la = apply_sign(vmap[var(a) as usize], a);
                let lb = apply_sign(vmap[var(b) as usize], b);
                sat.add_clause(&[-sv, la]);
                sat.add_clause(&[-sv, lb]);
                sat.add_clause(&[sv, -la, -lb]);
                vmap[v] = sv;
            }
        }
    }

    let nexts: Vec<i32> = g
        .latches
        .iter()
        .map(|l| apply_sign(vmap[var(l.next) as usize], l.next))
        .collect();
    let constrs: Vec<i32> = g
        .constraints
        .iter()
        .map(|&c| apply_sign(vmap[var(c) as usize], c))
        .collect();
    (vmap, nexts, constrs)
}

/// Checks `btor_text` for reachable dead-end states up to `max_depth` steps.
pub fn find_dead_end(
    btor_text: &str,
    max_depth: u32,
    timeout: Duration,
) -> Result<Option<DeadEnd>, String> {
    let m = informal_ir::parse(btor_text).map_err(|e| format!("dead_end parse: {e}"))?;
    let g = informal_aig::btor::from_btor2(&m).map_err(|e| format!("dead_end bit-blast: {e:#}"))?;
    if g.constraints.is_empty() {
        return Ok(None);
    }

    let deadline = Instant::now() + timeout;

    // Universal single-step checker `A`: checks whether SOME input satisfies `g.constraints`
    // from a candidate latch assignment.
    let mut a_sat = make_solver(deadline);
    let mut a_next_var: i32 = 1;
    let nl = g.latches.len();
    a_sat.reserve((nl as i32) + 2);
    let a_latch_vars: Vec<i32> = (0..nl)
        .map(|_| {
            let v = a_next_var;
            a_next_var += 1;
            v
        })
        .collect();
    let (a_vmap, _, a_constrs) =
        encode_frame(&g, a_sat.as_mut(), &mut a_next_var, &a_latch_vars, None);
    for &c in &a_constrs {
        a_sat.add_clause(&[c]);
    }
    let a_input_vars: Vec<i32> = g
        .inputs
        .iter()
        .map(|&inp| a_vmap[var(inp) as usize])
        .collect();

    // Existential unrolling solver `E`, built incrementally across depths `0..=max_depth`.
    let mut e_sat = make_solver(deadline);
    let mut e_next_var: i32 = 1;
    e_sat.reserve((nl as i32) + 2);
    let mut cur_latches: Vec<i32> = (0..nl)
        .map(|_| {
            let v = e_next_var;
            e_next_var += 1;
            v
        })
        .collect();

    // Constrain frame-0 initial state in `E`.
    // (For `Init::Expr`, encode a frame-0 init helper if needed.)
    let mut has_expr_init = false;
    for (li, l) in g.latches.iter().enumerate() {
        match l.init {
            Init::Zero => e_sat.add_clause(&[-cur_latches[li]]),
            Init::One => e_sat.add_clause(&[cur_latches[li]]),
            Init::Free => {}
            Init::Expr(_) => has_expr_init = true,
        }
    }
    if has_expr_init {
        let (init_vmap, _, _) =
            encode_frame(&g, e_sat.as_mut(), &mut e_next_var, &cur_latches, None);
        for (li, l) in g.latches.iter().enumerate() {
            if let Init::Expr(e) = l.init {
                let le = apply_sign(init_vmap[var(e) as usize], e);
                let lv = cur_latches[li];
                e_sat.add_clause(&[-lv, le]);
                e_sat.add_clause(&[lv, -le]);
            }
        }
    }

    for k in 0..=max_depth {
        if Instant::now() >= deadline {
            return Err("timeout".into());
        }
        // Guard activation literal `act_k` so instantiated `!C(L_k, u_hat)` clauses
        // only constrain step `k`'s dead-end query and do not restrict step `k+1`!
        let act_k = e_next_var;
        e_next_var += 1;
        e_sat.reserve(e_next_var + 2);

        loop {
            match e_sat.solve(&[act_k]) {
                Answer::Unsat => break,
                Answer::Unknown => return Err("timeout".into()),
                Answer::Sat => {
                    let cand_bits: Vec<bool> =
                        cur_latches.iter().map(|&lv| e_sat.value(lv)).collect();
                    let a_assump: Vec<i32> = a_latch_vars
                        .iter()
                        .zip(&cand_bits)
                        .map(|(&av, &b)| if b { av } else { -av })
                        .collect();
                    match a_sat.solve(&a_assump) {
                        Answer::Unsat => {
                            let mut trap_state = Vec::new();
                            for (li, (&av, &b)) in a_assump.iter().zip(&cand_bits).enumerate() {
                                if a_sat.failed(av) {
                                    let lvar = var(g.latches[li].lit);
                                    let name = g
                                        .names
                                        .get(&lvar)
                                        .cloned()
                                        .unwrap_or_else(|| format!("latch_{li}"));
                                    trap_state.push((name, b));
                                }
                            }
                            return Ok(Some(DeadEnd {
                                depth: k,
                                trap_state,
                            }));
                        }
                        Answer::Unknown => return Err("timeout".into()),
                        Answer::Sat => {
                            let u_hat: Vec<bool> =
                                a_input_vars.iter().map(|&uv| a_sat.value(uv)).collect();
                            let (_, _, inst_constrs) = encode_frame(
                                &g,
                                e_sat.as_mut(),
                                &mut e_next_var,
                                &cur_latches,
                                Some(&u_hat),
                            );
                            // Under `act_k`, at least one constraint in `inst_constrs` MUST fail!
                            let mut blocking = Vec::with_capacity(inst_constrs.len() + 1);
                            blocking.push(-act_k);
                            for c in inst_constrs {
                                blocking.push(-c);
                            }
                            e_sat.add_clause(&blocking);
                        }
                    }
                }
            }
        }
        // Permanently retire `act_k` before advancing to `k + 1`.
        e_sat.add_clause(&[-act_k]);

        if k < max_depth {
            let (_, nexts, step_constrs) =
                encode_frame(&g, e_sat.as_mut(), &mut e_next_var, &cur_latches, None);
            for c in step_constrs {
                e_sat.add_clause(&[c]);
            }
            cur_latches = nexts;
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_reachable_dead_end_and_passes_live_environment() {
        // 2-bit counter from 0..3; assumption requires `!(cnt == 2 && inp == 0) && !(cnt == 2 && inp == 1)`
        // -> dead-end reached at depth 2 when `cnt == 2`!
        let trapped = "\
1 sort bitvec 2
2 sort bitvec 1
3 zero 1
4 state 1 cnt
5 init 1 4 3
6 input 2 inp
7 constd 1 1
8 add 1 4 7
9 next 1 4 8
10 constd 1 2
11 eq 2 4 10
12 not 2 11
13 and 2 11 6
14 not 2 13
15 not 2 6
16 and 2 11 15
17 not 2 16
18 constraint 14
19 constraint 17
";
        let res = find_dead_end(trapped, 5, Duration::from_secs(10)).unwrap();
        let de = res.expect("should detect dead-end at cnt == 2");
        assert_eq!(de.depth, 2);

        // Live environment: assumption allows `inp == 1` even when `cnt == 2` (only `14` constrained).
        let live = "\
1 sort bitvec 2
2 sort bitvec 1
3 zero 1
4 state 1 cnt
5 init 1 4 3
6 input 2 inp
7 constd 1 1
8 add 1 4 7
9 next 1 4 8
10 constd 1 2
11 eq 2 4 10
12 and 2 11 6
13 not 2 12
14 constraint 13
";
        let ok = find_dead_end(live, 6, Duration::from_secs(10)).unwrap();
        assert_eq!(ok, None);
    }
}
