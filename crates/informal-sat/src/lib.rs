//! SAT solving for the in-house engines.
//!
//! [`Sat`] is the incremental interface the engines use: add clauses,
//! solve under assumptions, read the model. [`Cadical`] wraps CaDiCaL
//! through its C API; [`Dpll`] is a small solver with unit propagation,
//! used by tests and as a fallback when CaDiCaL is not linked.
//! Literals are DIMACS-style: variable `v > 0` is `v`, its negation `-v`.

pub mod interp;
pub mod mini;
pub use interp::{InterpAig, Interpolator};
pub use mini::Mini;

pub type Lit = i32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Sat,
    Unsat,
    /// Stopped by the terminate callback.
    Unknown,
}

pub trait Sat {
    fn add_clause(&mut self, lits: &[Lit]);
    /// Solve under `assumptions` (they hold for this call only).
    fn solve(&mut self, assumptions: &[Lit]) -> Answer;
    /// Value of `lit` in the last model (after `Sat`).
    fn value(&self, lit: Lit) -> bool;
    /// Polled during search; returning true stops it with `Unknown`.
    fn set_terminate(&mut self, f: Box<dyn Fn() -> bool + Send>);
    fn name(&self) -> &'static str;
    /// After `Unsat` under assumptions: true if assumption `lit` was needed
    /// (it is in the final conflict). Solvers without cores say true.
    fn failed(&self, _lit: Lit) -> bool {
        true
    }
    /// Solve deciding only on the variables in `domain` (GipSAT-style COI
    /// restriction). The caller guarantees that any assignment of the
    /// domain without conflict extends to a model of every clause. Solvers
    /// that cannot restrict decisions just solve.
    fn solve_domain(&mut self, assumptions: &[Lit], _domain: &[Lit]) -> Answer {
        self.solve(assumptions)
    }
    /// Announce `n` more variables before using them (CaDiCaL >= 2.2
    /// wants this for incremental use).
    fn reserve(&mut self, _n: i32) {}
    /// Add a clause to watch-list group `group` (small numbers). It is a
    /// clause like any other; the group only lets
    /// [`Sat::set_active_groups_from`] skip it. Solvers without groups
    /// just add it.
    fn add_clause_grouped(&mut self, lits: &[Lit], _group: u32) {
        self.add_clause(lits);
    }
    /// For the next [`Sat::solve_domain`] calls: clauses of groups below
    /// `group` may be ignored. The caller guarantees that they are
    /// satisfiable whatever the domain is assigned (for instance, each
    /// carries an activation literal that is not assumed).
    fn set_active_groups_from(&mut self, _group: u32) {}
    /// Add `lits` as a clause that only holds while `act` is assumed
    /// (`lits | -act`), for one query. `act` must occur nowhere else.
    /// At most one such clause is outstanding at a time; end it with
    /// [`Sat::retire`] after reading the answer.
    fn add_temp_clause(&mut self, lits: &[Lit], act: Lit) {
        // The activation literal first, as IC3 always built this clause:
        // the order decides which models CaDiCaL returns, and IC3's path
        // (not its verdict) depends on them.
        let mut c = Vec::with_capacity(lits.len() + 1);
        c.push(-act);
        c.extend_from_slice(lits);
        self.add_clause(&c);
    }
    /// Drop the temporary clause of `act`. True when the solver removed
    /// every trace of it and `act` may be used again for the next
    /// temporary clause; false when `act` is now fixed false (the caller
    /// takes a fresh variable next time).
    fn retire(&mut self, act: Lit) -> bool {
        self.add_clause(&[-act]);
        false
    }
}

/// The best solver available in this build.
pub fn best() -> Box<dyn Sat> {
    match Cadical::new() {
        Ok(c) => Box::new(c),
        // Mini is our own CDCL; Dpll is only a reference for tests.
        Err(_) => Box::new(Mini::default()),
    }
}

// ------------------------------------------------------------ CaDiCaL

#[cfg(informal_cadical)]
mod ffi {
    use std::ffi::{c_char, c_int, c_void};
    #[repr(C)]
    pub struct CCaDiCaL {
        _p: [u8; 0],
    }
    extern "C" {
        pub fn ccadical_init() -> *mut CCaDiCaL;
        pub fn ccadical_release(s: *mut CCaDiCaL);
        pub fn ccadical_add(s: *mut CCaDiCaL, lit: c_int);
        pub fn ccadical_assume(s: *mut CCaDiCaL, lit: c_int);
        pub fn ccadical_solve(s: *mut CCaDiCaL) -> c_int;
        pub fn ccadical_val(s: *mut CCaDiCaL, lit: c_int) -> c_int;
        pub fn ccadical_set_terminate(
            s: *mut CCaDiCaL,
            state: *mut c_void,
            f: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
        );
        pub fn ccadical_signature() -> *const c_char;
        pub fn ccadical_declare_more_variables(s: *mut CCaDiCaL, n: c_int) -> c_int;
        pub fn ccadical_failed(s: *mut CCaDiCaL, lit: c_int) -> c_int;
    }
}

/// CaDiCaL (Biere et al.), the solver the plan names for the in-house
/// BMC, k-induction and IC3 engines.
pub struct Cadical {
    #[cfg(informal_cadical)]
    raw: *mut ffi::CCaDiCaL,
    #[cfg(informal_cadical)]
    terminate: Option<Box<Box<dyn Fn() -> bool + Send>>>,
}

// One solver is used by one thread at a time.
unsafe impl Send for Cadical {}

#[cfg(informal_cadical)]
unsafe extern "C" fn call_terminate(state: *mut std::ffi::c_void) -> std::ffi::c_int {
    let f = &*(state as *const Box<dyn Fn() -> bool + Send>);
    f() as std::ffi::c_int
}

impl Cadical {
    pub fn new() -> Result<Cadical, String> {
        #[cfg(informal_cadical)]
        {
            let raw = unsafe { ffi::ccadical_init() };
            if raw.is_null() {
                return Err("ccadical_init failed".into());
            }
            Ok(Cadical {
                raw,
                terminate: None,
            })
        }
        #[cfg(not(informal_cadical))]
        Err("informal was built without CaDiCaL (set INFORMAL_CADICAL_DIR and rebuild)".into())
    }

    /// CaDiCaL's version string, when linked.
    pub fn signature() -> Option<String> {
        #[cfg(informal_cadical)]
        {
            let p = unsafe { ffi::ccadical_signature() };
            Some(
                unsafe { std::ffi::CStr::from_ptr(p) }
                    .to_string_lossy()
                    .into_owned(),
            )
        }
        #[cfg(not(informal_cadical))]
        None
    }
}

#[cfg(informal_cadical)]
impl Drop for Cadical {
    fn drop(&mut self) {
        unsafe { ffi::ccadical_release(self.raw) };
    }
}

#[cfg(informal_cadical)]
impl Sat for Cadical {
    fn add_clause(&mut self, lits: &[Lit]) {
        for &l in lits {
            unsafe { ffi::ccadical_add(self.raw, l) };
        }
        unsafe { ffi::ccadical_add(self.raw, 0) };
    }
    fn solve(&mut self, assumptions: &[Lit]) -> Answer {
        for &a in assumptions {
            unsafe { ffi::ccadical_assume(self.raw, a) };
        }
        match unsafe { ffi::ccadical_solve(self.raw) } {
            10 => Answer::Sat,
            20 => Answer::Unsat,
            _ => Answer::Unknown,
        }
    }
    fn value(&self, lit: Lit) -> bool {
        // `val` returns `lit` when it is true and `-lit` when false.
        unsafe { ffi::ccadical_val(self.raw, lit) == lit }
    }
    fn set_terminate(&mut self, f: Box<dyn Fn() -> bool + Send>) {
        let boxed = Box::new(f);
        let state = &*boxed as *const Box<dyn Fn() -> bool + Send> as *mut std::ffi::c_void;
        unsafe { ffi::ccadical_set_terminate(self.raw, state, Some(call_terminate)) };
        self.terminate = Some(boxed);
    }
    fn name(&self) -> &'static str {
        "cadical"
    }
    fn failed(&self, lit: Lit) -> bool {
        unsafe { ffi::ccadical_failed(self.raw, lit) != 0 }
    }
    fn reserve(&mut self, n: i32) {
        if n > 0 {
            unsafe { ffi::ccadical_declare_more_variables(self.raw, n) };
        }
    }
}

#[cfg(not(informal_cadical))]
impl Sat for Cadical {
    fn add_clause(&mut self, _: &[Lit]) {
        unreachable!("Cadical::new fails without CaDiCaL")
    }
    fn solve(&mut self, _: &[Lit]) -> Answer {
        unreachable!()
    }
    fn value(&self, _: Lit) -> bool {
        unreachable!()
    }
    fn set_terminate(&mut self, _: Box<dyn Fn() -> bool + Send>) {}
    fn name(&self) -> &'static str {
        "cadical"
    }
}

// ------------------------------------------------------------ DPLL

/// DPLL with unit propagation and chronological backtracking. Fine for
/// tests and toy models; hopeless on real designs.
#[derive(Default)]
pub struct Dpll {
    clauses: Vec<Vec<Lit>>,
    vars: usize,
    model: Vec<i8>,
    terminate: Option<Box<dyn Fn() -> bool + Send>>,
}

impl Dpll {
    fn val(assign: &[i8], l: Lit) -> i8 {
        let v = assign[l.unsigned_abs() as usize];
        if l > 0 {
            v
        } else {
            -v
        }
    }

    /// Propagate units to a fixpoint; false on conflict.
    fn propagate(&self, assign: &mut [i8], trail: &mut Vec<usize>) -> bool {
        loop {
            let mut changed = false;
            for c in &self.clauses {
                let mut unassigned = None;
                let mut free = 0;
                let mut sat = false;
                for &l in c {
                    match Self::val(assign, l) {
                        1 => {
                            sat = true;
                            break;
                        }
                        0 => {
                            free += 1;
                            unassigned = Some(l);
                        }
                        _ => {}
                    }
                }
                if sat {
                    continue;
                }
                match (free, unassigned) {
                    (0, _) => return false,
                    (1, Some(l)) => {
                        let v = l.unsigned_abs() as usize;
                        assign[v] = if l > 0 { 1 } else { -1 };
                        trail.push(v);
                        changed = true;
                    }
                    _ => {}
                }
            }
            if !changed {
                return true;
            }
        }
    }

    fn search(&self, assign: &mut Vec<i8>, steps: &mut u64) -> Option<bool> {
        *steps += 1;
        if (*steps).is_multiple_of(1024) && self.terminate.as_ref().is_some_and(|f| f()) {
            return None;
        }
        let mut trail = Vec::new();
        if !self.propagate(assign, &mut trail) {
            for v in trail {
                assign[v] = 0;
            }
            return Some(false);
        }
        let Some(v) = (1..=self.vars).find(|&v| assign[v] == 0) else {
            return Some(true);
        };
        for val in [1i8, -1] {
            assign[v] = val;
            match self.search(assign, steps) {
                Some(true) => return Some(true),
                None => return None,
                Some(false) => {}
            }
        }
        assign[v] = 0;
        for v in trail {
            assign[v] = 0;
        }
        Some(false)
    }
}

impl Sat for Dpll {
    fn add_clause(&mut self, lits: &[Lit]) {
        for &l in lits {
            self.vars = self.vars.max(l.unsigned_abs() as usize);
        }
        self.clauses.push(lits.to_vec());
    }
    fn solve(&mut self, assumptions: &[Lit]) -> Answer {
        for &a in assumptions {
            self.vars = self.vars.max(a.unsigned_abs() as usize);
        }
        let mut assign = vec![0i8; self.vars + 1];
        for &a in assumptions {
            let v = a.unsigned_abs() as usize;
            let want = if a > 0 { 1 } else { -1 };
            if assign[v] == -want {
                return Answer::Unsat;
            }
            assign[v] = want;
        }
        match self.search(&mut assign, &mut 0) {
            Some(true) => {
                self.model = assign;
                Answer::Sat
            }
            Some(false) => Answer::Unsat,
            None => Answer::Unknown,
        }
    }
    fn value(&self, lit: Lit) -> bool {
        self.model
            .get(lit.unsigned_abs() as usize)
            .is_some_and(|&v| (v > 0) == (lit > 0) && v != 0)
    }
    fn set_terminate(&mut self, f: Box<dyn Fn() -> bool + Send>) {
        self.terminate = Some(f);
    }
    fn name(&self) -> &'static str {
        "dpll"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exercise(s: &mut dyn Sat) {
        // (a | b) & (!a | c) & (!b | c)
        s.add_clause(&[1, 2]);
        s.add_clause(&[-1, 3]);
        s.add_clause(&[-2, 3]);
        assert_eq!(s.solve(&[]), Answer::Sat);
        assert!(s.value(3));
        assert_eq!(s.solve(&[-3]), Answer::Unsat);
        // Assumptions do not stick.
        assert_eq!(s.solve(&[1]), Answer::Sat);
        assert!(s.value(1) && s.value(3) && !s.value(-3));
        assert_eq!(s.solve(&[-1, -2]), Answer::Unsat);
        assert_eq!(s.solve(&[-1]), Answer::Sat);
        assert!(s.value(-1) && s.value(2));
        s.add_clause(&[-3]);
        assert_eq!(s.solve(&[]), Answer::Unsat);
    }

    #[test]
    fn dpll() {
        exercise(&mut Dpll::default());
    }

    #[test]
    fn cadical_when_linked() {
        match Cadical::new() {
            Ok(mut c) => {
                assert!(Cadical::signature().unwrap().starts_with("cadical"));
                exercise(&mut c);
            }
            Err(e) => {
                // Same rule as informal_engine::tool_missing (this crate is below it).
                if std::env::var_os("INFORMAL_REQUIRE_TOOLS").is_some_and(|v| !v.is_empty()) {
                    panic!("{e} (INFORMAL_REQUIRE_TOOLS is set: a missing tool fails the test)");
                }
                eprintln!("skipping: {e}")
            }
        }
    }

    #[test]
    fn mini() {
        exercise(&mut Mini::default());
    }

    fn temp_clauses(s: &mut dyn Sat, reusable: bool) {
        s.add_clause(&[1, 2]);
        // For one query: !a. Then b must hold.
        let mut act = 10;
        s.reserve(10);
        s.add_temp_clause(&[-1], act);
        assert_eq!(s.solve(&[act, -2]), Answer::Unsat);
        assert!(s.failed(-2));
        assert_eq!(s.solve(&[act]), Answer::Sat);
        assert!(s.value(2) && !s.value(1));
        assert_eq!(s.retire(act), reusable);
        if !reusable {
            act = 11;
            s.reserve(1);
        }
        // Gone: a alone is fine again.
        assert_eq!(s.solve(&[-2]), Answer::Sat);
        assert!(s.value(1));
        // A temporary clause that is false at level 0.
        s.add_clause(&[1]);
        s.add_temp_clause(&[-1], act);
        assert_eq!(s.solve(&[act]), Answer::Unsat);
        assert!(s.failed(act));
        assert_eq!(s.retire(act), reusable);
        assert_eq!(s.solve(&[]), Answer::Sat);
    }

    #[test]
    fn temporary_clauses() {
        temp_clauses(&mut Mini::default(), true);
        temp_clauses(&mut Dpll::default(), false);
        if let Ok(mut c) = Cadical::new() {
            temp_clauses(&mut c, false);
        }
    }

    /// Random queries with a temporary clause each, the activation
    /// variable reused throughout: Mini agrees with a fresh DPLL that has
    /// the clause permanently, before and after each retire.
    #[test]
    fn mini_temp_clauses_match_dpll() {
        let mut seed = 0x9E3779B97F4A7C15u64;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for round in 0..400 {
            let nv = 5 + rnd(10) as i32;
            let act = nv + 1;
            let lit = |rnd: &mut dyn FnMut(u64) -> u64| {
                let v = 1 + rnd(nv as u64) as Lit;
                if rnd(2) == 0 {
                    v
                } else {
                    -v
                }
            };
            let mut base: Vec<Vec<Lit>> = Vec::new();
            let mut m = Mini::default();
            m.reserve(act);
            for _ in 0..(nv as usize * 2) {
                let c: Vec<Lit> = (0..1 + rnd(3)).map(|_| lit(&mut rnd)).collect();
                m.add_clause(&c);
                base.push(c);
            }
            for q in 0..40 {
                let temp: Vec<Lit> = (0..1 + rnd(3)).map(|_| lit(&mut rnd)).collect();
                let assume: Vec<Lit> = (0..rnd(4)).map(|_| lit(&mut rnd)).collect();
                let want = |cls: &[Vec<Lit>], extra: Option<&Vec<Lit>>, a: &[Lit]| {
                    let mut d = Dpll::default();
                    for c in cls.iter().chain(extra) {
                        d.add_clause(c);
                    }
                    d.solve(a)
                };
                m.add_temp_clause(&temp, act);
                let mut a = assume.clone();
                a.push(act);
                assert_eq!(
                    m.solve(&a),
                    want(&base, Some(&temp), &assume),
                    "round {round} query {q} with the clause"
                );
                assert!(m.retire(act));
                assert_eq!(
                    m.solve(&assume),
                    want(&base, None, &assume),
                    "round {round} query {q} after retire"
                );
                if rnd(5) == 0 {
                    let c: Vec<Lit> = (0..1 + rnd(3)).map(|_| lit(&mut rnd)).collect();
                    m.add_clause(&c);
                    base.push(c);
                }
            }
        }
    }

    /// Random small CNFs: Mini agrees with DPLL, its models satisfy the
    /// clauses, and its failed assumptions alone are already UNSAT.
    #[test]
    fn mini_matches_dpll_on_random_cnf() {
        let mut seed = 0x2545F4914F6CDD1Du64;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for round in 0..3000 {
            let nv = 4 + rnd(12) as i32;
            let nc = (nv as u64 * (2 + rnd(4))) as usize;
            let mut cls = Vec::new();
            for _ in 0..nc {
                let len = 1 + rnd(3) as usize;
                let c: Vec<Lit> = (0..len)
                    .map(|_| {
                        let v = 1 + rnd(nv as u64) as Lit;
                        if rnd(2) == 0 {
                            v
                        } else {
                            -v
                        }
                    })
                    .collect();
                cls.push(c);
            }
            let assume: Vec<Lit> = (0..rnd(4))
                .map(|_| {
                    let v = 1 + rnd(nv as u64) as Lit;
                    if rnd(2) == 0 {
                        v
                    } else {
                        -v
                    }
                })
                .collect();
            let (mut m, mut d) = (Mini::default(), Dpll::default());
            for c in &cls {
                m.add_clause(c);
                d.add_clause(c);
            }
            let (a, b) = (m.solve(&assume), d.solve(&assume));
            assert_eq!(a, b, "round {round}");
            if a == Answer::Sat {
                for c in &cls {
                    assert!(
                        c.iter().any(|&l| m.value(l)),
                        "round {round}: clause {c:?} false"
                    );
                }
                for &x in &assume {
                    assert!(m.value(x));
                }
            } else {
                let core: Vec<Lit> = assume.iter().copied().filter(|&x| m.failed(x)).collect();
                let mut d2 = Dpll::default();
                for c in &cls {
                    d2.add_clause(c);
                }
                assert_eq!(
                    d2.solve(&core),
                    Answer::Unsat,
                    "round {round}: core {core:?} of {assume:?}"
                );
            }
        }
    }

    #[test]
    fn mini_solve_domain_and_groups_match_dpll() {
        let mut seed = 0xD1B5_4A32_D192_ED03u64;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for round in 0..300 {
            let nv = 6 + rnd(8) as i32;
            let act0 = nv + 1;
            let act1 = nv + 2;
            let act2 = nv + 3;
            let mut m = Mini::with_seed(round as u64 + 1);
            m.reserve(act2);
            let mut base: Vec<Vec<Lit>> = Vec::new();
            let mut g_cls: [Vec<Vec<Lit>>; 3] = [Vec::new(), Vec::new(), Vec::new()];
            let mk_lit = |rnd: &mut dyn FnMut(u64) -> u64| {
                let v = 1 + rnd(nv as u64) as Lit;
                if rnd(2) == 0 {
                    v
                } else {
                    -v
                }
            };
            for _ in 0..(nv as usize * 2) {
                let len = 2 + rnd(2) as usize;
                let c: Vec<Lit> = (0..len).map(|_| mk_lit(&mut rnd)).collect();
                m.add_clause(&c);
                base.push(c);
            }
            let acts = [act0, act1, act2];
            for g in 0..3 {
                for _ in 0..4 {
                    let len = 1 + rnd(3) as usize;
                    let mut c: Vec<Lit> = (0..len).map(|_| mk_lit(&mut rnd)).collect();
                    c.push(-acts[g]);
                    m.add_clause_grouped(&c, g as u32);
                    g_cls[g].push(c);
                }
            }
            let dom: Vec<Lit> = (1..=nv).collect();
            for active_from in 0..3u32 {
                let mut assume: Vec<Lit> = (0..rnd(3)).map(|_| mk_lit(&mut rnd)).collect();
                for &a in &acts[active_from as usize..] {
                    assume.push(a);
                }
                m.set_active_groups_from(active_from);
                let ans = m.solve_domain(&assume, &dom);

                let mut d = Dpll::default();
                for c in &base {
                    d.add_clause(c);
                }
                for cls in g_cls.iter().skip(active_from as usize) {
                    for c in cls {
                        d.add_clause(c);
                    }
                }
                let want = d.solve(&assume);
                assert_eq!(ans, want, "round {round} group {active_from}");
            }
            // Follow with an unrestricted solve to exercise `heap_dirty` rebuild.
            let assume: Vec<Lit> = (0..rnd(3)).map(|_| mk_lit(&mut rnd)).collect();
            let ans = m.solve(&assume);
            let mut d = Dpll::default();
            for c in base.iter().chain(g_cls.iter().flatten()) {
                d.add_clause(c);
            }
            assert_eq!(ans, d.solve(&assume), "round {round} unrestricted follow-up");
        }
    }

    #[test]
    fn pigeonhole_3_into_2_is_unsat() {
        let mut s = best();
        let p = |i: i32, h: i32| i * 2 + h + 1; // pigeon i in hole h
        for i in 0..3 {
            s.add_clause(&[p(i, 0), p(i, 1)]);
        }
        for h in 0..2 {
            for i in 0..3 {
                for j in i + 1..3 {
                    s.add_clause(&[-p(i, h), -p(j, h)]);
                }
            }
        }
        assert_eq!(s.solve(&[]), Answer::Unsat);
    }
}
