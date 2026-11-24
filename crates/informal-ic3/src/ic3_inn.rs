//! IC3 with Internal-Node Generalization and DynAMic/EXCTG (`informal-ic3-inn`).
//!
//! Promotes up to `MAX_INN_SIGNALS` high-fanout latch-only internal AND nodes
//! `n(s)` into first-class shadow latches `s_aux` with `init(s_aux) = n(init)`
//! and `next(s_aux) = n(next(s))`, enabling IC3 (configured with DynAMic +
//! EXCTG + derived predicates + coupled pushes + Houdini) to learn 1-literal
//! internal-signal invariants directly. Any resulting invariant mentioning
//! shadow latches is projected back onto concrete `g.latches` for independent
//! certification via [`informal_bmc::certify_lemmas`].

use informal_aig::{is_neg, neg, var, Aig, Init, Lit, Node, FALSE, TRUE};
use informal_bmc::{ic3, seed_from_hub, solver, Prepared};
use informal_engine::{RunContext, Verdict};
use informal_sat::{Answer, Sat};
use std::collections::HashMap;

const MAX_INN_SIGNALS: usize = 16;
const MAX_PROJECT_CUBES: usize = 4_096;

/// Outcome of `IC3-INN` on a prepared AIG.
pub enum InnOutcome {
    Safe(Vec<ic3::Cube>),
    Unsafe { depth: u32 },
    Aborted(String),
}

/// Run IC3-INN (internal-signal promotion + DynAMic/EXCTG/coupled IC3) on `g`.
pub fn run_ic3_inn(g: &Prepared, ctx: &RunContext) -> InnOutcome {
    if g.latches.iter().any(|l| matches!(l.init, Init::Expr(_))) {
        return InnOutcome::Aborted("latch inits that are not constants".into());
    }

    let (hub, from_hub) = seed_from_hub(g, ctx, "informal-ic3-inn");
    let (inn_g, aux_defs) = build_inn_aig(g);
    let c = ctx.clone();
    let cfg = ic3::Config {
        dynamic_order: true,
        exctg: true,
        derive: true,
        coupled_push: true,
        cti_mine: true,
        houdini_every: 4,
        ..ic3::Config::default()
    };
    let Some(mut ic3) = ic3::Ic3::with_config(&inn_g, Box::new(move || solver(&c)), cfg) else {
        return InnOutcome::Aborted("ic3-inn init failed".into());
    };
    if !from_hub.is_empty() {
        ic3.assume_invariant(from_hub);
    }
    if let Some(h) = hub {
        ic3.attach_hub(h);
    }
    match ic3.check(ctx) {
        Verdict::Safe => {
            let Some(inn_lemmas) = ic3.invariant() else {
                return InnOutcome::Aborted("missing ic3-inn invariant".into());
            };
            match project_inn_lemmas(g, &inn_g, &aux_defs, &inn_lemmas, ctx) {
                Ok(lemmas) => InnOutcome::Safe(lemmas),
                Err(e) => InnOutcome::Aborted(e),
            }
        }
        Verdict::Unsafe { depth: Some(d) } => InnOutcome::Unsafe { depth: d },
        _ => InnOutcome::Aborted(ctx.stop_reason().into()),
    }
}

/// Select up to `MAX_INN_SIGNALS` high-fanout latch-only internal AND nodes
/// with constant initial values and promote them to shadow latches in `inn_g`.
/// Returns `(inn_g, aux_defs)` where `aux_defs[j] = (aux_latch_lit_in_inn, orig_and_lit_in_g)`.
fn build_inn_aig(g: &Aig) -> (Aig, Vec<(Lit, Lit)>) {
    let n = g.nodes.len();
    let mut latch_idx = vec![None; n];
    for (li, l) in g.latches.iter().enumerate() {
        latch_idx[var(l.lit) as usize] = Some(li);
    }

    // 1. Identify nodes depending purely on constant-initialized latches, and evaluate init.
    let mut pure_state = vec![false; n];
    let mut init_val = vec![false; n];
    let mut fanout = vec![0u32; n];

    for (v, node) in g.nodes.iter().enumerate() {
        match *node {
            Node::Const => {
                pure_state[v] = true;
                init_val[v] = false;
            }
            Node::Input => {
                pure_state[v] = false;
            }
            Node::Latch => {
                let li = latch_idx[v].expect("latch idx");
                match g.latches[li].init {
                    Init::Zero => {
                        pure_state[v] = true;
                        init_val[v] = false;
                    }
                    Init::One => {
                        pure_state[v] = true;
                        init_val[v] = true;
                    }
                    Init::Free | Init::Expr(_) => pure_state[v] = false,
                }
            }
            Node::And(a, b) => {
                let va = var(a) as usize;
                let vb = var(b) as usize;
                fanout[va] += 1;
                fanout[vb] += 1;
                if pure_state[va] && pure_state[vb] {
                    pure_state[v] = true;
                    init_val[v] = (init_val[va] ^ is_neg(a)) && (init_val[vb] ^ is_neg(b));
                }
            }
        }
    }
    for &r in g.bads.iter().chain(&g.constraints) {
        fanout[var(r) as usize] += 4;
    }
    for l in &g.latches {
        fanout[var(l.next) as usize] += 2;
    }

    let mut cands: Vec<(u32, usize)> = (0..n)
        .filter(|&v| matches!(g.nodes[v], Node::And(..)) && pure_state[v] && fanout[v] >= 2)
        .map(|v| (fanout[v], v))
        .collect();
    cands.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    cands.truncate(MAX_INN_SIGNALS);

    if cands.is_empty() {
        return (g.clone(), Vec::new());
    }

    // 2. Build `inn_g` = copy of `g` + shadow latches `s_aux` with `next(s_aux) = eval_next(node)`.
    let mut inn_g = g.clone();
    let mut next_memo: HashMap<usize, Lit> = HashMap::new();
    let mut aux_defs = Vec::with_capacity(cands.len());

    for &(_, v) in &cands {
        let init = if init_val[v] { Init::One } else { Init::Zero };
        let aux_lit = inn_g.latch(init);
        let aux_next = eval_cone_at_next(g, &mut inn_g, &latch_idx, v, &mut next_memo);
        inn_g.set_next(aux_lit, aux_next);
        aux_defs.push((aux_lit, 2 * v as u32));
    }

    (inn_g, aux_defs)
}

fn eval_cone_at_next(
    g: &Aig,
    inn_g: &mut Aig,
    latch_idx: &[Option<usize>],
    v: usize,
    memo: &mut HashMap<usize, Lit>,
) -> Lit {
    if let Some(&l) = memo.get(&v) {
        return l;
    }
    let res = match g.nodes[v] {
        Node::Const => FALSE,
        Node::Input => unreachable!("pure_state node cannot depend on inputs"),
        Node::Latch => {
            let li = latch_idx[v].expect("latch idx");
            g.latches[li].next
        }
        Node::And(a, b) => {
            let ma = eval_cone_at_next(g, inn_g, latch_idx, var(a) as usize, memo) ^ (a & 1);
            let mb = eval_cone_at_next(g, inn_g, latch_idx, var(b) as usize, memo) ^ (b & 1);
            inn_g.and(ma, mb)
        }
    };
    memo.insert(v, res);
    res
}

/// Project `inn_lemmas` (over `g.latches ∪ aux_latches`) onto `g.latches` by
/// substituting `aux_lit <=> orig_and_lit(g.latches)`.
fn project_inn_lemmas(
    g: &Aig,
    inn_g: &Aig,
    aux_defs: &[(Lit, Lit)],
    inn_lemmas: &[ic3::Cube],
    ctx: &RunContext,
) -> Result<Vec<ic3::Cube>, String> {
    if aux_defs.is_empty() {
        return Ok(inn_lemmas.to_vec());
    }
    let n_orig = g.latches.len();
    let orig_vars: std::collections::HashSet<u32> = g.latches.iter().map(|l| var(l.lit)).collect();

    let mut pure = Vec::new();
    let mut mixed = Vec::new();
    for cube in inn_lemmas {
        if cube.iter().all(|&l| orig_vars.contains(&var(l))) {
            pure.push(cube.clone());
        } else {
            mixed.push(cube.clone());
        }
    }
    if mixed.is_empty() {
        return Ok(pure);
    }

    // Project `Inv_inn(s, aux(s))` into minimal latch-only blocked cubes over `g.latches`.
    let build_inv_solver = |assert_inv: bool| -> (Box<dyn Sat>, Vec<i32>) {
        let mut sat = solver(ctx);
        let tru = 1i32;
        let mut nv = 2i32;
        let mut clauses = vec![vec![tru]];

        // Allocate SAT variables for `g`'s latches and their combinational AND cones.
        let mut node_var = vec![0i32; g.nodes.len()];
        let mut s_vars = Vec::with_capacity(n_orig);
        for (v, node) in g.nodes.iter().enumerate() {
            node_var[v] = match *node {
                Node::Const => -tru,
                Node::Input => {
                    let x = nv;
                    nv += 1;
                    x
                }
                Node::Latch => {
                    let x = nv;
                    nv += 1;
                    s_vars.push(x);
                    x
                }
                Node::And(a, b) => {
                    let out = nv;
                    nv += 1;
                    let la = node_var[var(a) as usize] * if is_neg(a) { -1 } else { 1 };
                    let lb = node_var[var(b) as usize] * if is_neg(b) { -1 } else { 1 };
                    clauses.push(vec![-out, la]);
                    clauses.push(vec![-out, lb]);
                    clauses.push(vec![out, -la, -lb]);
                    out
                }
            };
        }

        // Map every `inn_g` latch variable (original or shadow aux) to its SAT literal.
        let mut inn_lit_to_sat: HashMap<u32, i32> = HashMap::with_capacity(inn_g.latches.len());
        for l in &g.latches {
            inn_lit_to_sat.insert(var(l.lit), node_var[var(l.lit) as usize]);
        }
        for &(aux_lit, orig_and) in aux_defs {
            let sat_l = node_var[var(orig_and) as usize] * if is_neg(orig_and) { -1 } else { 1 };
            inn_lit_to_sat.insert(var(aux_lit), sat_l);
        }
        let map_inn = |l: Lit| -> i32 {
            let v = inn_lit_to_sat[&var(l)];
            if is_neg(l) {
                -v
            } else {
                v
            }
        };

        if assert_inv {
            // Assert `∧_{c ∈ inn_lemmas} !c`.
            for cube in inn_lemmas {
                let cl: Vec<i32> = cube.iter().map(|&l| -map_inn(l)).collect();
                clauses.push(cl);
            }
        } else {
            // Assert `∨_{c ∈ mixed} c` (already covered by `pure` is excluded via `pure` clauses!).
            for cube in &pure {
                let cl: Vec<i32> = cube.iter().map(|&l| -map_inn(l)).collect();
                clauses.push(cl);
            }
            let mut sels = Vec::with_capacity(mixed.len());
            for cube in &mixed {
                let sel = nv;
                nv += 1;
                sels.push(sel);
                for &l in cube {
                    clauses.push(vec![-sel, map_inn(l)]);
                }
            }
            clauses.push(sels);
        }

        sat.reserve(nv);
        for c in &clauses {
            sat.add_clause(c);
        }
        (sat, s_vars)
    };

    let (mut enum_sat, enum_s) = build_inv_solver(false);
    let (mut gen_sat, gen_s) = build_inv_solver(true);
    let mut out = pure;

    loop {
        if ctx.should_stop() {
            return Err(ctx.stop_reason().into());
        }
        if out.len() >= MAX_PROJECT_CUBES {
            return Err("ic3-inn projection cube limit reached".into());
        }
        match enum_sat.solve(&[]) {
            Answer::Unsat => return Ok(out),
            Answer::Unknown => return Err(ctx.stop_reason().into()),
            Answer::Sat => {
                let mut lits: Vec<(usize, bool)> = (0..n_orig)
                    .map(|i| (i, enum_sat.value(enum_s[i])))
                    .collect();
                let assume: Vec<i32> = lits
                    .iter()
                    .map(|&(i, v)| if v { gen_s[i] } else { -gen_s[i] })
                    .collect();
                if matches!(gen_sat.solve(&assume), Answer::Unsat) {
                    lits.retain(|&(i, v)| {
                        let sl = if v { gen_s[i] } else { -gen_s[i] };
                        gen_sat.failed(sl)
                    });
                }
                let mut idx = 0;
                while idx < lits.len() {
                    if ctx.should_stop() {
                        return Err(ctx.stop_reason().into());
                    }
                    let trial: Vec<i32> = lits
                        .iter()
                        .enumerate()
                        .filter(|&(j, _)| j != idx)
                        .map(|(_, &(i, v))| if v { gen_s[i] } else { -gen_s[i] })
                        .collect();
                    if matches!(gen_sat.solve(&trial), Answer::Unsat) {
                        lits.remove(idx);
                    } else {
                        idx += 1;
                    }
                }
                let block: Vec<i32> = lits
                    .iter()
                    .map(|&(i, v)| if v { -enum_s[i] } else { enum_s[i] })
                    .collect();
                enum_sat.add_clause(&block);

                let mut cube: ic3::Cube = lits
                    .into_iter()
                    .map(|(i, v)| {
                        let l = g.latches[i].lit;
                        if v {
                            l
                        } else {
                            neg(l)
                        }
                    })
                    .collect();
                cube.sort_unstable();
                out.push(cube);
            }
        }
    }
}

#[allow(dead_code)]
fn _keep_true_const() -> Lit {
    TRUE
}
