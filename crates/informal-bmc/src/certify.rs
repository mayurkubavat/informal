//! Independent check of a SAFE verdict: an inductive invariant, carried
//! back to the model as loaded (before simplification and latch merging),
//! checked with three fresh SAT queries.
//!
//! - **Init:** every initial state (that meets the constraints) satisfies
//!   the invariant.
//! - **Consecution:** from a state that satisfies the invariant and the
//!   constraints, every step into a state that meets the constraints
//!   satisfies it again.
//! - **Safety:** no state that satisfies the invariant and the
//!   constraints is bad.
//!
//! The queries use their own plain Tseitin encoding of the loaded AIG, not
//! the engines' CNF template, variable elimination or preprocessing, so a
//! bug there (the old `scorr` and lemma-import bugs were of that kind)
//! makes the check fail instead of passing silently. What is still
//! trusted: the AIGER/Btor2 reader and bit-blaster, this file, and the
//! SAT solver's UNSAT answers.

use informal_aig::preprocess::LatchMap;
use informal_aig::{is_neg, var, Aig, Init, Lit, Node, FALSE, TRUE};
use informal_sat::{Answer, Sat};
use std::collections::HashMap;

/// A clause over AIG literals of the loaded model's latches.
pub type Clause = Vec<Lit>;

/// In an invariant of a clock-folded model ([`check_folded`]): the literal
/// "the constraints held in the half-step before this state" (the fold's
/// `ok` latch, which the loaded model does not have). `OK ^ 1` negates it.
pub const OK: Lit = u32::MAX - 1;

/// The invariant over the loaded model `orig`: IC3's `lemmas` (cubes over
/// `prepared`'s latch literals, each blocked: the clause is the cube's
/// negation), plus what preprocessing assumed: latches it found constant
/// and latches it merged (`map`, see [`LatchMap`]).
pub fn lift(
    orig: &Aig,
    map: &LatchMap,
    prepared: &Aig,
    lemmas: &[Vec<Lit>],
) -> Result<Vec<Clause>, String> {
    lift_reps(orig, map, prepared, lemmas).map(|(c, _)| c)
}

/// Clauses for equalities between literals of the loaded model's latches
/// (`x == y`, where `y` may be a constant): what latch correspondence
/// proved and merged. They are part of the invariant: the merged model's
/// lemmas only hold with them, and a merged latch that then fell out of
/// the cone appears nowhere else.
pub fn equalities(eqs: &[(Lit, Lit)]) -> Vec<Clause> {
    let mut out = Vec::new();
    for &(x, y) in eqs {
        if y == TRUE {
            out.push(vec![x]);
        } else if y == FALSE {
            out.push(vec![x ^ 1]);
        } else {
            out.push(vec![x ^ 1, y]);
            out.push(vec![x, y ^ 1]);
        }
    }
    out
}

/// [`lift`] for a clock-folded model: `ok` is the prepared model's
/// literal for the fold's `ok` latch (if it survived simplification as a
/// latch), which becomes [`OK`] in the result.
pub fn lift_folded(
    orig: &Aig,
    map: &LatchMap,
    prepared: &Aig,
    lemmas: &[Vec<Lit>],
    ok: Option<Lit>,
) -> Result<Vec<Clause>, String> {
    let extra = ok.filter(|&l| l != TRUE && l != FALSE).map(|l| (l, OK));
    lift_extra(orig, map, prepared, lemmas, extra).map(|(c, _)| c)
}

/// [`lift`], also returning, for each of `prepared`'s latches that some
/// loaded latch maps to, one loaded literal equal to it: two states differ
/// as `prepared` sees them exactly when they differ on these.
pub fn lift_reps(
    orig: &Aig,
    map: &LatchMap,
    prepared: &Aig,
    lemmas: &[Vec<Lit>],
) -> Result<(Vec<Clause>, Vec<Lit>), String> {
    lift_extra(orig, map, prepared, lemmas, None)
}

/// [`lift_reps`] with one more prepared literal and what it stands for.
fn lift_extra(
    orig: &Aig,
    map: &LatchMap,
    prepared: &Aig,
    lemmas: &[Vec<Lit>],
    extra: Option<(Lit, Lit)>,
) -> Result<(Vec<Clause>, Vec<Lit>), String> {
    if map.len() != orig.latches.len() {
        return Err(format!(
            "latch map has {} entries for {} latches",
            map.len(),
            orig.latches.len()
        ));
    }
    let prepared_latch: std::collections::HashSet<u32> =
        prepared.latches.iter().map(|l| var(l.lit)).collect();
    // Prepared latch variable -> the loaded model's literal equal to it.
    let mut rep: HashMap<u32, Lit> = HashMap::new();
    if let Some((p, o)) = extra {
        rep.insert(var(p), o ^ (p & 1));
    }
    let mut inv = Vec::new();
    for (l, m) in orig.latches.iter().zip(map) {
        let Some(m) = *m else { continue };
        let x = l.lit;
        if m == FALSE {
            inv.push(vec![x ^ 1]);
        } else if m == TRUE {
            inv.push(vec![x]);
        } else {
            if !prepared_latch.contains(&var(m)) {
                return Err(format!("latch map points at non-latch literal {m}"));
            }
            // x == m, so the prepared latch's positive literal equals x ^ neg(m).
            let x = x ^ (m & 1);
            match rep.get(&var(m)) {
                None => {
                    rep.insert(var(m), x);
                }
                Some(&y) => {
                    inv.push(vec![x ^ 1, y]);
                    inv.push(vec![x, y ^ 1]);
                }
            }
        }
    }
    for cube in lemmas {
        let mut c = Vec::with_capacity(cube.len());
        for &l in cube {
            let Some(&y) = rep.get(&var(l)) else {
                return Err(format!(
                    "lemma literal {l} is not a latch of the loaded model"
                ));
            };
            c.push((y ^ (l & 1)) ^ 1);
        }
        inv.push(c);
    }
    let mut reps: Vec<Lit> = rep.into_values().filter(|&l| l | 1 != OK | 1).collect();
    reps.sort_unstable();
    Ok((inv, reps))
}

/// SAT literal of every AIG variable in one time frame.
struct Frame(Vec<i32>);

impl Frame {
    fn lit(&self, l: Lit) -> i32 {
        let x = self.0[var(l) as usize];
        if is_neg(l) {
            -x
        } else {
            x
        }
    }
}

/// The SAT literal of `l` in `f`, with `ok` standing for [`OK`].
fn lit_ok(f: &Frame, l: Lit, ok: i32) -> i32 {
    if l | 1 == OK | 1 {
        if l == OK {
            ok
        } else {
            -ok
        }
    } else {
        f.lit(l)
    }
}

struct Enc {
    sat: Box<dyn Sat>,
    next: i32,
}

impl Enc {
    fn new(mut sat: Box<dyn Sat>) -> Self {
        sat.reserve(1);
        sat.add_clause(&[1]); // variable 1 is true
        Enc { sat, next: 2 }
    }
    fn fresh(&mut self) -> i32 {
        self.sat.reserve(1);
        self.next += 1;
        self.next - 1
    }
    /// One frame of `g` over the nodes in `cone` (others stay unencoded);
    /// latch values from `latch` (by latch index) or free.
    fn frame(&mut self, g: &Aig, cone: &[bool], latch: Option<&[i32]>) -> Frame {
        let index: HashMap<u32, usize> = g
            .latches
            .iter()
            .enumerate()
            .map(|(i, l)| (var(l.lit), i))
            .collect();
        let mut f = Frame(vec![0; g.nodes.len()]);
        for (v, n) in g.nodes.iter().enumerate() {
            if !cone[v] {
                continue;
            }
            f.0[v] = match *n {
                Node::Const => -1,
                Node::Input => self.fresh(),
                Node::Latch => match latch {
                    Some(vals) => {
                        let x = vals[index[&(v as u32)]];
                        debug_assert!(x != 0, "next state outside the cone");
                        x
                    }
                    None => self.fresh(),
                },
                Node::And(a, b) => {
                    let (a, b) = (f.lit(a), f.lit(b));
                    let x = self.fresh();
                    self.sat.add_clause(&[-x, a]);
                    self.sat.add_clause(&[-x, b]);
                    self.sat.add_clause(&[x, -a, -b]);
                    x
                }
            };
        }
        f
    }
    fn assert_inv(&mut self, f: &Frame, inv: &[Clause]) {
        self.assert_inv_ok(f, inv, 1)
    }
    /// [`Self::assert_inv`] with the value of [`OK`] in this frame.
    fn assert_inv_ok(&mut self, f: &Frame, inv: &[Clause], ok: i32) {
        for c in inv {
            let c: Vec<i32> = c.iter().map(|&l| lit_ok(f, l, ok)).collect();
            self.sat.add_clause(&c);
        }
    }
    /// Some clause of `inv` is false in `f`; returns the selector to assume.
    fn violate_inv(&mut self, f: &Frame, inv: &[Clause]) -> i32 {
        self.violate_inv_ok(f, inv, 1)
    }
    /// [`Self::violate_inv`] with the value of [`OK`] in this frame.
    fn violate_inv_ok(&mut self, f: &Frame, inv: &[Clause], ok: i32) -> i32 {
        let any = self.fresh();
        let mut or = vec![-any];
        for c in inv {
            let s = self.fresh();
            for &l in c {
                self.sat.add_clause(&[-s, -lit_ok(f, l, ok)]);
            }
            or.push(s);
        }
        self.sat.add_clause(&or);
        any
    }
    fn constraints(&mut self, g: &Aig, f: &Frame) {
        for &c in &g.constraints {
            let l = f.lit(c);
            self.sat.add_clause(&[l]);
        }
    }
    fn unsat(&mut self, assume: &[i32], what: &str) -> Result<(), String> {
        match self.sat.solve(assume) {
            Answer::Unsat => Ok(()),
            Answer::Sat => Err(format!("{what} fails")),
            Answer::Unknown => Err(format!("{what} check stopped")),
        }
    }
}

/// Nodes the queries can depend on: the cone of the bad and constraint
/// outputs and of the invariant's latches, through latch next states and
/// inits. Everything outside is unconstrained by the queries, so leaving
/// it out changes no answer (the big CPU models have many unrelated
/// properties' logic in one file).
fn cone(g: &Aig, inv: &[Clause]) -> Vec<bool> {
    let index: HashMap<u32, usize> = g
        .latches
        .iter()
        .enumerate()
        .map(|(i, l)| (var(l.lit), i))
        .collect();
    let mut need = vec![false; g.nodes.len()];
    let mut stack: Vec<u32> = g
        .bads
        .iter()
        .chain(&g.constraints)
        .chain(inv.iter().flatten().filter(|&&l| l | 1 != OK | 1))
        .map(|&l| var(l))
        .collect();
    while let Some(v) = stack.pop() {
        if std::mem::replace(&mut need[v as usize], true) {
            continue;
        }
        match g.nodes[v as usize] {
            Node::And(a, b) => stack.extend([var(a), var(b)]),
            Node::Latch => {
                let l = &g.latches[index[&v]];
                stack.push(var(l.next));
                if let Init::Expr(e) = l.init {
                    stack.push(var(e));
                }
            }
            _ => {}
        }
    }
    need
}

impl Enc {
    /// The initial-state constraints on frame `f`.
    fn init(&mut self, g: &Aig, cone: &[bool], f: &Frame) {
        for l in g.latches.iter().filter(|l| cone[var(l.lit) as usize]) {
            let x = f.lit(l.lit);
            match l.init {
                Init::Zero => self.sat.add_clause(&[-x]),
                Init::One => self.sat.add_clause(&[x]),
                Init::Expr(i) => {
                    let y = f.lit(i);
                    self.sat.add_clause(&[-x, y]);
                    self.sat.add_clause(&[x, -y]);
                }
                Init::Free => {}
            }
        }
    }
    /// The frame after `f`.
    fn step(&mut self, g: &Aig, cone: &[bool], f: &Frame) -> Frame {
        let next: Vec<i32> = g
            .latches
            .iter()
            .map(|l| {
                if cone[var(l.lit) as usize] {
                    f.lit(l.next)
                } else {
                    0
                }
            })
            .collect();
        self.frame(g, cone, Some(&next))
    }
    /// A literal true when some bad output is true in `f`.
    fn any_bad(&mut self, g: &Aig, f: &Frame) -> i32 {
        let any = self.fresh();
        let mut or = vec![-any];
        or.extend(g.bads.iter().map(|&b| f.lit(b)));
        self.sat.add_clause(&or);
        // and back: any <- each bad, so that -any means no bad.
        for &b in &g.bads {
            self.sat.add_clause(&[any, -f.lit(b)]);
        }
        any
    }
}

/// Init and consecution: `inv` holds in every reachable state.
fn check_inductive(
    g: &Aig,
    cone: &[bool],
    inv: &[Clause],
    new_sat: &dyn Fn() -> Box<dyn Sat>,
) -> Result<(), String> {
    let mut e = Enc::new(new_sat());
    let f0 = e.frame(g, cone, None);
    e.init(g, cone, &f0);
    e.constraints(g, &f0);
    let s = e.violate_inv(&f0, inv);
    e.unsat(&[s], "init")?;

    let mut e = Enc::new(new_sat());
    let f0 = e.frame(g, cone, None);
    e.assert_inv(&f0, inv);
    e.constraints(g, &f0);
    let f1 = e.step(g, cone, &f0);
    e.constraints(g, &f1);
    let s = e.violate_inv(&f1, inv);
    e.unsat(&[s], "consecution")
}

/// Check that `inv` is an inductive invariant of `g` that excludes every
/// bad state. Each query gets a fresh solver from `new_sat`.
pub fn check(g: &Aig, inv: &[Clause], new_sat: &dyn Fn() -> Box<dyn Sat>) -> Result<(), String> {
    let cone = cone(g, inv);
    check_inductive(g, &cone, inv, new_sat)?;
    let mut e = Enc::new(new_sat());
    let f0 = e.frame(g, &cone, None);
    e.assert_inv(&f0, inv);
    e.constraints(g, &f0);
    let any = e.any_bad(g, &f0);
    e.unsat(&[any], "safety")
}

/// Check a k-induction proof at depth `k` (k >= 1) on `g`: `inv` (the
/// strengthening, and what preprocessing assumed) is an inductive
/// invariant; no bad state is reachable in fewer than `k` steps; and on
/// every path of `k` good states that satisfy `inv` and differ pairwise,
/// the next state is good. The simple-path constraints are added lazily,
/// as the engine does, but with this file's own encoding.
///
/// Two states differ when they differ on some latch of the cone this
/// file computes itself on `g` (bad and constraint outputs and the
/// invariant's latches, through next states). The engine's own notion of
/// "differ" (the latches its preprocessing kept) is not taken on trust:
/// distinctness over fewer latches than the cone has would cut real
/// paths and could let a wrong proof pass. Where preprocessing merged or
/// fixed latches correctly, `inv` says so and is assumed in every frame,
/// so both notions agree and no correct proof is lost.
pub fn check_kind(
    g: &Aig,
    inv: &[Clause],
    k: usize,
    new_sat: &dyn Fn() -> Box<dyn Sat>,
    stop: &dyn Fn() -> bool,
) -> Result<(), String> {
    if k == 0 {
        return Err("k-induction at depth 0".into());
    }
    let cone = cone(g, inv);
    let state: Vec<Lit> = g
        .latches
        .iter()
        .map(|l| l.lit)
        .filter(|&l| cone[var(l) as usize])
        .collect();
    let reps = &state[..];
    check_inductive(g, &cone, inv, new_sat)?;

    // Base: no bad state in frames 0..k-1 from the initial states.
    let mut e = Enc::new(new_sat());
    let mut f = e.frame(g, &cone, None);
    e.init(g, &cone, &f);
    for j in 0..k {
        if stop() {
            return Err("base check stopped".into());
        }
        if j > 0 {
            f = e.step(g, &cone, &f);
        }
        e.constraints(g, &f);
        let bad = e.any_bad(g, &f);
        e.unsat(&[bad], "base")?;
        e.sat.add_clause(&[-bad]);
    }

    // Step: frames 0..k-1 good and in inv, frame k bad.
    let mut e = Enc::new(new_sat());
    let mut frames = vec![e.frame(g, &cone, None)];
    for j in 0..=k {
        if j > 0 {
            let f = e.step(g, &cone, &frames[j - 1]);
            frames.push(f);
        }
        let f = &frames[j];
        e.assert_inv(f, inv);
        e.constraints(g, f);
    }
    for f in &frames[..k] {
        let bad = e.any_bad(g, f);
        e.sat.add_clause(&[-bad]);
    }
    let bad = e.any_bad(g, &frames[k]);
    loop {
        if stop() {
            return Err("step check stopped".into());
        }
        match e.sat.solve(&[bad]) {
            Answer::Unsat => return Ok(()),
            Answer::Unknown => return Err("step check stopped".into()),
            Answer::Sat => {}
        }
        // Two equal states on the path: require them to differ.
        let val = |f: &Frame, e: &Enc| -> Vec<bool> {
            reps.iter().map(|&r| e.sat.value(f.lit(r))).collect()
        };
        let vals: Vec<Vec<bool>> = frames.iter().map(|f| val(f, &e)).collect();
        let pair = (0..=k)
            .flat_map(|i| (i + 1..=k).map(move |j| (i, j)))
            .find(|&(i, j)| vals[i] == vals[j]);
        let Some((i, j)) = pair else {
            return Err("step fails".into());
        };
        let mut or = Vec::new();
        for &r in reps {
            let (x, y) = (frames[i].lit(r), frames[j].lit(r));
            let d = e.fresh();
            // d -> x != y
            e.sat.add_clause(&[-d, x, y]);
            e.sat.add_clause(&[-d, -x, -y]);
            or.push(d);
        }
        e.sat.add_clause(&or);
    }
}

/// Check the invariant of a clock-folded model (`preprocess::fold_clock`)
/// on `g` as loaded. `inv` is over `g`'s latches and [`OK`]; it must
/// contain the clock's phase as a unit clause (the fold's latch map gives
/// it). One step of the folded model is two of `g`, so the invariant is
/// checked as a 2-step one, on the states of the clock's initial phase:
///
/// - **Init:** every initial state satisfies it (with OK true).
/// - **Consecution:** from a state that satisfies it (OK true) and the
///   constraints, two constraint-meeting steps on, it holds again in a
///   state that meets the constraints (OK true again).
/// - **Safety:** such a state is not bad, and neither is its successor
///   when the successor meets the constraints.
///
/// Then every state on a path that keeps the constraints is good: even
/// steps by induction over pairs of steps, odd steps by the second half
/// of safety.
pub fn check_folded(
    g: &Aig,
    inv: &[Clause],
    new_sat: &dyn Fn() -> Box<dyn Sat>,
) -> Result<(), String> {
    let cone = cone(g, inv);
    // Init.
    let mut e = Enc::new(new_sat());
    let f0 = e.frame(g, &cone, None);
    e.init(g, &cone, &f0);
    e.constraints(g, &f0);
    let s = e.violate_inv_ok(&f0, inv, 1);
    e.unsat(&[s], "init")?;

    // Consecution over two steps.
    let mut e = Enc::new(new_sat());
    let f0 = e.frame(g, &cone, None);
    e.assert_inv_ok(&f0, inv, 1);
    e.constraints(g, &f0);
    let f1 = e.step(g, &cone, &f0);
    e.constraints(g, &f1);
    let f2 = e.step(g, &cone, &f1);
    // As in the 1-step check, the invariant only has to hold in states
    // that meet the constraints, and both steps into it must have met
    // them (so OK is true there). Houdini's lemmas are proved in exactly
    // this sense: its two frames both carry the constraints.
    e.constraints(g, &f2);
    let s = e.violate_inv_ok(&f2, inv, 1);
    e.unsat(&[s], "consecution")?;

    // Safety now, and one step on.
    let mut e = Enc::new(new_sat());
    let f0 = e.frame(g, &cone, None);
    e.assert_inv_ok(&f0, inv, 1);
    e.constraints(g, &f0);
    let bad0 = e.any_bad(g, &f0);
    e.unsat(&[bad0], "safety")?;
    let f1 = e.step(g, &cone, &f0);
    e.constraints(g, &f1);
    let bad1 = e.any_bad(g, &f1);
    e.unsat(&[bad1], "safety in the second half-step")
}

/// Check an UNSAFE verdict at `depth` on `g` as loaded: find a trace
/// with this file's own encoding (initial states, constraints in every
/// frame, some bad output in frame `depth`), then simulate the solver's
/// inputs and free initial values through the AIG and confirm the bad
/// output. Engines and preprocessing are not trusted: a merge or encoding
/// bug that invents a counterexample makes this fail.
pub fn check_unsafe(
    g: &Aig,
    depth: usize,
    new_sat: &dyn Fn() -> Box<dyn Sat>,
    stop: &dyn Fn() -> bool,
) -> Result<(), String> {
    let cone = cone(g, &[]);
    let mut e = Enc::new(new_sat());
    let mut frames = vec![e.frame(g, &cone, None)];
    e.init(g, &cone, &frames[0]);
    for k in 0..=depth {
        if stop() {
            return Err("unsafe check stopped".into());
        }
        if k > 0 {
            let f = e.step(g, &cone, &frames[k - 1]);
            frames.push(f);
        }
        e.constraints(g, &frames[k]);
    }
    let bad = e.any_bad(g, &frames[depth]);
    match e.sat.solve(&[bad]) {
        Answer::Sat => {}
        Answer::Unsat => return Err(format!("unsafe fails: no counterexample of depth {depth}")),
        Answer::Unknown => return Err("unsafe check stopped".into()),
    }
    // Simulate the model's inputs and free initial values.
    let val = |e: &Enc, x: i32| -> bool {
        match x {
            0 | -1 => false,
            1 => true,
            x => e.sat.value(x),
        }
    };
    let trace = Trace {
        init: g
            .latches
            .iter()
            .map(|l| cone[var(l.lit) as usize] && val(&e, frames[0].0[var(l.lit) as usize]))
            .collect(),
        inputs: frames
            .iter()
            .map(|f| {
                g.inputs
                    .iter()
                    .map(|&i| cone[var(i) as usize] && val(&e, f.0[var(i) as usize]))
                    .collect()
            })
            .collect(),
    };
    replay_unsafe(g, depth, &trace)
}

/// A counterexample on the model as loaded: the value of every latch in
/// the initial state (used only for latches whose init is not constant)
/// and of every input in each frame, by index.
pub struct Trace {
    pub init: Vec<bool>,
    pub inputs: Vec<Vec<bool>>,
}

/// Simulate `trace` through `g` for frames 0..=depth: initial values and
/// constraints must hold, and some bad output in frame `depth`.
pub fn replay_unsafe(g: &Aig, depth: usize, trace: &Trace) -> Result<(), String> {
    if trace.inputs.len() <= depth || trace.init.len() != g.latches.len() {
        return Err("unsafe fails: trace has the wrong shape".into());
    }
    let index: HashMap<u32, usize> = g
        .latches
        .iter()
        .enumerate()
        .map(|(i, l)| (var(l.lit), i))
        .collect();
    let input_ix: HashMap<u32, usize> = g
        .inputs
        .iter()
        .enumerate()
        .map(|(i, &l)| (var(l), i))
        .collect();
    let mut state: Vec<bool> = g
        .latches
        .iter()
        .zip(&trace.init)
        .map(|(l, &b)| match l.init {
            Init::Zero => false,
            Init::One => true,
            _ => b,
        })
        .collect();
    let mut v = vec![false; g.nodes.len()];
    let lv = |v: &[bool], l: Lit| v[var(l) as usize] ^ is_neg(l);
    let eval = |v: &mut Vec<bool>, state: &[bool], k: usize| {
        for (n, node) in g.nodes.iter().enumerate() {
            v[n] = match *node {
                Node::Const => false,
                Node::Input => trace.inputs[k][input_ix[&(n as u32)]],
                Node::Latch => state[index[&(n as u32)]],
                Node::And(a, b) => lv(v, a) && lv(v, b),
            };
        }
    };
    let has_expr = g.latches.iter().any(|l| matches!(l.init, Init::Expr(_)));
    for k in 0..=depth {
        eval(&mut v, &state, k);
        if k == 0 && has_expr {
            // Inits given by an expression (Btor2) are whatever it says in
            // frame 0; the trace's values for them do not matter.
            for (i, l) in g.latches.iter().enumerate() {
                if let Init::Expr(x) = l.init {
                    state[i] = lv(&v, x);
                }
            }
            eval(&mut v, &state, k);
        }
        if g.constraints.iter().any(|&c| !lv(&v, c)) {
            return Err(format!(
                "unsafe fails: trace breaks a constraint in frame {k}"
            ));
        }
        if k == depth {
            break;
        }
        for (i, l) in g.latches.iter().enumerate() {
            state[i] = lv(&v, l.next);
        }
    }
    if g.bads.iter().any(|&b| lv(&v, b)) {
        Ok(())
    } else {
        Err("unsafe fails: simulated trace does not reach a bad state".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use informal_aig::neg;

    fn sat() -> Box<dyn Sat> {
        informal_sat::best()
    }

    /// Two latches that always toggle together from 0; bad = they differ.
    fn twins() -> Aig {
        let mut g = Aig::new();
        let a = g.latch(Init::Zero);
        let b = g.latch(Init::Zero);
        g.set_next(a, neg(a));
        g.set_next(b, neg(b));
        let bad = g.xor(a, b);
        g.bads.push(bad);
        g
    }

    #[test]
    fn accepts_an_inductive_invariant_and_rejects_wrong_ones() {
        let g = twins();
        let (a, b) = (g.latches[0].lit, g.latches[1].lit);
        // a == b.
        let good = vec![vec![neg(a), b], vec![a, neg(b)]];
        assert_eq!(check(&g, &good, &sat), Ok(()));
        // Not inductive: a is false holds initially but not after a step.
        assert_eq!(
            check(&g, &[vec![neg(a)], vec![neg(b)]], &sat),
            Err("consecution fails".into())
        );
        // Does not hold initially.
        assert_eq!(check(&g, &[vec![a]], &sat), Err("init fails".into()));
        // Inductive (true) but too weak to exclude the bad states.
        assert_eq!(check(&g, &[], &sat), Err("safety fails".into()));
    }

    #[test]
    fn lifts_merged_and_constant_latches_back() {
        let g = twins();
        // Preprocessing merged b into a, and IC3 needed no lemma: the
        // invariant is the merge itself.
        let mut p = Aig::new();
        let pa = p.latch(Init::Zero);
        p.set_next(pa, neg(pa));
        let map = vec![Some(pa), Some(pa)];
        let inv = lift(&g, &map, &p, &[]).unwrap();
        assert_eq!(check(&g, &inv, &sat), Ok(()));
        // A wrong merge (b == !a) is caught on the loaded model.
        let wrong = vec![Some(pa), Some(neg(pa))];
        let inv = lift(&g, &wrong, &p, &[]).unwrap();
        assert_eq!(check(&g, &inv, &sat), Err("init fails".into()));
        // A latch claimed constant that is not.
        let inv = lift(&g, &vec![Some(FALSE), None], &p, &[]).unwrap();
        assert_eq!(check(&g, &inv, &sat), Err("consecution fails".into()));
        // Lemma cubes are blocked: cube {pa} means clause !a.
        let inv = lift(&g, &vec![Some(pa), None], &p, &[vec![pa]]).unwrap();
        assert_eq!(inv, vec![vec![neg(g.latches[0].lit)]]);
    }

    /// a' = 0, b' = a, both from `a0` / 0; bad = b. With a0 = 0 the
    /// property is 2-inductive but not 1-inductive; with a0 = 1 it fails
    /// at depth 1.
    fn shift(a0: Init) -> Aig {
        let mut g = Aig::new();
        let a = g.latch(a0);
        let b = g.latch(Init::Zero);
        g.set_next(a, FALSE);
        g.set_next(b, a);
        g.bads.push(b);
        g
    }

    #[test]
    fn checks_k_induction_depth_base_and_step() {
        let never = || false;
        let g = shift(Init::Zero);
        assert_eq!(check_kind(&g, &[], 2, &sat, &never), Ok(()));
        assert_eq!(
            check_kind(&g, &[], 1, &sat, &never),
            Err("step fails".into())
        );
        let bad = shift(Init::One);
        assert_eq!(
            check_kind(&bad, &[], 2, &sat, &never),
            Err("base fails".into())
        );
        // A strengthening that is not inductive is caught first.
        let a = g.latches[0].lit;
        assert_eq!(
            check_kind(&g, &[vec![a]], 2, &sat, &never),
            Err("init fails".into())
        );
    }

    /// A 3-bit counter from 0, bad at 7 (reached at depth 7). Paths of
    /// three states that differ pairwise on the low bit alone do not
    /// exist, so a check that took "differ" from a caller who named only
    /// that bit would pass a 2-induction proof of a false property. The
    /// check computes the state itself and rejects it.
    #[test]
    fn k_induction_distinctness_covers_the_whole_cone() {
        let never = || false;
        let mut g = Aig::new();
        let b: Vec<Lit> = (0..3).map(|_| g.latch(Init::Zero)).collect();
        let c1 = g.and(b[0], b[1]);
        let x1 = {
            let n = g.and(neg(b[0]), neg(b[1]));
            g.and(neg(c1), neg(n))
        };
        let x2 = {
            let n = g.and(neg(c1), neg(b[2]));
            let both = g.and(c1, b[2]);
            g.and(neg(both), neg(n))
        };
        g.set_next(b[0], neg(b[0]));
        g.set_next(b[1], x1);
        g.set_next(b[2], x2);
        let top = g.and(c1, b[2]);
        g.bads.push(top);
        assert_eq!(
            check_kind(&g, &[], 2, &sat, &never),
            Err("step fails".into())
        );
        assert_eq!(
            check_kind(&g, &[], 7, &sat, &never),
            Err("step fails".into())
        );
        assert_eq!(
            check_kind(&g, &[], 8, &sat, &never),
            Err("base fails".into())
        );
    }

    #[test]
    fn checks_unsafe_depth_on_the_loaded_model() {
        let never = || false;
        // shift(One): b becomes 1 at frame 1.
        let g = shift(Init::One);
        assert_eq!(check_unsafe(&g, 1, &sat, &never), Ok(()));
        assert!(check_unsafe(&g, 0, &sat, &never)
            .unwrap_err()
            .starts_with("unsafe fails"));
        // shift(Zero) is safe: no depth works.
        let s = shift(Init::Zero);
        assert!(check_unsafe(&s, 3, &sat, &never)
            .unwrap_err()
            .starts_with("unsafe fails"));
    }

    /// clk toggles from 0; a holds the clock's last value.
    fn clocked(bad_on_high_phase: bool) -> Aig {
        let mut g = Aig::new();
        let clk = g.latch(Init::Zero);
        let a = g.latch(Init::Zero);
        g.set_next(clk, neg(clk));
        g.set_next(a, clk);
        let bad = if bad_on_high_phase {
            g.and(a, clk) // never: a is 0 whenever clk is 1
        } else {
            g.and(a, neg(clk)) // at step 2
        };
        g.bads.push(bad);
        g
    }

    #[test]
    fn folded_invariants_are_checked_over_two_steps() {
        let g = clocked(true);
        let clk = g.latches[0].lit;
        // "The clock is low" holds on the even phase only: fine as a
        // 2-step invariant, not as a 1-step one.
        let inv = vec![vec![neg(clk)]];
        assert_eq!(check_folded(&g, &inv, &sat), Ok(()));
        assert_eq!(check(&g, &inv, &sat), Err("consecution fails".into()));
        // The same invariant does not make the other property safe.
        let bad = clocked(false);
        assert_eq!(check_folded(&bad, &inv, &sat), Err("safety fails".into()));
        // Without the phase, the second half-step is not covered.
        assert_eq!(check_folded(&g, &[], &sat), Err("safety fails".into()));
        // OK stands for "the last half-step met the constraints": with no
        // constraints it is true, so a clause of OK alone holds and a
        // clause of its negation alone does not.
        assert_eq!(check_folded(&g, &[vec![neg(clk)], vec![OK]], &sat), Ok(()));
        assert_eq!(
            check_folded(&g, &[vec![neg(clk)], vec![OK ^ 1]], &sat),
            Err("init fails".into())
        );
    }
}
