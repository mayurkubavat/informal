//! Proof-tracing Craig interpolation backed by CaDiCaL's `CaDiCaL::Tracer`
//! (McMillan CAV'03 rule over pruned RUP/LRAT antecedent proofs).

use crate::{Answer, Lit};

/// Compact interpolant AIG returned by [`Interpolator::extract_interpolant`].
///
/// Literal encoding:
/// - `0` = `FALSE`, `1` = `TRUE`
/// - `2 * (1 + i)` (even) = positive `shared_vars[i]`; `^ 1` = negated
/// - `2 * (1 + shared_vars.len() + g)` = AND gate `ands[g]`; `^ 1` = negated
#[derive(Debug, Clone)]
pub struct InterpAig {
    pub shared_vars: Vec<i32>,
    pub ands: Vec<(u32, u32)>,
    pub root: u32,
}

#[cfg(informal_cadical_interp)]
mod ffi {
    use std::ffi::{c_int, c_void};
    extern "C" {
        pub fn informal_interp_new() -> *mut c_void;
        pub fn informal_interp_free(ptr: *mut c_void);
        pub fn informal_interp_set_terminate(
            ptr: *mut c_void,
            state: *mut c_void,
            f: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
        );
        pub fn informal_interp_reserve(ptr: *mut c_void, n: c_int);
        pub fn informal_interp_add_clause(
            ptr: *mut c_void,
            part: c_int,
            lits: *const c_int,
            len: usize,
        );
        pub fn informal_interp_solve(ptr: *mut c_void) -> c_int;
        pub fn informal_interp_val(ptr: *mut c_void, lit: c_int) -> c_int;
        pub fn informal_interp_extract(
            ptr: *mut c_void,
            out_shared: *mut *const i32,
            out_num_shared: *mut usize,
            out_lhs: *mut *const u32,
            out_rhs: *mut *const u32,
            out_num_ands: *mut usize,
            out_root: *mut u32,
        ) -> c_int;
    }
}

#[cfg(informal_cadical_interp)]
unsafe extern "C" fn call_terminate(state: *mut std::ffi::c_void) -> std::ffi::c_int {
    let f = &*(state as *const Box<dyn Fn() -> bool + Send>);
    f() as std::ffi::c_int
}

/// Partitioned SAT solver (`A` vs `B`) that produces a McMillan Craig
/// interpolant AIG whenever `A & B` is UNSAT.
pub struct Interpolator {
    #[cfg(informal_cadical_interp)]
    raw: *mut std::ffi::c_void,
    #[cfg(informal_cadical_interp)]
    terminate: Option<Box<Box<dyn Fn() -> bool + Send>>>,
}

unsafe impl Send for Interpolator {}

impl Interpolator {
    /// Whether this build linked the CaDiCaL proof-tracing interpolator.
    pub fn available() -> bool {
        cfg!(informal_cadical_interp)
    }

    pub fn new() -> Result<Self, String> {
        #[cfg(informal_cadical_interp)]
        {
            let raw = unsafe { ffi::informal_interp_new() };
            if raw.is_null() {
                return Err("informal_interp_new failed".into());
            }
            Ok(Self {
                raw,
                terminate: None,
            })
        }
        #[cfg(not(informal_cadical_interp))]
        Err("CaDiCaL proof tracer not linked in this build".into())
    }

    pub fn set_terminate(&mut self, f: Box<dyn Fn() -> bool + Send>) {
        #[cfg(informal_cadical_interp)]
        {
            let boxed = Box::new(f);
            let state = &*boxed as *const Box<dyn Fn() -> bool + Send> as *mut std::ffi::c_void;
            unsafe { ffi::informal_interp_set_terminate(self.raw, state, Some(call_terminate)) };
            self.terminate = Some(boxed);
        }
        #[cfg(not(informal_cadical_interp))]
        let _ = f;
    }

    pub fn reserve(&mut self, n: i32) {
        #[cfg(informal_cadical_interp)]
        if n > 0 {
            unsafe { ffi::informal_interp_reserve(self.raw, n) };
        }
        #[cfg(not(informal_cadical_interp))]
        let _ = n;
    }

    /// Add a clause belonging to partition `A`.
    pub fn add_a(&mut self, lits: &[Lit]) {
        #[cfg(informal_cadical_interp)]
        unsafe {
            ffi::informal_interp_add_clause(self.raw, 0, lits.as_ptr(), lits.len());
        }
        #[cfg(not(informal_cadical_interp))]
        let _ = lits;
    }

    /// Add a clause belonging to partition `B`.
    pub fn add_b(&mut self, lits: &[Lit]) {
        #[cfg(informal_cadical_interp)]
        unsafe {
            ffi::informal_interp_add_clause(self.raw, 1, lits.as_ptr(), lits.len());
        }
        #[cfg(not(informal_cadical_interp))]
        let _ = lits;
    }

    pub fn solve(&mut self) -> Answer {
        #[cfg(informal_cadical_interp)]
        {
            match unsafe { ffi::informal_interp_solve(self.raw) } {
                10 => Answer::Sat,
                20 => Answer::Unsat,
                _ => Answer::Unknown,
            }
        }
        #[cfg(not(informal_cadical_interp))]
        Answer::Unknown
    }

    pub fn value(&self, lit: Lit) -> bool {
        #[cfg(informal_cadical_interp)]
        {
            unsafe { ffi::informal_interp_val(self.raw, lit) == lit }
        }
        #[cfg(not(informal_cadical_interp))]
        {
            let _ = lit;
            false
        }
    }

    /// Extract the McMillan interpolant AIG after [`Self::solve`] returned `Answer::Unsat`.
    pub fn extract_interpolant(&mut self) -> Result<InterpAig, String> {
        #[cfg(informal_cadical_interp)]
        {
            let mut p_shared: *const i32 = std::ptr::null();
            let mut n_shared: usize = 0;
            let mut p_lhs: *const u32 = std::ptr::null();
            let mut p_rhs: *const u32 = std::ptr::null();
            let mut n_ands: usize = 0;
            let mut root: u32 = 0;
            let rc = unsafe {
                ffi::informal_interp_extract(
                    self.raw,
                    &mut p_shared,
                    &mut n_shared,
                    &mut p_lhs,
                    &mut p_rhs,
                    &mut n_ands,
                    &mut root,
                )
            };
            if rc != 0 {
                return Err(format!("interpolant extraction failed (rc={rc})"));
            }
            let shared_vars = if n_shared == 0 {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(p_shared, n_shared) }.to_vec()
            };
            let ands = if n_ands == 0 {
                Vec::new()
            } else {
                let lhs = unsafe { std::slice::from_raw_parts(p_lhs, n_ands) };
                let rhs = unsafe { std::slice::from_raw_parts(p_rhs, n_ands) };
                lhs.iter().copied().zip(rhs.iter().copied()).collect()
            };
            Ok(InterpAig {
                shared_vars,
                ands,
                root,
            })
        }
        #[cfg(not(informal_cadical_interp))]
        Err("CaDiCaL proof tracer not linked in this build".into())
    }
}

#[cfg(informal_cadical_interp)]
impl Drop for Interpolator {
    fn drop(&mut self) {
        unsafe { ffi::informal_interp_free(self.raw) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{best, Sat};

    fn check_craig_axioms(a: &[Vec<Lit>], b: &[Vec<Lit>]) {
        let mut itp = Interpolator::new().unwrap();
        let mut max_v = 0i32;
        for c in a.iter().chain(b) {
            for &l in c {
                max_v = max_v.max(l.abs());
            }
        }
        itp.reserve(max_v);
        for c in a {
            itp.add_a(c);
        }
        for c in b {
            itp.add_b(c);
        }
        assert_eq!(itp.solve(), Answer::Unsat);
        let g = itp.extract_interpolant().unwrap();

        let vars_of = |cls: &[Vec<Lit>]| -> std::collections::HashSet<i32> {
            cls.iter().flat_map(|c| c.iter().map(|l| l.abs())).collect()
        };
        let va = vars_of(a);
        let vb = vars_of(b);
        for &sv in &g.shared_vars {
            assert!(
                va.contains(&sv) && vb.contains(&sv),
                "interpolant variable {sv} is not shared"
            );
        }

        // Helper: encode InterpAig into a SAT solver and return the root SAT literal.
        let encode_itp = |sat: &mut dyn Sat, base_var: i32| -> Lit {
            let tru = base_var;
            sat.reserve(1 + g.ands.len() as i32);
            sat.add_clause(&[tru]);
            let map_lit = |al: u32| -> Lit {
                let idx = (al >> 1) as usize;
                let neg = (al & 1) != 0;
                let pos = if idx == 0 {
                    -tru
                } else if idx <= g.shared_vars.len() {
                    g.shared_vars[idx - 1]
                } else {
                    base_var + 1 + (idx - 1 - g.shared_vars.len()) as i32
                };
                if neg {
                    -pos
                } else {
                    pos
                }
            };
            for (gi, &(l, r)) in g.ands.iter().enumerate() {
                let out = base_var + 1 + gi as i32;
                let sl = map_lit(l);
                let sr = map_lit(r);
                sat.add_clause(&[-out, sl]);
                sat.add_clause(&[-out, sr]);
                sat.add_clause(&[out, -sl, -sr]);
            }
            map_lit(g.root)
        };

        // 1. A => I  (i.e. A & !I is UNSAT)
        let mut s1 = best();
        s1.reserve(max_v);
        for c in a {
            s1.add_clause(c);
        }
        let root1 = encode_itp(&mut *s1, max_v + 1);
        s1.add_clause(&[-root1]);
        assert_eq!(s1.solve(&[]), Answer::Unsat, "A => I failed");

        // 2. I & B is UNSAT
        let mut s2 = best();
        s2.reserve(max_v);
        for c in b {
            s2.add_clause(c);
        }
        let root2 = encode_itp(&mut *s2, max_v + 1);
        s2.add_clause(&[root2]);
        assert_eq!(s2.solve(&[]), Answer::Unsat, "I & B UNSAT failed");
    }

    #[test]
    fn craig_interpolant_satisfies_all_three_axioms() {
        if !Interpolator::available() {
            return;
        }
        // Simple chain: A = (x1) & (-x1 | s1) & (-s1 | x2) & (-x2 | s2); B = (-s2 | y1) & (-y1)
        check_craig_axioms(
            &[vec![1], vec![-1, 2], vec![-2, 3], vec![-3, 4]],
            &[vec![-4, 5], vec![-5]],
        );

        // Two-path disjunction over shared vars {10, 11}:
        // A forces (10 | 11) via local vars 1, 2; B forbids both 10 and 11 via local var 20.
        check_craig_axioms(
            &[vec![1, 2], vec![-1, 10], vec![-2, 11]],
            &[vec![-10, 20], vec![-11, 20], vec![-20]],
        );

        // Pigeonhole 3->2 split across A (pigeons 0, 1) and B (pigeon 2 + hole uniqueness).
        let p = |i: i32, h: i32| i * 2 + h + 1;
        let a = vec![
            vec![p(0, 0), p(0, 1)],
            vec![p(1, 0), p(1, 1)],
            vec![-p(0, 0), -p(1, 0)],
            vec![-p(0, 1), -p(1, 1)],
        ];
        let b = vec![
            vec![p(2, 0), p(2, 1)],
            vec![-p(0, 0), -p(2, 0)],
            vec![-p(1, 0), -p(2, 0)],
            vec![-p(0, 1), -p(2, 1)],
            vec![-p(1, 1), -p(2, 1)],
        ];
        check_craig_axioms(&a, &b);
    }
}
