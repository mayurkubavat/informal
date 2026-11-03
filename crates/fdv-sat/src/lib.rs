//! SAT solving for the in-house engines.
//!
//! [`Sat`] is the incremental interface the engines use: add clauses,
//! solve under assumptions, read the model. [`Cadical`] wraps CaDiCaL
//! through its C API; [`Dpll`] is a small solver with unit propagation,
//! used by tests and as a fallback when CaDiCaL is not linked.
//! Literals are DIMACS-style: variable `v > 0` is `v`, its negation `-v`.

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
    /// Announce `n` more variables before using them (CaDiCaL >= 2.2
    /// wants this for incremental use).
    fn reserve(&mut self, _n: i32) {}
}

/// The best solver available in this build.
pub fn best() -> Box<dyn Sat> {
    match Cadical::new() {
        Ok(c) => Box::new(c),
        Err(_) => Box::new(Dpll::default()),
    }
}

// ------------------------------------------------------------ CaDiCaL

#[cfg(fdv_cadical)]
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
    }
}

/// CaDiCaL (Biere et al.), the solver the plan names for the in-house
/// BMC, k-induction and IC3 engines.
pub struct Cadical {
    #[cfg(fdv_cadical)]
    raw: *mut ffi::CCaDiCaL,
    #[cfg(fdv_cadical)]
    terminate: Option<Box<Box<dyn Fn() -> bool + Send>>>,
}

// One solver is used by one thread at a time.
unsafe impl Send for Cadical {}

#[cfg(fdv_cadical)]
unsafe extern "C" fn call_terminate(state: *mut std::ffi::c_void) -> std::ffi::c_int {
    let f = &*(state as *const Box<dyn Fn() -> bool + Send>);
    f() as std::ffi::c_int
}

impl Cadical {
    pub fn new() -> Result<Cadical, String> {
        #[cfg(fdv_cadical)]
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
        #[cfg(not(fdv_cadical))]
        Err("fdv was built without CaDiCaL (set FDV_CADICAL_DIR and rebuild)".into())
    }

    /// CaDiCaL's version string, when linked.
    pub fn signature() -> Option<String> {
        #[cfg(fdv_cadical)]
        {
            let p = unsafe { ffi::ccadical_signature() };
            Some(
                unsafe { std::ffi::CStr::from_ptr(p) }
                    .to_string_lossy()
                    .into_owned(),
            )
        }
        #[cfg(not(fdv_cadical))]
        None
    }
}

#[cfg(fdv_cadical)]
impl Drop for Cadical {
    fn drop(&mut self) {
        unsafe { ffi::ccadical_release(self.raw) };
    }
}

#[cfg(fdv_cadical)]
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
    fn reserve(&mut self, n: i32) {
        if n > 0 {
            unsafe { ffi::ccadical_declare_more_variables(self.raw, n) };
        }
    }
}

#[cfg(not(fdv_cadical))]
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
        if *steps % 1024 == 0 && self.terminate.as_ref().is_some_and(|f| f()) {
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
            Err(e) => eprintln!("skipping: {e}"),
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
