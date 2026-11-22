//! CNF builder wrapper over `informal_sat::Sat` implementing [`CnfEmitter`].

use informal_sat::{Lit, Sat};

use crate::expr_bv::CnfEmitter;

pub(crate) struct CnfBuilder {
    pub sat: Box<dyn Sat>,
    pub next_var: i32,
    pub base_clauses: Vec<Vec<Lit>>,
    pub const_true: Lit,
}

impl CnfBuilder {
    pub fn new() -> Self {
        let mut sat = informal_sat::best();
        sat.reserve(1);
        let const_true = 1;
        let base_clauses = vec![vec![const_true]];
        sat.add_clause(&[const_true]);
        Self {
            sat,
            next_var: 2,
            base_clauses,
            const_true,
        }
    }

    pub fn alloc_var(&mut self) -> Lit {
        let v = self.next_var;
        self.next_var += 1;
        self.sat.reserve(1);
        v
    }

    pub fn add_clause(&mut self, lits: &[Lit]) {
        self.base_clauses.push(lits.to_vec());
        self.sat.add_clause(lits);
    }

    pub fn tseitin_and(&mut self, lits: &[Lit]) -> Lit {
        if lits.is_empty() {
            return self.const_true;
        }
        if lits.len() == 1 {
            return lits[0];
        }
        let z = self.alloc_var();
        let mut big = Vec::with_capacity(lits.len() + 1);
        big.push(z);
        for &l in lits {
            self.add_clause(&[-z, l]);
            big.push(-l);
        }
        self.add_clause(&big);
        z
    }

    pub fn tseitin_or(&mut self, lits: &[Lit]) -> Lit {
        if lits.is_empty() {
            return -self.const_true;
        }
        if lits.len() == 1 {
            return lits[0];
        }
        let z = self.alloc_var();
        let mut big = Vec::with_capacity(lits.len() + 1);
        big.push(-z);
        for &l in lits {
            self.add_clause(&[z, -l]);
            big.push(l);
        }
        self.add_clause(&big);
        z
    }

    pub fn tseitin_xor(&mut self, a: Lit, b: Lit) -> Lit {
        let z = self.alloc_var();
        self.add_clause(&[-a, -b, -z]);
        self.add_clause(&[a, b, -z]);
        self.add_clause(&[-a, b, z]);
        self.add_clause(&[a, -b, z]);
        z
    }

    pub fn tseitin_xnor(&mut self, a: Lit, b: Lit) -> Lit {
        -self.tseitin_xor(a, b)
    }

    pub fn tseitin_ite(&mut self, c: Lit, t: Lit, e: Lit) -> Lit {
        let z = self.alloc_var();
        self.add_clause(&[-c, -t, z]);
        self.add_clause(&[-c, t, -z]);
        self.add_clause(&[c, -e, z]);
        self.add_clause(&[c, e, -z]);
        self.add_clause(&[-t, -e, z]);
        self.add_clause(&[t, e, -z]);
        z
    }

    /// Encodes `sum(vars) <= 1`, using pairwise clauses for `n <= 16`
    /// and a Sinz sequential counter for `n > 16`.
    pub fn at_most_one(&mut self, vars: &[Lit]) {
        let n = vars.len();
        if n <= 1 {
            return;
        }
        if n <= 16 {
            for i in 0..n {
                for j in (i + 1)..n {
                    self.add_clause(&[-vars[i], -vars[j]]);
                }
            }
        } else {
            let s: Vec<Lit> = (0..(n - 1)).map(|_| self.alloc_var()).collect();
            self.add_clause(&[-vars[0], s[0]]);
            for i in 1..(n - 1) {
                self.add_clause(&[-vars[i], s[i]]);
                self.add_clause(&[-s[i - 1], s[i]]);
                self.add_clause(&[-vars[i], -s[i - 1]]);
            }
            self.add_clause(&[-vars[n - 1], -s[n - 2]]);
        }
    }

    /// Encodes `sum(vars) == 1`.
    pub fn exactly_one(&mut self, vars: &[Lit]) {
        assert!(!vars.is_empty(), "exactly_one called with empty slice");
        self.add_clause(vars);
        self.at_most_one(vars);
    }

    /// Clones the accumulated CNF clauses into an independent fresh solver instance.
    pub fn clone_solver(&self) -> Box<dyn Sat> {
        let mut temp = informal_sat::best();
        temp.reserve(self.next_var - 1);
        for cl in &self.base_clauses {
            temp.add_clause(cl);
        }
        temp
    }
}

impl CnfEmitter for CnfBuilder {
    fn const_true(&self) -> Lit {
        self.const_true
    }

    fn alloc_var(&mut self) -> Lit {
        CnfBuilder::alloc_var(self)
    }

    fn tseitin_and(&mut self, lits: &[Lit]) -> Lit {
        CnfBuilder::tseitin_and(self, lits)
    }

    fn tseitin_or(&mut self, lits: &[Lit]) -> Lit {
        CnfBuilder::tseitin_or(self, lits)
    }

    fn tseitin_xor(&mut self, a: Lit, b: Lit) -> Lit {
        CnfBuilder::tseitin_xor(self, a, b)
    }

    fn tseitin_xnor(&mut self, a: Lit, b: Lit) -> Lit {
        CnfBuilder::tseitin_xnor(self, a, b)
    }

    fn tseitin_ite(&mut self, c: Lit, t: Lit, e: Lit) -> Lit {
        CnfBuilder::tseitin_ite(self, c, t, e)
    }
}
