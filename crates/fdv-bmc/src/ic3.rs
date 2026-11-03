//! IC3 / PDR (Bradley 2011; Een, Mishchenko, Brayton 2011), the first cut
//! of the stage-2 engine on rIC3's design: one solver per frame holding
//! the transition relation (the CNF template) and that frame's lemmas,
//! predecessor states shrunk by lifting with a separate solver, cubes
//! generalized from the unsat core and then by dropping literals (MIC),
//! and lemmas pushed forward after every round.
//!
//! It runs on the SAT interface (CaDiCaL today); the IC3-only solver of
//! the round-2 plan replaces it underneath without changing this file's
//! logic. Not yet: CTG / CTP generalization, EXCTG, DynAMic, lemma sharing.

use crate::cnf::{self, TLit, Template};
use fdv_aig::{is_neg, neg, var, Aig, Init, Lit};
use fdv_engine::{RunContext, Verdict};
use fdv_sat::{Answer, Sat};
use std::collections::BinaryHeap;

/// A cube over latch literals (AIG literals), sorted.
type Cube = Vec<Lit>;

/// One SAT solver over the template: SAT variable = template variable.
struct Solver {
    sat: Box<dyn Sat>,
    next: i32,
}

impl Solver {
    fn new(t: &Template, nvars: usize, mut sat: Box<dyn Sat>) -> Self {
        sat.reserve(nvars as i32 + 1);
        sat.add_clause(&[-1]); // template variable 1 is the AIG constant false
        for c in &t.clauses {
            sat.add_clause(c);
        }
        Solver {
            sat,
            next: nvars as i32 + 1,
        }
    }

    fn fresh(&mut self) -> i32 {
        self.sat.reserve(1);
        let v = self.next;
        self.next += 1;
        v
    }
}

struct Obligation {
    level: usize,
    /// Steps from this state to a bad state.
    depth: usize,
    cube: Cube,
}

impl PartialEq for Obligation {
    fn eq(&self, o: &Self) -> bool {
        self.level == o.level && self.depth == o.depth
    }
}
impl Eq for Obligation {}
impl PartialOrd for Obligation {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Obligation {
    // Lowest level first (BinaryHeap is a max-heap), then deepest.
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        o.level.cmp(&self.level).then(self.depth.cmp(&o.depth))
    }
}

pub struct Ic3<'g> {
    g: &'g Aig,
    t: Template,
    nvars: usize,
    /// solvers[i] holds F_i: init for i = 0, else lemmas of level >= i.
    solvers: Vec<Solver>,
    /// Lemmas by the highest frame they are known to hold in.
    frames: Vec<Vec<Cube>>,
    lift: Solver,
    bad: TLit,
    /// latch variable -> template literal of its next-state function
    next_of: std::collections::HashMap<u32, TLit>,
    make_sat: Box<dyn Fn() -> Box<dyn Sat>>,
    pub stats_lemmas: usize,
}

fn t(l: Lit) -> TLit {
    cnf::tlit(l)
}

impl<'g> Ic3<'g> {
    /// `None` when the model has inits IC3 cannot express here.
    pub fn new(g: &'g Aig, make_sat: Box<dyn Fn() -> Box<dyn Sat>>) -> Option<Self> {
        if g.latches.iter().any(|l| matches!(l.init, Init::Expr(_))) {
            return None;
        }
        let tpl = cnf::build(g);
        let nvars = g.nodes.len() + 1;
        let lift = Solver::new(&tpl, nvars, make_sat());
        // One literal for "some bad output": an extra variable when needed.
        let bad = if g.bads.len() == 1 {
            t(g.bads[0])
        } else {
            nvars as i32 + 1
        };
        let nvars = if g.bads.len() == 1 { nvars } else { nvars + 1 };
        let mut ic3 = Ic3 {
            g,
            t: tpl,
            nvars,
            solvers: Vec::new(),
            frames: Vec::new(),
            lift,
            bad,
            next_of: g.latches.iter().map(|l| (var(l.lit), t(l.next))).collect(),
            make_sat,
            stats_lemmas: 0,
        };
        // The lifting solver has no constraint units: lifting must keep
        // states that satisfy the constraints (see `lifted_state`).
        ic3.lift.next = ic3.nvars as i32 + 1;
        if g.bads.len() > 1 {
            let mut c = vec![-ic3.bad];
            c.extend(g.bads.iter().map(|&b| t(b)));
            ic3.lift.sat.add_clause(&c);
        }
        ic3.push_frame(); // F_0
        for l in &g.latches {
            match l.init {
                Init::Zero => ic3.solvers[0].sat.add_clause(&[-t(l.lit)]),
                Init::One => ic3.solvers[0].sat.add_clause(&[t(l.lit)]),
                _ => {}
            }
        }
        Some(ic3)
    }

    /// Constraints and the bad definition on top of the template.
    fn base_solver_from(&self, mut s: Solver) -> Solver {
        if s.next < self.nvars as i32 + 1 {
            s.next = self.nvars as i32 + 1;
        }
        for &c in &self.g.constraints {
            s.sat.add_clause(&[t(c)]);
        }
        if self.g.bads.len() > 1 {
            let mut c = vec![-self.bad];
            c.extend(self.g.bads.iter().map(|&b| t(b)));
            s.sat.add_clause(&c);
        }
        s
    }

    fn push_frame(&mut self) {
        // A new top frame has no lemmas yet: a lemma of level j lives in
        // solvers 1..=j only.
        let s = Solver::new(&self.t, self.nvars, (self.make_sat)());
        let s = self.base_solver_from(s);
        self.solvers.push(s);
        self.frames.push(Vec::new());
    }

    fn top(&self) -> usize {
        self.frames.len() - 1
    }

    fn intersects_init(&self, cube: &[Lit]) -> bool {
        cube.iter().all(|&l| {
            let latch = self.g.latches.iter().find(|x| x.lit == (l & !1)).unwrap();
            match latch.init {
                Init::Zero => is_neg(l),
                Init::One => !is_neg(l),
                _ => true,
            }
        })
    }

    fn add_lemma(&mut self, cube: Cube, level: usize) {
        let clause: Vec<i32> = cube.iter().map(|&l| -t(l)).collect();
        for i in 1..=level {
            self.solvers[i].sat.add_clause(&clause);
        }
        self.frames[level].push(cube);
        self.stats_lemmas += 1;
    }

    /// Latch and input values of solver `i`'s current model. Read it
    /// before adding clauses: that discards the model.
    fn model(&self, i: usize) -> (Cube, Vec<TLit>) {
        let g = self.g;
        let s = &self.solvers[i].sat;
        let latches = g
            .latches
            .iter()
            .map(|l| if s.value(t(l.lit)) { l.lit } else { neg(l.lit) })
            .collect();
        let inputs = g
            .inputs
            .iter()
            .map(|&x| if s.value(t(x)) { t(x) } else { -t(x) })
            .collect();
        (latches, inputs)
    }

    /// Shrink a state by lifting: the part of `latches` that, with these
    /// inputs, still forces every `target` literal (and the constraints).
    fn lift(&mut self, latches: Cube, inputs: &[TLit], target: &[TLit]) -> Cube {
        let g = self.g;
        // The target and the constraints hold in the model; ask for one
        // where some of them fail, with the same inputs and latches: UNSAT,
        // and its core names the latches that force them all. Every state of
        // the lifted cube then satisfies the constraints and reaches target.
        let act = self.lift.fresh();
        let mut clause = vec![-act];
        clause.extend(target.iter().map(|&x| -x));
        clause.extend(g.constraints.iter().map(|&c| -t(c)));
        self.lift.sat.add_clause(&clause);
        let mut assume = vec![act];
        assume.extend(inputs);
        assume.extend(latches.iter().map(|&l| t(l)));
        let r = self.lift.sat.solve(&assume);
        let cube = if r == Answer::Unsat {
            latches
                .iter()
                .copied()
                .filter(|&l| self.lift.sat.failed(t(l)))
                .collect()
        } else {
            latches // should not happen; keep the full state
        };
        self.lift.sat.add_clause(&[-act]);
        cube
    }

    fn primed(&self, cube: &[Lit]) -> Vec<TLit> {
        cube.iter()
            .map(|&l| {
                let n = self.next_of[&var(l)];
                if is_neg(l) {
                    -n
                } else {
                    n
                }
            })
            .collect()
    }

    /// Is `cube` blocked at level `i` relative to F_{i-1}: is
    /// F_{i-1} & !cube & T & cube' unsatisfiable? `Ok(core)`: yes, with the
    /// core-reduced cube (still disjoint from init). `Err(pred)`: no, with a
    /// lifted predecessor when `want_pred`.
    fn relative_inductive(
        &mut self,
        cube: &[Lit],
        i: usize,
        want_pred: bool,
    ) -> Result<Cube, Option<Cube>> {
        let primed = self.primed(cube);
        let s = &mut self.solvers[i - 1];
        let act = s.fresh();
        let mut clause = vec![-act];
        clause.extend(cube.iter().map(|&l| -t(l)));
        s.sat.add_clause(&clause);
        let mut assume = vec![act];
        assume.extend(&primed);
        let r = s.sat.solve(&assume);
        let out = match r {
            Answer::Unsat => Ok(cube
                .iter()
                .zip(&primed)
                .filter(|(_, &p)| s.sat.failed(p))
                .map(|(&l, _)| l)
                .collect::<Cube>()),
            Answer::Sat if want_pred => Err(Some(self.model(i - 1))),
            _ => Err(None),
        };
        self.solvers[i - 1].sat.add_clause(&[-act]);
        match out {
            Ok(core) => Ok(self.keep_out_of_init(core, cube)),
            Err(Some((latches, inputs))) => Err(Some(self.lift(latches, &inputs, &primed))),
            Err(None) => Err(None),
        }
    }

    /// Add back a literal of `orig` if `core` intersects init.
    fn keep_out_of_init(&self, mut core: Cube, orig: &[Lit]) -> Cube {
        if core.is_empty() || self.intersects_init(&core) {
            if let Some(&l) = orig.iter().find(|&&l| !self.intersects_init(&[l])) {
                core.push(l);
                core.sort();
                core.dedup();
            }
        }
        core
    }

    /// Drop literals one at a time while the cube stays blocked (MIC).
    fn generalize(&mut self, mut cube: Cube, i: usize, ctx: &RunContext) -> Cube {
        let mut idx = 0;
        while idx < cube.len() && !ctx.should_stop() {
            let mut cand = cube.clone();
            cand.remove(idx);
            if cand.is_empty() || self.intersects_init(&cand) {
                idx += 1;
                continue;
            }
            match self.relative_inductive(&cand, i, false) {
                Ok(core) if !self.intersects_init(&core) => cube = core,
                _ => idx += 1,
            }
        }
        cube
    }

    /// Block a bad cube found at the top frame; `Err(depth)` on a real
    /// counterexample, `Ok(false)` when stopped.
    fn block(&mut self, cube: Cube, ctx: &RunContext) -> Result<bool, usize> {
        let mut q = BinaryHeap::new();
        q.push(Obligation {
            level: self.top(),
            depth: 0,
            cube,
        });
        while let Some(ob) = q.pop() {
            if ctx.should_stop() {
                return Ok(false);
            }
            // Every state of an obligation's cube reaches a bad state, so a
            // cube that meets the initial states is a real counterexample.
            if ob.level == 0 || self.intersects_init(&ob.cube) {
                return Err(ob.depth);
            }
            if self.already_blocked(&ob.cube, ob.level) {
                if ob.level < self.top() {
                    q.push(Obligation {
                        level: ob.level + 1,
                        ..ob
                    });
                }
                continue;
            }
            match self.relative_inductive(&ob.cube, ob.level, true) {
                Ok(core) => {
                    let g = self.generalize(core, ob.level, ctx);
                    // Push the lemma as high as it stays inductive.
                    let mut lvl = ob.level;
                    while lvl < self.top() && self.relative_inductive(&g, lvl + 1, false).is_ok() {
                        lvl += 1;
                    }
                    self.add_lemma(g, lvl);
                    if lvl < self.top() {
                        q.push(Obligation {
                            level: lvl + 1,
                            ..ob
                        });
                    }
                }
                Err(pred) => {
                    // A predecessor in F_{level-1}.
                    let Some(pred) = pred else { return Ok(false) };
                    let (lvl, depth) = (ob.level, ob.depth);
                    q.push(ob);
                    q.push(Obligation {
                        level: lvl - 1,
                        depth: depth + 1,
                        cube: pred,
                    });
                }
            }
        }
        Ok(true)
    }

    /// Is the cube already excluded by some lemma at `level` or above?
    fn already_blocked(&self, cube: &[Lit], level: usize) -> bool {
        self.frames[level..]
            .iter()
            .flatten()
            .any(|lemma| lemma.iter().all(|l| cube.binary_search(l).is_ok()))
    }

    /// Push lemmas forward; true when two frames become equal (proof).
    fn propagate(&mut self, ctx: &RunContext) -> bool {
        for i in 1..self.top() {
            let lemmas = std::mem::take(&mut self.frames[i]);
            let mut stay = Vec::new();
            for c in lemmas {
                if ctx.should_stop() {
                    stay.push(c);
                    continue;
                }
                if self.relative_inductive(&c, i + 1, false).is_ok() {
                    let clause: Vec<i32> = c.iter().map(|&l| -t(l)).collect();
                    self.solvers[i + 1].sat.add_clause(&clause);
                    self.frames[i + 1].push(c);
                } else {
                    stay.push(c);
                }
            }
            self.frames[i] = stay;
            if self.frames[i].is_empty() {
                return true;
            }
        }
        false
    }

    pub fn check(&mut self, ctx: &RunContext) -> Verdict {
        // Depth 0: a bad initial state.
        match self.solvers[0].sat.solve(&[self.bad]) {
            Answer::Sat => return Verdict::Unsafe { depth: Some(0) },
            Answer::Unknown => {
                return Verdict::Unknown {
                    reason: "timeout".into(),
                }
            }
            Answer::Unsat => {}
        }
        self.push_frame(); // F_1
        loop {
            // Block every bad state in the top frame.
            loop {
                if ctx.should_stop() {
                    return Verdict::Unknown {
                        reason: "timeout".into(),
                    };
                }
                let k = self.top();
                match self.solvers[k].sat.solve(&[self.bad]) {
                    Answer::Unknown => {
                        return Verdict::Unknown {
                            reason: "timeout".into(),
                        }
                    }
                    Answer::Unsat => break,
                    Answer::Sat => {
                        let bad = self.bad;
                        let (latches, inputs) = self.model(k);
                        let cube = self.lift(latches, &inputs, &[bad]);
                        match self.block(cube, ctx) {
                            Err(depth) => {
                                return Verdict::Unsafe {
                                    depth: Some(depth as u32),
                                }
                            }
                            Ok(false) => {
                                return Verdict::Unknown {
                                    reason: "timeout".into(),
                                }
                            }
                            Ok(true) => {}
                        }
                    }
                }
            }
            self.push_frame();
            if self.propagate(ctx) {
                return Verdict::Safe;
            }
            if ctx.should_stop() {
                return Verdict::Unknown {
                    reason: "timeout".into(),
                };
            }
        }
    }

    pub fn depth(&self) -> usize {
        self.top()
    }
}
