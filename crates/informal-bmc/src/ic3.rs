//! IC3 / PDR (Bradley 2011; Een, Mishchenko, Brayton 2011), the first cut
//! of the stage-2 engine on rIC3's design: one solver per frame holding
//! the transition relation (the CNF template) and that frame's lemmas,
//! predecessor states shrunk by lifting with a separate solver, cubes
//! generalized from the unsat core and then by dropping literals (MIC),
//! and lemmas pushed forward after every round.
//!
//! It runs on the SAT interface (CaDiCaL today); the IC3-only solver of
//! the round-2 plan replaces it underneath without changing this file's
//! logic. Generalization uses CTGs. Several IC3s can share lemmas through
//! a [`LemmaPool`]: a lemma of frame j excludes no state reachable in j
//! steps, whichever IC3 found it, but it is only relatively inductive to
//! its finder's frames, so an importer re-checks it against its own
//! frames and adds it at the highest level where it holds there. Not yet: CTP, EXCTG, DynAMic.
//!
//! TODO(D7, Gemini P2): EXCTG and DynAMic generalization, an A-IC3 bandit
//! over generalization strategies, and stall-aware push budgets.
//! TODO(D7, Gemini P4): parent-lemma CTP prediction (CTP is off by
//! default today, on in one informal-ic3-mt worker).
//! TODO(D8, Gemini P1): certify every SAFE verdict: check the final frame
//! (init, consecution, safety) with independent CaDiCaL queries against
//! the un-preprocessed AIG, then Certifaiger.

use crate::cnf::{self, TLit, Template};
use informal_aig::{is_neg, neg, var, Aig, Init, Lit};
use informal_engine::{RunContext, Verdict};
use informal_sat::{Answer, Sat};
use std::collections::BinaryHeap;

pub static DIAG_LI: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static DIAG_LIVE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static DIAG_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How one IC3 generalizes; portfolio workers differ here.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// CTGs blocked per literal drop, and how deep CTG blocking recurses.
    pub max_ctgs: usize,
    pub max_ctg_depth: usize,
    /// Try dropping literals from the highest variable down.
    pub reverse_drop: bool,
    /// Counterexamples to propagation blocked per lemma when pushing it.
    pub max_ctps: usize,
    /// Close each query's decision domain over the frame's lemmas.
    pub lemma_domain: bool,
    /// Try dropping the literals that occur least in recent lemmas first
    /// (IC3ref / rIC3 ordering) instead of by variable index.
    pub activity_order: bool,
    /// Parent-lemma MIC (Xia et al., CAV'23): start from the lemma of
    /// level i-1 that covers the cube, keep its literals for last.
    pub parent_lemma: bool,
    /// Every this many frames (0: never), run Houdini on all lemmas and
    /// move the inductive subset to the invariant every frame assumes
    /// (hypothesis H2 in docs/ic3-research.md).
    /// H12: for an obligation at level i, first try to promote a lemma
    /// from a lower level that covers the obligation's cube.
    pub promote: bool,
    /// H13: derived predicates for lemma groups that differ in one
    /// literal (single-solver layout only).
    pub derive: bool,
    pub houdini_every: usize,
    /// H2b: instead, run Houdini when the lemmas in the frames have
    /// doubled since its last run (and number at least 100).
    pub houdini_grow: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            max_ctgs: std::env::var("INFORMAL_IC3_CTGS")
                .ok()
                .and_then(|x| x.parse().ok())
                .unwrap_or(3),
            max_ctg_depth: std::env::var("INFORMAL_IC3_CTG_DEPTH")
                .ok()
                .and_then(|x| x.parse().ok())
                .unwrap_or(1),
            reverse_drop: false,
            // Off: on the HWMCC'24 sample CTP lost more models than it
            // won (frames converge later); one informal-ic3-mt worker uses it.
            max_ctps: 0,
            lemma_domain: std::env::var_os("INFORMAL_IC3_LEMMA_DOMAIN").is_some(),
            // Off by default: on the 75-model set it lost more models than
            // it won; parent-lemma alone does better (INFORMAL_IC3_ACTIVITY on).
            activity_order: std::env::var_os("INFORMAL_IC3_ACTIVITY").is_some(),
            // Off by default: on 75 models it traded 3 models for 2 and
            // was slower; two informal-ic3-mt workers use it for diversity.
            parent_lemma: std::env::var_os("INFORMAL_IC3_PARENT").is_some(),
            promote: std::env::var("INFORMAL_IC3_PROMOTE").is_ok_and(|x| x != "0"),
            derive: std::env::var("INFORMAL_IC3_DERIVE").is_ok_and(|x| x != "0"),
            houdini_every: std::env::var("INFORMAL_IC3_HOUDINI")
                .ok()
                .and_then(|x| x.parse().ok())
                .unwrap_or(0),
            houdini_grow: std::env::var("INFORMAL_IC3_HOUDINI").is_ok_and(|x| x == "grow"),
        }
    }
}

/// Lemmas published by several IC3s: (cube, frame, publisher).
#[derive(Default)]
pub struct LemmaPool {
    lemmas: std::sync::Mutex<Vec<(Cube, usize, usize)>>,
    len: std::sync::atomic::AtomicUsize,
}

impl LemmaPool {
    fn publish(&self, cube: &Cube, level: usize, id: usize) {
        let mut v = self.lemmas.lock().unwrap();
        v.push((cube.clone(), level, id));
        self.len
            .store(v.len(), std::sync::atomic::Ordering::Release);
    }
}

struct Share {
    pool: std::sync::Arc<LemmaPool>,
    id: usize,
    seen: usize,
}

/// A cube over latch literals (AIG literals), sorted.
pub type Cube = Vec<Lit>;

/// One SAT solver over the template: SAT variable = template variable.
struct Solver {
    sat: Box<dyn Sat>,
    next: i32,
    /// An activation variable the solver handed back for reuse
    /// ([`Sat::retire`]).
    spare_act: Option<i32>,
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
            spare_act: None,
        }
    }

    /// Add `clause` for one query; returns the literal to assume.
    fn temp_clause(&mut self, clause: &[i32]) -> i32 {
        let act = match self.spare_act.take() {
            Some(a) => a,
            None => self.fresh(),
        };
        self.sat.add_temp_clause(clause, act);
        act
    }

    /// End the query of [`Solver::temp_clause`] (after reading its
    /// answer, core or model).
    fn retire(&mut self, act: i32) {
        if self.sat.retire(act) {
            self.spare_act = Some(act);
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
    t: std::sync::Arc<Template>,
    nvars: usize,
    /// solvers[i] holds F_i: init for i = 0, else lemmas of level >= i.
    /// With `single`, there is one solver for every frame instead: a lemma
    /// of level j is guarded by `acts[j]`, init by `acts[0]`, and a query
    /// on F_i assumes the guards of the levels it needs (GipSAT keeps all
    /// frames in one solver too). One copy of the transition relation per
    /// IC3 instead of one per frame. Default; `INFORMAL_IC3_PER_FRAME`
    /// restores the per-frame solvers.
    solvers: Vec<Solver>,
    single: bool,
    acts: Vec<i32>,
    /// Proved invariants given up front ([`Ic3::assume_invariant`]): in
    /// every frame >= 1 and part of the final invariant.
    inf: Vec<Cube>,
    /// H13: derived predicates. Predicate k is the OR of `aux[k]` (latch
    /// literals, sorted); as a cube literal it is the positive literal of
    /// AIG variable `g.nodes.len() + k`, which sorts after every latch.
    /// Its solver variables (now and next) lie in a block reserved at
    /// construction (`aux_cap` each); `next_of` maps it like a latch.
    aux: Vec<Vec<Lit>>,
    aux_index: std::collections::HashMap<Vec<Lit>, usize>,
    aux_cap: usize,
    /// By AIG variable: the predicates a latch is a member of. A query's
    /// decision domain takes in every predicate with a member in it (and
    /// with it the other members): the solver skips clauses that have an
    /// unassigned literal outside the domain, which is only right for
    /// variables that are free there, and a predicate is not.
    aux_of: Vec<Vec<u32>>,
    pub stats_aux: usize,
    pub stats_aux_merged: usize,
    pub stats_widen_tried: usize,
    pub stats_widened: usize,

    /// Lemmas in the frames right after the last Houdini run (H2b).
    pool_after_houdini: usize,

    /// Lemmas by the highest frame they are known to hold in.
    frames: Vec<Vec<Cube>>,
    /// The frame that emptied when the proof closed (see [`Ic3::invariant`]).
    proof_level: Option<usize>,
    lift: Solver,
    bad: TLit,
    /// latch variable -> template literal of its next-state function
    next_of: std::collections::HashMap<u32, TLit>,
    /// Initial value by AIG variable: Some(constant) for latches with a
    /// constant init, None otherwise (and for non-latches).
    init_of: Vec<Option<bool>>,
    make_sat: Box<dyn Fn() -> Box<dyn Sat>>,
    /// DFS marks for domain computation (epoch-stamped).
    mark: Vec<u32>,
    epoch: u32,
    cfg: Config,
    share: Option<Share>,
    /// Per AIG variable: how often it occurred in recent lemmas (decayed).
    act: Vec<f64>,
    act_inc: f64,
    /// INFORMAL_IC3_TRACE=<file>: one JSON line per lemma, push, frame
    /// and obligation event (diagnostics for generalization research).
    trace: Option<std::io::BufWriter<std::fs::File>>,
    trace_src: &'static str,
    /// CTG lemmas added during literal drops that succeeded / failed.
    /// H12: promotions tried and done.
    pub stats_promote_tried: usize,
    pub stats_promoted: usize,
    pub stats_ctg_ok: usize,
    pub stats_ctg_fail: usize,
    pub stats_lemmas: usize,
    pub stats_imported: usize,
    pub stats_ctps: usize,
    pub stats_lift_fallbacks: usize,
    /// MIC calls, total length of their results and inputs, and literal
    /// drops that succeeded / failed.
    pub stats_mic: usize,
    pub stats_parent_hits: usize,
    pub stats_mic_len: usize,
    pub stats_mic_in_len: usize,
    pub stats_drop_ok: usize,
    pub stats_drop_fail: usize,
    pub stats_queries: usize,
    pub stats_query_time: std::time::Duration,
    pub stats_lift_time: std::time::Duration,
}

/// The shared part of a lemma group, and per member its index in the
/// frame and the literal it adds.
type LemmaGroup = (Vec<Lit>, Vec<(usize, Lit)>);

/// Derived predicates one IC3 may define (H13).
const AUX_MAX: usize = 4096;

fn t(l: Lit) -> TLit {
    cnf::tlit(l)
}

impl<'g> Ic3<'g> {
    /// `None` when the model has inits IC3 cannot express here.
    pub fn new(g: &'g Aig, make_sat: Box<dyn Fn() -> Box<dyn Sat>>) -> Option<Self> {
        Self::with_config(g, make_sat, Config::default())
    }

    /// [`Ic3::new`] with a configuration (derived predicates need theirs
    /// at construction: they reserve solver variables).
    pub fn with_config(
        g: &'g Aig,
        make_sat: Box<dyn Fn() -> Box<dyn Sat>>,
        cfg: Config,
    ) -> Option<Self> {
        if g.latches.iter().any(|l| matches!(l.init, Init::Expr(_))) {
            return None;
        }
        let tpl = cnf::template(g);
        // H13 reserves solver variables for derived predicates (none when
        // the feature is off, so variable numbering stays as it was).
        let aux_cap = if cfg.derive { AUX_MAX } else { 0 };
        let nvars = g.nodes.len() + 1 + 2 * aux_cap;
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
            single: std::env::var_os("INFORMAL_IC3_PER_FRAME").is_none(),
            acts: Vec::new(),
            inf: Vec::new(),
            aux: Vec::new(),
            aux_index: std::collections::HashMap::new(),
            aux_cap,
            aux_of: if aux_cap > 0 {
                vec![Vec::new(); g.nodes.len()]
            } else {
                Vec::new()
            },
            stats_aux: 0,
            stats_aux_merged: 0,
            stats_widen_tried: 0,
            stats_widened: 0,
            pool_after_houdini: 0,

            frames: Vec::new(),
            proof_level: None,
            lift,
            bad,
            next_of: g.latches.iter().map(|l| (var(l.lit), t(l.next))).collect(),
            init_of: {
                let mut v = vec![None; g.nodes.len() + aux_cap];
                for l in &g.latches {
                    v[var(l.lit) as usize] = match l.init {
                        Init::Zero => Some(false),
                        Init::One => Some(true),
                        _ => None,
                    };
                }
                v
            },
            make_sat,
            mark: vec![0; nvars + 2],
            epoch: 0,
            cfg,
            share: None,
            act: vec![0.0; g.nodes.len() + aux_cap],
            act_inc: 1.0,
            trace: std::env::var_os("INFORMAL_IC3_TRACE")
                .and_then(|p| std::fs::File::create(p).ok())
                .map(std::io::BufWriter::new),
            trace_src: "po",
            stats_promote_tried: 0,
            stats_promoted: 0,
            stats_ctg_ok: 0,
            stats_ctg_fail: 0,
            stats_lemmas: 0,
            stats_imported: 0,
            stats_ctps: 0,
            stats_lift_fallbacks: 0,
            stats_mic: 0,
            stats_parent_hits: 0,
            stats_mic_len: 0,
            stats_mic_in_len: 0,
            stats_drop_ok: 0,
            stats_drop_fail: 0,
            stats_queries: 0,
            stats_query_time: Default::default(),
            stats_lift_time: Default::default(),
        };
        // The lifting solver has no constraint units: lifting must keep
        // states that satisfy the constraints (see `lifted_state`).
        ic3.lift.next = ic3.nvars as i32 + 1;
        if g.bads.len() > 1 {
            let mut c = vec![-ic3.bad];
            c.extend(g.bads.iter().map(|&b| t(b)));
            ic3.lift.sat.add_clause(&c);
            for &b in &g.bads {
                ic3.lift.sat.add_clause(&[ic3.bad, -t(b)]);
            }
        }
        ic3.push_frame(); // F_0
        for l in &g.latches {
            match l.init {
                Init::Zero => ic3.add_frame_clause(0, &[-t(l.lit)]),
                Init::One => ic3.add_frame_clause(0, &[t(l.lit)]),
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
            // And back, so that -bad means no bad output (H1 in the
            // feedback report: the one-sided OR only cost completeness).
            for &b in &self.g.bads {
                s.sat.add_clause(&[self.bad, -t(b)]);
            }
        }
        s
    }

    fn push_frame(&mut self) {
        // A new top frame has no lemmas yet: a lemma of level j lives in
        // solvers 1..=j only.
        if !self.single || self.solvers.is_empty() {
            let s = Solver::new(&self.t, self.nvars, (self.make_sat)());
            let mut s = self.base_solver_from(s);
            if !self.solvers.is_empty() {
                for c in &self.inf {
                    let clause: Vec<i32> = c.iter().map(|&l| -t(l)).collect();
                    s.sat.add_clause(&clause);
                }
            }
            self.solvers.push(s);
        }
        if self.single {
            let a = self.solvers[0].fresh();
            self.acts.push(a);
        }
        self.frames.push(Vec::new());
    }

    /// The solver that answers queries on F_i.
    fn sol(&mut self, i: usize) -> &mut Solver {
        let k = if self.single { 0 } else { i };
        &mut self.solvers[k]
    }

    /// Assumptions that make the solver of F_i hold exactly F_i.
    fn frame_assumptions(&self, i: usize) -> Vec<i32> {
        match (self.single, i) {
            (false, _) => Vec::new(),
            (true, 0) => vec![self.acts[0]],
            (true, _) => self.acts[i..].to_vec(),
        }
    }

    /// Add `clause` to frame `level` only (level 0: init) — in the
    /// per-frame layout, to that frame's solver; in the single layout,
    /// guarded by the level's activation literal.
    fn add_frame_clause(&mut self, level: usize, clause: &[i32]) {
        if self.single {
            let mut c = clause.to_vec();
            c.push(-self.acts[level]);
            // One watch-list group per level: a query on F_i skips the
            // lemmas of lower levels (their guards are not assumed).
            self.solvers[0].sat.add_clause_grouped(&c, level as u32);
        } else {
            self.solvers[level].sat.add_clause(clause);
        }
    }

    fn top(&self) -> usize {
        self.frames.len() - 1
    }

    fn intersects_init(&self, cube: &[Lit]) -> bool {
        // O(cube) with the init table; it was a scan of every latch per
        // literal (H4 in the feedback report).
        cube.iter().all(|&l| match self.init_of[var(l) as usize] {
            Some(v) => v != is_neg(l),
            None => true,
        })
    }

    pub fn set_config(&mut self, cfg: Config) {
        self.cfg = cfg;
    }

    /// Publish this IC3's lemmas to `pool` as `id` and take the others'.
    pub fn share(&mut self, pool: std::sync::Arc<LemmaPool>, id: usize) {
        self.share = Some(Share { pool, id, seen: 0 });
    }

    fn publish(&self, cube: &Cube, level: usize) {
        if let Some(sh) = &self.share {
            // Other IC3s do not know this one's derived predicates.
            for c in self.expand(cube) {
                sh.pool.publish(&c, level, sh.id);
            }
        }
    }

    /// True for the cube literal of a derived predicate.
    fn is_aux(&self, l: Lit) -> bool {
        var(l) as usize >= self.g.nodes.len()
    }

    /// Does `lemma` (a blocked cube, possibly with derived predicates)
    /// exclude every state of `cube` (latch literals only)?
    fn covers(&self, lemma: &[Lit], cube: &[Lit]) -> bool {
        lemma.iter().all(|&l| {
            if self.is_aux(l) {
                let k = var(l) as usize - self.g.nodes.len();
                self.aux[k].iter().any(|m| cube.binary_search(m).is_ok())
            } else {
                cube.binary_search(&l).is_ok()
            }
        })
    }

    /// `cube` over latch literals only: one cube per member of its
    /// derived predicate (lemmas hold at most one). Together they block
    /// exactly the same states.
    fn expand(&self, cube: &Cube) -> Vec<Cube> {
        let Some(pos) = cube.iter().position(|&l| self.is_aux(l)) else {
            return vec![cube.clone()];
        };
        let k = var(cube[pos]) as usize - self.g.nodes.len();
        let rest: Vec<Lit> = cube.iter().copied().filter(|&l| !self.is_aux(l)).collect();
        self.aux[k]
            .iter()
            .filter(|&&m| rest.binary_search(&neg(m)).is_err())
            .map(|&m| {
                let mut c = rest.clone();
                if let Err(i) = c.binary_search(&m) {
                    c.insert(i, m);
                }
                c
            })
            .collect()
    }

    /// The cube literal of the predicate "some literal of `members`
    /// holds", defining it in the solvers the first time. `None` when
    /// the reserved block is full.
    fn define_aux(&mut self, members: Vec<Lit>) -> Option<Lit> {
        let n = self.g.nodes.len();
        if let Some(&k) = self.aux_index.get(&members) {
            return Some(2 * (n + k) as Lit);
        }
        if self.aux.len() >= self.aux_cap {
            return None;
        }
        let k = self.aux.len();
        let lit = 2 * (n + k) as Lit;
        let x = t(lit);
        let xp = (n + 1 + self.aux_cap + k) as TLit;
        let now: Vec<TLit> = members.iter().map(|&m| t(m)).collect();
        let next: Vec<TLit> = self.primed(&members);
        for s in [&mut self.solvers[0], &mut self.lift] {
            for (v, ms) in [(x, &now), (xp, &next)] {
                let mut or = vec![-v];
                or.extend(ms.iter());
                s.sat.add_clause(&or);
                for &m in ms.iter() {
                    s.sat.add_clause(&[v, -m]);
                }
            }
        }
        self.next_of.insert(var(lit), xp);
        // Initially: true if some member is, false if none can be.
        let inits: Vec<Option<bool>> = members
            .iter()
            .map(|&m| self.init_of[var(m) as usize].map(|v| v != is_neg(m)))
            .collect();
        self.init_of[n + k] = if inits.contains(&Some(true)) {
            Some(true)
        } else if inits.iter().all(|i| *i == Some(false)) {
            Some(false)
        } else {
            None
        };
        for &m in &members {
            self.aux_of[var(m) as usize].push(k as u32);
        }
        self.aux_index.insert(members.clone(), k);
        self.aux.push(members);
        self.stats_aux += 1;
        Some(lit)
    }

    /// H13, stage A: in each frame, lemmas that are equal except for one
    /// literal (4 or more of them) become one lemma over a derived
    /// predicate. The new clause says exactly what the group said, so
    /// nothing changes logically; the group is then pushed as one.
    fn derive_groups(&mut self) {
        for lvl in 1..=self.top() {
            let lemmas = &self.frames[lvl];
            let mut groups: std::collections::HashMap<Vec<Lit>, Vec<(usize, Lit)>> =
                std::collections::HashMap::new();
            for (i, c) in lemmas.iter().enumerate() {
                if c.len() < 2 || c.iter().any(|&l| self.is_aux(l)) {
                    continue;
                }
                for k in 0..c.len() {
                    let mut rest = c.clone();
                    let m = rest.remove(k);
                    groups.entry(rest).or_default().push((i, m));
                }
            }
            let mut big: Vec<LemmaGroup> =
                groups.into_iter().filter(|(_, v)| v.len() >= 4).collect();
            if big.is_empty() {
                continue;
            }
            // Largest first; ties by the shared part, for a fixed order.
            big.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
            let mut used = vec![false; lemmas.len()];
            let mut merged: Vec<Cube> = Vec::new();
            for (rest, members) in big {
                let free: Vec<(usize, Lit)> =
                    members.into_iter().filter(|&(i, _)| !used[i]).collect();
                if free.len() < 4 {
                    continue;
                }
                let mut ms: Vec<Lit> = free.iter().map(|&(_, m)| m).collect();
                ms.sort_unstable();
                ms.dedup();
                let Some(x) = self.define_aux(ms) else { break };
                for &(i, _) in &free {
                    used[i] = true;
                }
                self.stats_aux_merged += free.len();
                let mut cube = rest;
                cube.push(x); // sorts after every latch literal
                merged.push(cube);
            }
            if merged.is_empty() {
                continue;
            }
            let old = std::mem::take(&mut self.frames[lvl]);
            self.frames[lvl] = old
                .into_iter()
                .zip(used)
                .filter_map(|(c, u)| (!u).then_some(c))
                .collect();
            for cube in merged {
                let clause: Vec<i32> = cube.iter().map(|&l| -t(l)).collect();
                self.add_frame_clause(lvl, &clause);
                self.frames[lvl].push(cube);
            }
        }
    }

    /// Add the other IC3s' new lemmas, each at its frame or our top.
    fn import(&mut self, ctx: &RunContext) {
        let Some(sh) = &mut self.share else { return };
        if sh.pool.len.load(std::sync::atomic::Ordering::Acquire) == sh.seen {
            return;
        }
        let new: Vec<(Cube, usize)> = {
            let v = sh.pool.lemmas.lock().unwrap();
            let new = v[sh.seen..]
                .iter()
                .filter(|(_, _, id)| *id != sh.id)
                .map(|(c, l, _)| (c.clone(), *l))
                .collect();
            sh.seen = v.len();
            new
        };
        // A lemma is only relatively inductive to its publisher's frames.
        // The convergence proof needs every lemma of level L to be
        // inductive relative to *our* F_{L-1}, so each import is
        // re-checked here and added at the highest level where it is.
        // (Unchecked imports gave wrong "safe" verdicts on a03/a11.)
        for (c, l) in new {
            if ctx.should_stop() {
                return;
            }
            let mut lvl = l.min(self.top());
            while lvl >= 1 && !self.already_blocked(&c, lvl) {
                if self.relative_inductive(&c, lvl, false).is_ok() {
                    self.trace_src = "import";
                    self.add_lemma_quiet(c, lvl);
                    self.trace_src = "po";
                    self.stats_imported += 1;
                    break;
                }
                lvl -= 1;
            }
        }
    }

    fn add_lemma(&mut self, cube: Cube, level: usize) {
        self.publish(&cube, level);
        self.add_lemma_quiet(cube, level);
    }

    fn tr(&mut self, line: std::fmt::Arguments) {
        if let Some(w) = &mut self.trace {
            use std::io::Write;
            let _ = writeln!(w, "{line}");
        }
    }

    fn add_lemma_quiet(&mut self, cube: Cube, level: usize) {
        if self.trace.is_some() {
            // Lemmas the new one subsumes (its cube is a subset of theirs).
            let sub = self.frames[1..=level]
                .iter()
                .flatten()
                .filter(|o| cube.iter().all(|l| o.binary_search(l).is_ok()))
                .count();
            let (src, top, q) = (self.trace_src, self.top(), self.stats_queries);
            self.tr(format_args!(
                "{{\"e\":\"L\",\"src\":\"{src}\",\"lvl\":{level},\"top\":{top},\"q\":{q},\"sub\":{sub},\"lits\":{cube:?}}}"
            ));
        }
        for &l in &cube {
            self.act[var(l) as usize] += self.act_inc;
        }
        self.act_inc *= 1.0 / 0.99;
        if self.act_inc > 1e100 {
            for a in &mut self.act {
                *a *= 1e-100;
            }
            self.act_inc *= 1e-100;
        }
        let clause: Vec<i32> = cube.iter().map(|&l| -t(l)).collect();
        if self.single {
            self.add_frame_clause(level, &clause);
        } else {
            for i in 1..=level {
                self.solvers[i].sat.add_clause(&clause);
            }
        }
        self.frames[level].push(cube);
        self.stats_lemmas += 1;
    }

    /// Latch and input values of solver `i`'s current model. Read it
    /// before adding clauses: that discards the model.
    fn model(&self, i: usize) -> (Cube, Vec<TLit>) {
        let g = self.g;
        let s = &self.solvers[if self.single { 0 } else { i }].sat;
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
    /// Template variables (by index) that justify `target` and the
    /// constraints under the given latch and input values: an AND that is
    /// true needs both inputs, a false one needs one false input (one
    /// already needed if possible). Values come from simulating the AIG.
    fn justify(&self, latches: &[Lit], inputs: &[TLit], target: &[TLit]) -> Vec<bool> {
        let g = self.g;
        let n = g.nodes.len();
        let mut val = vec![false; n];
        for &l in latches {
            val[var(l) as usize] = !is_neg(l);
        }
        for &x in inputs {
            val[x.unsigned_abs() as usize - 1] = x > 0;
        }
        let lv = |val: &[bool], l: Lit| val[var(l) as usize] ^ is_neg(l);
        for v in 0..n {
            if let informal_aig::Node::And(a, b) = g.nodes[v] {
                val[v] = lv(&val, a) && lv(&val, b);
            }
        }
        let mut need = vec![false; self.nvars + 2];
        // Roots: the target (or the bads that hold, for the extra bad
        // variable) and every constraint.
        let mut roots: Vec<u32> = Vec::new();
        for &x in target {
            let v = x.unsigned_abs() as usize;
            if v - 1 < n {
                roots.push(v as u32 - 1);
            } else if v > n + self.aux_cap && v - n - 1 - self.aux_cap < self.aux.len() {
                // The next-state copy of a derived predicate (an OR): one
                // member that holds justifies it true, all of them false.
                let nexts: Vec<Lit> = self.aux[v - n - 1 - self.aux_cap]
                    .iter()
                    .map(|&m| {
                        let nx = self.next_of[&var(m)];
                        let l = 2 * (nx.unsigned_abs() - 1) + u32::from(nx < 0);
                        if is_neg(m) {
                            neg(l)
                        } else {
                            l
                        }
                    })
                    .collect();
                if x > 0 {
                    let pick = nexts.iter().find(|&&l| lv(&val, l)).or(nexts.first());
                    roots.extend(pick.map(|&l| var(l)));
                } else {
                    roots.extend(nexts.iter().map(|&l| var(l)));
                }
            } else {
                roots.extend(g.bads.iter().filter(|&&b| lv(&val, b)).map(|&b| var(b)));
            }
        }
        roots.extend(g.constraints.iter().map(|&c| var(c)));
        let mut mark = vec![false; n];
        for r in roots {
            mark[r as usize] = true;
        }
        for v in (0..n).rev() {
            if !mark[v] {
                continue;
            }
            need[v + 1] = true;
            if let informal_aig::Node::And(a, b) = g.nodes[v] {
                let (va, vb) = (var(a) as usize, var(b) as usize);
                if val[v] {
                    mark[va] = true;
                    mark[vb] = true;
                } else {
                    let fa = !lv(&val, a);
                    let fb = !lv(&val, b);
                    // One controlling (false) input is enough.
                    let pick = match (fa, fb) {
                        (true, true) if mark[vb] && !mark[va] => vb,
                        (true, _) => va,
                        _ => vb,
                    };
                    mark[pick] = true;
                }
            }
        }
        need
    }

    fn lift(&mut self, latches: Cube, inputs: &[TLit], target: &[TLit]) -> Cube {
        let g = self.g;
        // The target and the constraints hold in the model; ask for one
        // where some of them fail, with the same inputs and latches: UNSAT,
        // and its core names the latches that force them all. Every state of
        // the lifted cube then satisfies the constraints and reaches target.
        // Only latches and inputs in the cone of the target (and the
        // constraints) can matter; the rest are left out of the query.
        let roots: Vec<TLit> = target.to_vec();
        let dom = self.domain(&roots, 0);
        // Justification first: only latches and inputs that some path of
        // controlling values needs go to the SAT core below.
        let need = self.justify(&latches, inputs, target);
        let in_cone = |x: TLit| need[x.unsigned_abs() as usize];
        let all = latches.clone();
        let latches: Cube = latches.into_iter().filter(|&l| in_cone(t(l))).collect();
        let mut clause: Vec<i32> = target.iter().map(|&x| -x).collect();
        clause.extend(g.constraints.iter().map(|&c| -t(c)));
        let act = self.lift.temp_clause(&clause);
        let mut assume = vec![act];
        assume.extend(inputs.iter().copied().filter(|&x| in_cone(x)));
        assume.extend(latches.iter().map(|&l| t(l)));
        let t0 = std::time::Instant::now();
        let r = self.lift.sat.solve_domain(&assume, &dom);
        self.stats_lift_time += t0.elapsed();
        let cube = if r == Answer::Unsat {
            latches
                .iter()
                .copied()
                .filter(|&l| self.lift.sat.failed(t(l)))
                .collect()
        } else {
            // Justification missed something (or the solver stopped):
            // keep the full state, which always reaches the target.
            self.stats_lift_fallbacks += 1;
            all
        };
        self.lift.retire(act);
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

    /// Variables a query needs decisions on (GipSAT): the cone of `roots`
    /// and of the constraints through the AND gates, closed under the
    /// lemmas of frames `>= level` that touch it. Everything outside can be
    /// completed afterwards: gates by evaluation, and the remaining latches
    /// by an initial state, which every lemma of a frame >= 1 admits.
    fn domain(&mut self, roots: &[TLit], level: usize) -> Vec<TLit> {
        self.epoch += 1;
        let ep = self.epoch;
        let mut out = Vec::new();
        let mut stack: Vec<u32> = roots
            .iter()
            .map(|r| r.unsigned_abs())
            .chain(self.g.constraints.iter().map(|&c| var(c) + 1))
            .collect();
        if self.g.bads.len() > 1 && roots.contains(&self.bad) {
            stack.extend(self.g.bads.iter().map(|&b| var(b) + 1));
        }
        let grow = |stack: &mut Vec<u32>, mark: &mut Vec<u32>, out: &mut Vec<TLit>| {
            while let Some(tv) = stack.pop() {
                let tv = tv as usize;
                if tv >= mark.len() || mark[tv] == ep {
                    continue;
                }
                mark[tv] = ep;
                out.push(tv as TLit);
                let n = self.g.nodes.len();
                if tv >= 1 && tv - 1 < n {
                    if let informal_aig::Node::And(a, b) = self.g.nodes[tv - 1] {
                        stack.push(var(a) + 1);
                        stack.push(var(b) + 1);
                    }
                    if let Some(ks) = self.aux_of.get(tv - 1) {
                        stack.extend(ks.iter().map(|&k| n as u32 + 1 + k));
                    }
                } else if tv > n && tv - n - 1 < self.aux.len() {
                    // A derived predicate: its members.
                    stack.extend(self.aux[tv - n - 1].iter().map(|&m| var(m) + 1));
                } else if tv > n + self.aux_cap && tv - n - 1 - self.aux_cap < self.aux.len() {
                    // Its next-state copy: the members' next-state functions.
                    let k = tv - n - 1 - self.aux_cap;
                    stack.extend(
                        self.aux[k]
                            .iter()
                            .map(|&m| self.next_of[&var(m)].unsigned_abs()),
                    );
                }
            }
        };
        grow(&mut stack, &mut self.mark, &mut out);
        // Closing over lemmas keeps predecessors inside F_i; without it a
        // predecessor may break a lemma outside the cone. Both are sound:
        // UNSAT on part of the clauses is UNSAT, the cone holds the whole
        // transition into the cube, and init units always propagate.
        if level >= 1 && self.cfg.lemma_domain {
            loop {
                let mut added = false;
                for lemma in self.frames[level..].iter().flatten() {
                    let touches = lemma.iter().any(|&l| self.mark[var(l) as usize + 1] == ep);
                    if touches {
                        for &l in lemma {
                            let tv = var(l) + 1;
                            if self.mark[tv as usize] != ep {
                                stack.push(tv);
                                added = true;
                            }
                        }
                    }
                }
                if !added {
                    break;
                }
                grow(&mut stack, &mut self.mark, &mut out);
            }
        }
        // Variables the CNF template eliminated are in no clause: deciding
        // them is pure waste (they were over half of each domain on the
        // cv32e40x models).
        let live = &self.t.live;
        out.retain(|&tv| live.get(tv as usize).copied().unwrap_or(true));
        // Latches and inputs first: Mini decides in this order, and once
        // they are set the gates mostly follow by propagation.
        let nodes = &self.g.nodes;
        out.sort_by_key(|&tv| {
            let v = tv as usize;
            !(v >= 1
                && v - 1 < nodes.len()
                && matches!(
                    nodes[v - 1],
                    informal_aig::Node::Latch | informal_aig::Node::Input
                ))
        });
        if std::env::var_os("INFORMAL_DIAG_DOM").is_some() {
            let li = out
                .iter()
                .filter(|&&tv| {
                    let v = tv as usize;
                    v >= 1
                        && v - 1 < nodes.len()
                        && matches!(
                            nodes[v - 1],
                            informal_aig::Node::Latch | informal_aig::Node::Input
                        )
                })
                .count();
            let live = out
                .iter()
                .filter(|&&tv| self.t.live.get(tv as usize).copied().unwrap_or(true))
                .count();
            DIAG_LI.fetch_add(li as u64, std::sync::atomic::Ordering::Relaxed);
            DIAG_LIVE.fetch_add(live as u64, std::sync::atomic::Ordering::Relaxed);
            DIAG_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        out
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
        let roots: Vec<TLit> = primed
            .iter()
            .copied()
            .chain(cube.iter().map(|&l| t(l)))
            .collect();
        let dom = self.domain(&roots, i - 1);
        let mut assume = self.frame_assumptions(i - 1);
        let k = if self.single { 0 } else { i - 1 };
        let s = &mut self.solvers[k];
        if self.single {
            // F_0 is init alone (group 0); F_j is the lemmas from j up.
            s.sat
                .set_active_groups_from(if i == 1 { 0 } else { (i - 1) as u32 });
        }
        let clause: Vec<i32> = cube.iter().map(|&l| -t(l)).collect();
        let act = s.temp_clause(&clause);
        assume.push(act);
        assume.extend(&primed);
        let t0 = std::time::Instant::now();
        let r = s.sat.solve_domain(&assume, &dom);
        self.stats_queries += 1;
        self.stats_query_time += t0.elapsed();
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
        self.sol(i - 1).retire(act);
        match out {
            Ok(core) => Ok(self.keep_out_of_init(core, cube)),
            Err(Some((latches, inputs))) => Err(Some(self.lift(latches, &inputs, &primed))),
            Err(None) => Err(None),
        }
    }

    /// H12: a lemma below `level` whose literals all occur in `cube` and
    /// that is inductive at `level` now is moved up (as far as it holds).
    /// At most 3 candidates, highest level first. True when one moved.
    fn promote_for(&mut self, cube: &[Lit], level: usize) -> bool {
        let mut cands: Vec<(usize, Cube)> = Vec::new();
        'levels: for j in (1..level).rev() {
            for l in &self.frames[j] {
                if l.len() <= cube.len()
                    && !l.iter().any(|&x| self.is_aux(x))
                    && self.covers(l, cube)
                {
                    cands.push((j, l.clone()));
                    if cands.len() == 3 {
                        break 'levels;
                    }
                }
            }
        }
        for (j, l) in cands {
            self.stats_promote_tried += 1;
            if self.relative_inductive(&l, level, false).is_err() {
                continue;
            }
            let mut lvl = level;
            while lvl < self.top() && self.relative_inductive(&l, lvl + 1, false).is_ok() {
                lvl += 1;
            }
            if let Some(pos) = self.frames[j].iter().position(|x| *x == l) {
                self.frames[j].swap_remove(pos);
            }
            self.stats_promoted += 1;
            self.trace_src = "promote";
            self.add_lemma(l, lvl);
            self.trace_src = "po";
            return true;
        }
        false
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

    /// Drop literals one at a time while the cube stays blocked (MIC),
    /// using counterexamples to generalization (CTG, Hassan, Bradley and
    /// Somenzi 2013) to make room: a predecessor that blocks a drop is
    /// itself blocked one level down when it can be, then the drop is
    /// retried; otherwise the candidate shrinks to what it shares with it.
    fn generalize(&mut self, mut cube: Cube, i: usize, depth: usize, ctx: &RunContext) -> Cube {
        let mut order = cube.clone();
        if self.cfg.activity_order {
            // Least active first; reverse_drop breaks ties the other way.
            let act = &self.act;
            if self.cfg.reverse_drop {
                order.reverse();
            }
            order.sort_by(|&a, &b| act[var(a) as usize].total_cmp(&act[var(b) as usize]));
        } else if self.cfg.reverse_drop {
            order.reverse();
        }
        if depth == 0 {
            self.stats_mic += 1;
            self.stats_mic_in_len += cube.len();
        }
        // Parent lemma: a lemma of level i-1 whose literals all occur in
        // the cube. If it holds at level i too, it is the result (already
        // general); otherwise its literals are tried last.
        let mut parent: Option<Cube> = None;
        if self.cfg.parent_lemma && depth == 0 && i >= 2 {
            parent = self.frames[i - 1]
                .iter()
                .filter(|p| p.iter().all(|l| cube.binary_search(l).is_ok()))
                .min_by_key(|p| p.len())
                .cloned();
            if let Some(p) = &parent {
                if p.len() < cube.len() {
                    if let Ok(core) = self.relative_inductive(p, i, false) {
                        self.stats_parent_hits += 1;
                        self.stats_mic_len += core.len();
                        return core;
                    }
                }
            }
        }
        if let Some(p) = &parent {
            // Stable: literals outside the parent first.
            order.sort_by_key(|l| p.binary_search(l).is_ok());
        }
        for l in order {
            if ctx.should_stop() {
                break;
            }
            let Ok(pos) = cube.binary_search(&l) else {
                continue; // already gone with an earlier core
            };
            let mut cand = cube.clone();
            cand.remove(pos);
            if let Some(core) = self.ctg_down(cand, i, depth, ctx) {
                cube = core;
                self.stats_drop_ok += 1;
            } else {
                self.stats_drop_fail += 1;
            }
        }
        if depth == 0 {
            self.stats_mic_len += cube.len();
            if self.aux_cap > 0 && self.cfg.derive && !self.aux.is_empty() {
                cube = self.widen_with_predicate(cube, i);
            }
        }
        cube
    }

    /// H13, stage B: replace one literal of a finished lemma by a derived
    /// predicate it is a member of, if the wider cube is still blocked at
    /// level `i` (and still misses the initial states). The one lemma then
    /// covers every member, so the others never need an obligation of
    /// their own. Largest predicates first, at most 2 queries.
    fn widen_with_predicate(&mut self, cube: Cube, i: usize) -> Cube {
        if cube.iter().any(|&l| self.is_aux(l)) {
            return cube;
        }
        let n = self.g.nodes.len();
        let mut cands: Vec<(usize, usize, usize)> = Vec::new(); // (size, k, position)
        for (pos, &l) in cube.iter().enumerate() {
            for &k in self.aux_of.get(var(l) as usize).into_iter().flatten() {
                let ms = &self.aux[k as usize];
                // The literal itself must be a member (same polarity), and
                // no other cube literal may contradict or repeat one.
                if ms.binary_search(&l).is_ok()
                    && !cube.iter().any(|&o| {
                        o != l
                            && (ms.binary_search(&o).is_ok() || ms.binary_search(&neg(o)).is_ok())
                    })
                {
                    cands.push((ms.len(), k as usize, pos));
                }
            }
        }
        cands.sort_by(|a, b| b.cmp(a));
        for &(_, k, pos) in cands.iter().take(2) {
            let mut wide = cube.clone();
            wide.remove(pos);
            wide.push(2 * (n + k) as Lit);
            self.stats_widen_tried += 1;
            if self.intersects_init(&wide) {
                continue;
            }
            if let Ok(core) = self.relative_inductive(&wide, i, false) {
                // Only a core that kept the predicate is a gain.
                if core.iter().any(|&l| self.is_aux(l)) {
                    self.stats_widened += 1;
                    return core;
                }
            }
        }
        cube
    }

    fn ctg_down(
        &mut self,
        mut cube: Cube,
        i: usize,
        depth: usize,
        ctx: &RunContext,
    ) -> Option<Cube> {
        let mut ctgs = 0;
        let mut added = 0;
        let r = loop {
            if cube.is_empty() || self.intersects_init(&cube) || ctx.should_stop() {
                break None;
            }
            match self.relative_inductive(&cube, i, true) {
                Ok(core) => break (!self.intersects_init(&core)).then_some(core),
                Err(None) => break None,
                Err(Some(ctg)) => {
                    if depth < self.cfg.max_ctg_depth
                        && ctgs < self.cfg.max_ctgs
                        && i > 1
                        && !self.intersects_init(&ctg)
                    {
                        if let Ok(core) = self.relative_inductive(&ctg, i - 1, false) {
                            ctgs += 1;
                            added += 1;
                            let g = self.generalize(core, i - 1, depth + 1, ctx);
                            let mut lvl = i - 1;
                            while lvl < self.top()
                                && self.relative_inductive(&g, lvl + 1, false).is_ok()
                            {
                                lvl += 1;
                            }
                            self.trace_src = "ctg";
                            self.add_lemma(g, lvl);
                            self.trace_src = "po";
                            continue;
                        }
                    }
                    // Join: keep only the literals the CTG state shares.
                    ctgs = 0;
                    let joined: Cube = cube
                        .iter()
                        .copied()
                        .filter(|l| ctg.binary_search(l).is_ok())
                        .collect();
                    if joined.len() == cube.len() {
                        break None;
                    }
                    cube = joined;
                }
            }
        };
        if r.is_some() {
            self.stats_ctg_ok += added;
        } else {
            self.stats_ctg_fail += added;
        }
        r
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
            self.import(ctx);
            if self.trace.is_some() {
                let (l, d, n) = (ob.level, ob.depth, ob.cube.len());
                self.tr(format_args!(
                    "{{\"e\":\"O\",\"lvl\":{l},\"d\":{d},\"n\":{n}}}"
                ));
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
            if self.cfg.promote && self.promote_for(&ob.cube, ob.level) {
                // The promoted lemma blocks the cube at this level (or
                // higher): the obligation is taken up again one level up.
                q.push(ob);
                continue;
            }
            match self.relative_inductive(&ob.cube, ob.level, true) {
                Ok(core) => {
                    let g = self.generalize(core, ob.level, 0, ctx);
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
            .any(|lemma| self.covers(lemma, cube))
    }

    /// Push lemmas forward; true when two frames become equal (proof).
    /// A lemma that fails to push is retried after blocking, one frame
    /// down, the state that stopped it (counterexample to propagation,
    /// CTP), up to `max_ctps` times.
    fn propagate(&mut self, ctx: &RunContext) -> bool {
        if self.trace.is_some() {
            let (sizes, q) = (self.frame_sizes(), self.stats_queries);
            self.tr(format_args!(
                "{{\"e\":\"F\",\"q\":{q},\"sizes\":{sizes:?}}}"
            ));
        }
        if self.single && self.aux_cap > 0 && self.cfg.derive {
            self.derive_groups();
        }
        for i in 1..self.top() {
            let mut lemmas: std::collections::VecDeque<Cube> =
                std::mem::take(&mut self.frames[i]).into();
            let mut stay = Vec::new();
            while let Some(c) = lemmas.pop_front() {
                if ctx.should_stop() {
                    stay.push(c);
                    continue;
                }
                let pushed = self.push_lemma(&c, i, ctx);
                if !pushed && c.iter().any(|&l| self.is_aux(l)) {
                    // A group moves only as a whole; when it cannot, its
                    // members (still clauses of this frame) try alone.
                    lemmas.extend(self.expand(&c));
                    continue;
                }
                if self.trace.is_some() {
                    self.tr(format_args!(
                        "{{\"e\":\"P\",\"from\":{i},\"ok\":{pushed},\"lits\":{c:?}}}"
                    ));
                }
                if pushed {
                    self.publish(&c, i + 1);
                    let clause: Vec<i32> = c.iter().map(|&l| -t(l)).collect();
                    self.add_frame_clause(i + 1, &clause);
                    self.frames[i + 1].push(c);
                } else {
                    stay.push(c);
                }
            }
            // Lemmas CTP added at level i while its list was taken.
            stay.append(&mut self.frames[i]);
            self.frames[i] = stay;
            if self.frames[i].is_empty() {
                self.proof_level = Some(i);
                return true;
            }
        }
        false
    }

    /// Does lemma `c` of frame `i` hold in frame `i + 1`?
    fn push_lemma(&mut self, c: &Cube, i: usize, ctx: &RunContext) -> bool {
        let mut ctps = 0;
        loop {
            // Only at the frontier: lower down, CTP lemmas cost more than
            // the pushes they win (measured on the HWMCC'24 sample).
            let ctp = ctps < self.cfg.max_ctps && i + 1 == self.top();
            // Tracing (H3) also asks for the counterexample state.
            let want = ctp || self.trace.is_some();
            match self.relative_inductive(c, i + 1, want) {
                Ok(_) => return true,
                Err(None) => return false,
                Err(Some(p)) => {
                    if self.trace.is_some() {
                        // Is the state that breaks the push excluded by a
                        // lemma stuck below level i (H3)?
                        let low = (1..i)
                            .rev()
                            .find(|&j| self.frames[j].iter().any(|l| self.covers(l, &p)))
                            .unwrap_or(0);
                        let n = p.len();
                        self.tr(format_args!(
                            "{{\"e\":\"X\",\"from\":{i},\"low\":{low},\"n\":{n}}}"
                        ));
                    }
                    if !ctp {
                        return false;
                    }
                    if ctx.should_stop() || self.intersects_init(&p) {
                        return false;
                    }
                    // Block the predecessor in frame i, if it is not
                    // reachable there; then the push may succeed.
                    match self.relative_inductive(&p, i, false) {
                        Ok(core) => {
                            ctps += 1;
                            self.stats_ctps += 1;
                            let g = self.generalize(core, i, 1, ctx);
                            // As high as it holds: left at level i it would
                            // keep frame i from emptying (convergence).
                            let mut lvl = i;
                            while lvl < self.top()
                                && self.relative_inductive(&g, lvl + 1, false).is_ok()
                            {
                                lvl += 1;
                            }
                            self.trace_src = "ctp";
                            self.add_lemma(g, lvl);
                            self.trace_src = "po";
                        }
                        Err(_) => return false,
                    }
                }
            }
        }
    }

    pub fn check(&mut self, ctx: &RunContext) -> Verdict {
        // Depth 0: a bad initial state.
        let mut a0 = self.frame_assumptions(0);
        a0.push(self.bad);
        if self.single {
            self.solvers[0].sat.set_active_groups_from(0);
        }
        match self.sol(0).sat.solve(&a0) {
            Answer::Sat => return Verdict::Unsafe { depth: Some(0) },
            Answer::Unknown => {
                return Verdict::Unknown {
                    reason: ctx.stop_reason().into(),
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
                        reason: ctx.stop_reason().into(),
                    };
                }
                self.import(ctx);
                let k = self.top();
                let bad = self.bad;
                let dom = self.domain(&[bad], k);
                let mut ak = self.frame_assumptions(k);
                ak.push(bad);
                if self.single {
                    self.solvers[0].sat.set_active_groups_from(k as u32);
                }
                match self.sol(k).sat.solve_domain(&ak, &dom) {
                    Answer::Unknown => {
                        return Verdict::Unknown {
                            reason: ctx.stop_reason().into(),
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
                                    reason: ctx.stop_reason().into(),
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

            let pool: usize = self.frames[1..].iter().map(Vec::len).sum();
            let due = if self.cfg.houdini_grow {
                pool >= 100 && pool >= 2 * self.pool_after_houdini
            } else {
                self.cfg.houdini_every > 0 && self.top().is_multiple_of(self.cfg.houdini_every)
            };
            if due {
                self.houdini_to_inf(ctx);
                self.pool_after_houdini = self.frames[1..].iter().map(Vec::len).sum();
            }
            if ctx.should_stop() {
                return Verdict::Unknown {
                    reason: ctx.stop_reason().into(),
                };
            }
        }
    }

    /// Every lemma IC3 holds, at any level. Each excludes the initial
    /// states but is only known to hold up to its frame: run them through
    /// [`crate::invariant::houdini`] before treating them as invariants.
    pub fn lemmas(&self) -> Vec<Cube> {
        self.frames
            .iter()
            .flatten()
            .flat_map(|c| self.expand(c))
            .collect()
    }

    /// The standing invariant (assumed or found by Houdini so far), as
    /// cubes over latch literals.
    pub fn standing_invariant(&self) -> Vec<Cube> {
        self.inf.iter().flat_map(|c| self.expand(c)).collect()
    }

    /// After [`Ic3::check`] said safe: the inductive invariant, as the cubes
    /// it blocks (the lemmas above the frame that emptied). `None` before.
    pub fn invariant(&self) -> Option<Vec<Cube>> {
        let i = self.proof_level?;
        Some(
            self.frames[i + 1..]
                .iter()
                .flatten()
                .chain(&self.inf)
                .flat_map(|c| self.expand(c))
                .collect(),
        )
    }

    /// Assume `cubes` blocked in every frame from 1 up: they must be a
    /// proved invariant (Houdini's output). They hold initially, so the
    /// single-solver layout adds them unguarded.
    pub fn assume_invariant(&mut self, cubes: Vec<Cube>) {
        for c in &cubes {
            let clause: Vec<i32> = c.iter().map(|&l| -t(l)).collect();
            let from = if self.single { 0 } else { 1 };
            for s in self.solvers.iter_mut().skip(from) {
                s.sat.add_clause(&clause);
            }
        }
        self.inf.extend(cubes);
    }

    /// H2: the lemmas that are inductive together (Houdini, within a tenth
    /// of the time left) go to [`Ic3::assume_invariant`] and leave the
    /// frames, so they need no more pushing in later frames.
    fn houdini_to_inf(&mut self, ctx: &RunContext) {
        let t0 = std::time::Instant::now();
        // The invariant so far is inductive on its own, so it survives and
        // helps the new lemmas through.
        let cands: Vec<Cube> = self.frames[1..]
            .iter()
            .flatten()
            .chain(&self.inf)
            .flat_map(|c| self.expand(c))
            .collect();
        let mut h = ctx.clone();
        let rest = ctx.deadline.saturating_duration_since(t0);
        h.deadline = t0 + rest / 10;
        let make = &self.make_sat;
        let Some(inv) = crate::invariant::houdini(self.g, cands, || make(), &h) else {
            return;
        };
        let keep: std::collections::HashSet<Cube> = inv.iter().cloned().collect();
        let mut moved = 0;
        for lvl in 1..self.frames.len() {
            let f = std::mem::take(&mut self.frames[lvl]);
            let before = f.len();
            // A group goes when every member is in the invariant.
            let kept: Vec<Cube> = f
                .into_iter()
                .filter(|c| !self.expand(c).iter().all(|e| keep.contains(e)))
                .collect();
            moved += before - kept.len();
            self.frames[lvl] = kept;
        }
        if self.trace.is_some() {
            let (n, q) = (inv.len(), self.stats_queries);
            self.tr(format_args!(
                "{{\"e\":\"H\",\"q\":{q},\"inv\":{n},\"moved\":{moved},\"ms\":{}}}",
                t0.elapsed().as_millis()
            ));
        }
        if std::env::var_os("INFORMAL_IC3_STATS").is_some() {
            eprintln!(
                "ic3 houdini at frame {}: {} lemmas inductive ({} moved), {:.2} s",
                self.top(),
                inv.len(),
                moved,
                t0.elapsed().as_secs_f64()
            );
        }
        let had: std::collections::HashSet<&Cube> = self.inf.iter().collect();
        let new: Vec<Cube> = inv.iter().filter(|c| !had.contains(c)).cloned().collect();
        self.assume_invariant(new);
    }

    /// Lemmas held at each level.
    pub fn frame_sizes(&self) -> Vec<usize> {
        self.frames.iter().map(Vec::len).collect()
    }

    pub fn depth(&self) -> usize {
        self.top()
    }
}
