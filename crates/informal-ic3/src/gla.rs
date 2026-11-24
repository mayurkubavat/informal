//! Gate-Level Abstraction with UNSAT-Core CEGAR (`informal-gla`).
//!
//! Starts from a tiny over-approximate localization containing only latches
//! directly in the shallow combinational cone of `bads` and `constraints`,
//! replacing all other latches with unconstrained primary inputs. Runs
//! DynAMic+EXCTG IC3 on the abstract model:
//! - Abstract `Safe` -> maps lemmas directly back to concrete latches and
//!   certifies via [`informal_bmc::certify_lemmas`] (always sound because the
//!   abstract model over-approximates all concrete traces).
//! - Abstract `Unsafe { depth: d }` -> checks concrete feasibility at depth `d`
//!   with assumption-guarded cut latches; SAT yields a genuine concrete trace,
//!   while UNSAT extracts the minimal UNSAT core of cut latches to refine.

use informal_aig::{is_neg, neg, var, Aig, Init, Lit, Node, FALSE};
use informal_bmc::cnf;
use informal_bmc::{ic3, invariant, seed_from_hub, solver, Prepared, PreparedTrace};
use informal_engine::{Progress, RunContext, Verdict};
use informal_sat::Answer;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Outcome of GLA-CEGAR on a prepared AIG.
pub enum GlaOutcome {
    Safe(Vec<ic3::Cube>),
    Unsafe {
        depth: usize,
        trace: PreparedTrace,
        witness: String,
    },
    Aborted(String),
}

/// Run Gate-Level Abstraction + CEGAR on prepared AIG `g`.
pub fn run_gla(g: &Prepared, max_depth: Option<usize>, ctx: &RunContext) -> GlaOutcome {
    if g.latches.iter().any(|l| matches!(l.init, Init::Expr(_))) {
        return GlaOutcome::Aborted("latch inits that are not constants".into());
    }
    let n_latches = g.latches.len();
    if n_latches == 0 {
        return run_concrete_ic3(g, ctx);
    }

    let (mut hub, mut standing) = seed_from_hub(g, ctx, "informal-gla");
    // Seed initial localization: latches in the combinational cone of `bads` and `constraints`.
    let mut visible = direct_cone_latches(g);
    let mut round = 0usize;

    loop {
        if ctx.should_stop() {
            return GlaOutcome::Aborted(ctx.stop_reason().into());
        }
        if let Some(h) = &mut hub {
            if h.has_new_standing() {
                standing.extend(h.poll_standing_verified(
                    g,
                    &standing,
                    ctx,
                    Duration::from_millis(50),
                ));
            }
        }
        round += 1;
        let vis_count = visible.iter().filter(|&&v| v).count();
        ctx.report(Progress {
            engine: "informal-gla".into(),
            bound: Some(round as u32),
            note: format!("gla round {round}: {vis_count}/{n_latches} latches"),
            output: None,
        });

        let (abs_g, abs_to_conc) = build_abstract_aig(g, &visible);
        let c = ctx.clone();
        let cfg = ic3::Config {
            dynamic_order: true,
            exctg: true,
            houdini_every: 4,
            ..ic3::Config::default()
        };
        let Some(mut ic3) = ic3::Ic3::with_config(&abs_g, Box::new(move || solver(&c)), cfg) else {
            return GlaOutcome::Aborted("ic3 init failed on abstract model".into());
        };
        if !standing.is_empty() {
            let abs_cands = map_conc_cubes_to_abs(g, &abs_g, &abs_to_conc, &standing);
            if !abs_cands.is_empty() {
                let now = Instant::now();
                let rest = ctx.deadline.saturating_duration_since(now);
                let mut h = ctx.clone();
                h.deadline = now + Duration::from_millis(50).min(rest / 10);
                if let Some(abs_inv) = invariant::houdini(&abs_g, abs_cands, || solver(&h), &h) {
                    ic3.assume_invariant(abs_inv);
                }
            }
        }
        let res = ic3.check(ctx);
        // Any standing invariant proved on over-approximate `abs_g` holds on
        // every concrete trace of `g`: share it even before CEGAR refinement.
        if let Some(h) = &hub {
            let abs_standing = ic3.standing_invariant();
            if !abs_standing.is_empty() {
                let conc = map_abs_cubes_to_conc(&abs_g, g, &abs_to_conc, &abs_standing);
                h.publish_standing(&conc);
            }
        }
        match res {
            Verdict::Safe => {
                let Some(abs_lemmas) = ic3.invariant() else {
                    return GlaOutcome::Aborted("missing abstract invariant".into());
                };
                let conc_lemmas = map_abs_cubes_to_conc(&abs_g, g, &abs_to_conc, &abs_lemmas);
                return GlaOutcome::Safe(conc_lemmas);
            }
            Verdict::Unsafe { depth: Some(d) } => {
                let d = d as usize;
                if max_depth.is_some_and(|m| d > m) {
                    return GlaOutcome::Aborted(format!(
                        "bound {m} reached",
                        m = max_depth.unwrap()
                    ));
                }
                match refine_or_confirm_cex(g, &visible, d, ctx) {
                    RefineStep::ConcreteCex { trace, witness } => {
                        return GlaOutcome::Unsafe {
                            depth: d,
                            trace,
                            witness,
                        };
                    }
                    RefineStep::AddLatches(core) => {
                        let mut added = 0usize;
                        for li in core {
                            if !visible[li] {
                                visible[li] = true;
                                added += 1;
                            }
                        }
                        if added == 0 {
                            if vis_count == n_latches {
                                return GlaOutcome::Aborted("refinement stalled".into());
                            }
                            visible.fill(true);
                        }
                    }
                    RefineStep::Aborted(r) => return GlaOutcome::Aborted(r),
                }
            }
            _ => return GlaOutcome::Aborted(ctx.stop_reason().into()),
        }
    }
}

fn map_abs_cubes_to_conc(
    abs_g: &Aig,
    g: &Aig,
    abs_to_conc: &[usize],
    cubes: &[ic3::Cube],
) -> Vec<ic3::Cube> {
    let mut abs_var_to_conc: HashMap<u32, Lit> = HashMap::with_capacity(abs_to_conc.len());
    for (ai, &ci) in abs_to_conc.iter().enumerate() {
        abs_var_to_conc.insert(var(abs_g.latches[ai].lit), g.latches[ci].lit);
    }
    cubes
        .iter()
        .map(|cube| {
            let mut mapped: ic3::Cube = cube
                .iter()
                .map(|&al| {
                    let cl = abs_var_to_conc[&var(al)];
                    if is_neg(al) {
                        neg(cl)
                    } else {
                        cl
                    }
                })
                .collect();
            mapped.sort_unstable();
            mapped
        })
        .collect()
}

fn map_conc_cubes_to_abs(
    g: &Aig,
    abs_g: &Aig,
    abs_to_conc: &[usize],
    cubes: &[ic3::Cube],
) -> Vec<ic3::Cube> {
    let mut conc_var_to_abs: HashMap<u32, Lit> = HashMap::with_capacity(abs_to_conc.len());
    for (ai, &ci) in abs_to_conc.iter().enumerate() {
        conc_var_to_abs.insert(var(g.latches[ci].lit), abs_g.latches[ai].lit);
    }
    cubes
        .iter()
        .filter_map(|cube| {
            let mut mapped = Vec::with_capacity(cube.len());
            for &cl in cube {
                let al = *conc_var_to_abs.get(&var(cl))?;
                mapped.push(if is_neg(cl) { neg(al) } else { al });
            }
            mapped.sort_unstable();
            Some(mapped)
        })
        .collect()
}

fn run_concrete_ic3(g: &Aig, ctx: &RunContext) -> GlaOutcome {
    let c = ctx.clone();
    let cfg = ic3::Config {
        dynamic_order: true,
        exctg: true,
        ..ic3::Config::default()
    };
    let Some(mut ic3) = ic3::Ic3::with_config(g, Box::new(move || solver(&c)), cfg) else {
        return GlaOutcome::Aborted("ic3 init failed".into());
    };
    match ic3.check(ctx) {
        Verdict::Safe => match ic3.invariant() {
            Some(inv) => GlaOutcome::Safe(inv),
            None => GlaOutcome::Aborted("missing invariant".into()),
        },
        Verdict::Unsafe { depth: Some(d) } => {
            match refine_or_confirm_cex(g, &vec![true; g.latches.len()], d as usize, ctx) {
                RefineStep::ConcreteCex { trace, witness } => GlaOutcome::Unsafe {
                    depth: d as usize,
                    trace,
                    witness,
                },
                _ => GlaOutcome::Aborted(ctx.stop_reason().into()),
            }
        }
        _ => GlaOutcome::Aborted(ctx.stop_reason().into()),
    }
}

/// Find latches directly appearing in the combinational cone of `bads` and `constraints`.
fn direct_cone_latches(g: &Aig) -> Vec<bool> {
    let mut latch_idx = vec![None; g.nodes.len()];
    for (li, l) in g.latches.iter().enumerate() {
        latch_idx[var(l.lit) as usize] = Some(li);
    }
    let mut seen = vec![false; g.nodes.len()];
    let mut stack: Vec<usize> = g
        .bads
        .iter()
        .chain(&g.constraints)
        .map(|&l| var(l) as usize)
        .collect();
    let mut vis = vec![false; g.latches.len()];
    while let Some(v) = stack.pop() {
        if seen[v] {
            continue;
        }
        seen[v] = true;
        if let Some(li) = latch_idx[v] {
            vis[li] = true;
        } else if let Node::And(a, b) = g.nodes[v] {
            stack.push(var(a) as usize);
            stack.push(var(b) as usize);
        }
    }
    vis
}

/// Build the abstract AIG keeping only `visible[li]` latches as latches and
/// replacing all other latches with fresh primary inputs.
fn build_abstract_aig(g: &Aig, visible: &[bool]) -> (Aig, Vec<usize>) {
    let mut latch_idx = vec![None; g.nodes.len()];
    for (li, l) in g.latches.iter().enumerate() {
        latch_idx[var(l.lit) as usize] = Some(li);
    }
    let mut out = Aig::new();
    let mut map = vec![FALSE; g.nodes.len()];
    let mut abs_to_conc = Vec::new();
    for (v, node) in g.nodes.iter().enumerate() {
        map[v] = match *node {
            Node::Const => FALSE,
            Node::Input => out.input(),
            Node::Latch => {
                let li = latch_idx[v].expect("latch idx");
                if visible[li] {
                    abs_to_conc.push(li);
                    out.latch(g.latches[li].init)
                } else {
                    out.input()
                }
            }
            Node::And(a, b) => {
                let ma = map[var(a) as usize] ^ (a & 1);
                let mb = map[var(b) as usize] ^ (b & 1);
                out.and(ma, mb)
            }
        };
    }
    let xl = |l: Lit| map[var(l) as usize] ^ (l & 1);
    for (ai, &ci) in abs_to_conc.iter().enumerate() {
        let al = out.latches[ai].lit;
        out.set_next(al, xl(g.latches[ci].next));
    }
    out.bads = g.bads.iter().map(|&b| xl(b)).collect();
    out.constraints = g.constraints.iter().map(|&c| xl(c)).collect();
    (out, abs_to_conc)
}

enum RefineStep {
    ConcreteCex {
        trace: PreparedTrace,
        witness: String,
    },
    AddLatches(Vec<usize>),
    Aborted(String),
}

/// Check concrete depth-`d` feasibility of `g` while guarding every cut latch's
/// init & transition clauses behind a per-latch assumption literal `act[li]`.
/// SAT yields a concrete trace; UNSAT extracts the minimal failing cut-latch core.
fn refine_or_confirm_cex(g: &Aig, visible: &[bool], d: usize, ctx: &RunContext) -> RefineStep {
    let tpl = cnf::template(g);
    let mut sat = solver(ctx);
    let tru = 1i32;
    let mut nv = 2i32;
    let mut clauses: Vec<Vec<i32>> = vec![vec![tru]];

    let mut cut_acts: Vec<(usize, i32)> = Vec::new();
    let mut act_of_latch: Vec<Option<i32>> = vec![None; g.latches.len()];
    for (li, &is_vis) in visible.iter().enumerate() {
        if !is_vis {
            let a = nv;
            nv += 1;
            act_of_latch[li] = Some(a);
            cut_acts.push((li, a));
        }
    }

    let mut frames: Vec<Vec<i32>> = Vec::with_capacity(d + 1);
    let mut frame_inputs: Vec<Vec<i32>> = Vec::with_capacity(d + 1);
    let mut latch_idx = vec![None; g.nodes.len()];
    for (li, l) in g.latches.iter().enumerate() {
        latch_idx[var(l.lit) as usize] = Some(li);
    }

    for t in 0..=d {
        let mut f = vec![0i32; g.nodes.len()];
        let mut in_t = Vec::with_capacity(g.inputs.len());
        for (v, node) in g.nodes.iter().enumerate() {
            f[v] = match *node {
                Node::Const => -tru,
                Node::Input => {
                    let x = nv;
                    nv += 1;
                    in_t.push(x);
                    x
                }
                Node::Latch => {
                    let li = latch_idx[v].expect("latch idx");
                    if let Some(act) = act_of_latch[li] {
                        // Cut latch: fresh variable at every frame, guarded by `act`.
                        let x = nv;
                        nv += 1;
                        if t == 0 {
                            match g.latches[li].init {
                                Init::Zero => clauses.push(vec![-act, -x]),
                                Init::One => clauses.push(vec![-act, x]),
                                Init::Free | Init::Expr(_) => {}
                            }
                        } else {
                            let prev_next = flit(&frames[t - 1], g.latches[li].next);
                            clauses.push(vec![-act, -x, prev_next]);
                            clauses.push(vec![-act, x, -prev_next]);
                        }
                        x
                    } else if t == 0 {
                        match g.latches[li].init {
                            Init::Zero => -tru,
                            Init::One => tru,
                            Init::Free | Init::Expr(_) => {
                                let x = nv;
                                nv += 1;
                                x
                            }
                        }
                    } else {
                        flit(&frames[t - 1], g.latches[li].next)
                    }
                }
                Node::And(..) if tpl.live[v + 1] => {
                    let x = nv;
                    nv += 1;
                    x
                }
                Node::And(..) => 0,
            };
        }
        let mut buf = Vec::new();
        for c in &tpl.clauses {
            buf.clear();
            buf.extend(c.iter().map(|&tl| {
                let x = f[tl.unsigned_abs() as usize - 1];
                if tl < 0 {
                    -x
                } else {
                    x
                }
            }));
            clauses.push(buf.clone());
        }
        for &c in &g.constraints {
            clauses.push(vec![flit(&f, c)]);
        }
        frames.push(f);
        frame_inputs.push(in_t);
    }

    let bad_lits: Vec<i32> = g.bads.iter().map(|&b| flit(&frames[d], b)).collect();
    clauses.push(bad_lits);

    sat.reserve(nv);
    for c in &clauses {
        sat.add_clause(c);
    }
    let assumptions: Vec<i32> = cut_acts.iter().map(|&(_, a)| a).collect();
    match sat.solve(&assumptions) {
        Answer::Sat => {
            let init: Vec<bool> = g
                .latches
                .iter()
                .map(|l| sat.value(flit(&frames[0], l.lit)))
                .collect();
            let inputs: Vec<Vec<bool>> = frame_inputs
                .iter()
                .map(|f_in| f_in.iter().map(|&v| sat.value(v)).collect())
                .collect();
            let witness = format_named_witness(g, &init, &inputs);
            RefineStep::ConcreteCex {
                trace: PreparedTrace { init, inputs },
                witness,
            }
        }
        Answer::Unsat => {
            let core: Vec<usize> = cut_acts
                .iter()
                .filter(|&&(_, a)| sat.failed(a))
                .map(|&(li, _)| li)
                .collect();
            RefineStep::AddLatches(core)
        }
        Answer::Unknown => RefineStep::Aborted(ctx.stop_reason().into()),
    }
}

#[inline]
fn flit(frame: &[i32], l: Lit) -> i32 {
    let x = frame[var(l) as usize];
    if is_neg(l) {
        -x
    } else {
        x
    }
}

pub(crate) fn format_named_witness(g: &Aig, init: &[bool], inputs: &[Vec<bool>]) -> String {
    let bit = |b: bool| if b { '1' } else { '0' };
    let d = inputs.len().saturating_sub(1);
    let mut s = format!("informalw 1\ndepth {d}\ninit");
    for (l, &val) in g.latches.iter().zip(init) {
        if matches!(l.init, Init::Zero | Init::One) {
            continue;
        }
        if let Some(name) = g.names.get(&var(l.lit)) {
            s += &format!(" {name}={}", bit(val));
        }
    }
    for (f, frame_in) in inputs.iter().enumerate() {
        s += &format!("\n@{f}");
        for (&i, &val) in g.inputs.iter().zip(frame_in) {
            if let Some(name) = g.names.get(&var(i)) {
                s += &format!(" {name}={}", bit(val));
            }
        }
    }
    s.push('\n');
    s
}
