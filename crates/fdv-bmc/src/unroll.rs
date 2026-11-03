//! Unrolls an AIG into a SAT solver one time frame at a time.
//!
//! Frame 0 latches take their reset value (free when uninitialized, or for
//! the induction step); frame k+1 latches take frame k's next-state
//! literal. Inputs are fresh in every frame and constraints hold in every
//! frame. Each AND node gets a Tseitin variable per frame.

use fdv_aig::{is_neg, var, Aig, Init, Node};
use fdv_sat::{Lit, Sat};

pub struct Unroll<'g> {
    g: &'g Aig,
    pub sat: Box<dyn Sat>,
    next_var: Lit,
    tru: Lit,
    /// Per frame: SAT literal of each AIG variable.
    frames: Vec<Vec<Lit>>,
    free_init: bool,
}

impl<'g> Unroll<'g> {
    pub fn new(g: &'g Aig, mut sat: Box<dyn Sat>, free_init: bool) -> Self {
        sat.reserve(1);
        sat.add_clause(&[1]);
        Unroll {
            g,
            sat,
            next_var: 2,
            tru: 1,
            frames: Vec::new(),
            free_init,
        }
    }

    fn fresh(&mut self) -> Lit {
        let v = self.next_var;
        self.next_var += 1;
        v
    }

    /// SAT literal of AIG literal `l` in frame `k`.
    pub fn lit(&self, k: usize, l: fdv_aig::Lit) -> Lit {
        let x = self.frames[k][var(l) as usize];
        if is_neg(l) {
            -x
        } else {
            x
        }
    }

    pub fn add_frame(&mut self) -> usize {
        let k = self.frames.len();
        let g = self.g;
        // Inputs, uninitialized latches and ANDs each take one variable.
        self.sat.reserve(g.nodes.len() as i32);
        let mut f = vec![0; g.nodes.len()];
        let mut late = Vec::new();
        let mut li = 0;
        for (v, n) in g.nodes.iter().enumerate() {
            f[v] = match *n {
                Node::Const => -self.tru,
                Node::Input => self.fresh(),
                Node::Latch => {
                    let latch = &g.latches[li];
                    li += 1;
                    debug_assert_eq!(var(latch.lit) as usize, v);
                    if k > 0 {
                        self.lit(k - 1, latch.next)
                    } else if self.free_init {
                        self.fresh()
                    } else {
                        match latch.init {
                            Init::Zero => -self.tru,
                            Init::One => self.tru,
                            Init::Free => self.fresh(),
                            Init::Expr(e) => {
                                let x = self.fresh();
                                late.push((x, e));
                                x
                            }
                        }
                    }
                }
                Node::And(a, b) => {
                    let la = if is_neg(a) {
                        -f[var(a) as usize]
                    } else {
                        f[var(a) as usize]
                    };
                    let lb = if is_neg(b) {
                        -f[var(b) as usize]
                    } else {
                        f[var(b) as usize]
                    };
                    let x = self.fresh();
                    self.sat.add_clause(&[-x, la]);
                    self.sat.add_clause(&[-x, lb]);
                    self.sat.add_clause(&[x, -la, -lb]);
                    x
                }
            };
        }
        self.frames.push(f);
        for (x, e) in late {
            let y = self.lit(k, e);
            self.sat.add_clause(&[-x, y]);
            self.sat.add_clause(&[x, -y]);
        }
        for &c in &g.constraints {
            let c = self.lit(k, c);
            self.sat.add_clause(&[c]);
        }
        k
    }

    /// A literal that is true iff some bad output holds in frame `k`.
    pub fn bad(&mut self, k: usize) -> Lit {
        let bads: Vec<Lit> = self.g.bads.iter().map(|&b| self.lit(k, b)).collect();
        if let [one] = bads[..] {
            return one;
        }
        self.sat.reserve(1);
        let x = self.fresh();
        let mut c = vec![-x];
        c.extend(bads);
        self.sat.add_clause(&c);
        x
    }

    fn state(&self, k: usize) -> Vec<bool> {
        self.g
            .latches
            .iter()
            .map(|l| self.sat.value(self.lit(k, l.lit)))
            .collect()
    }

    /// In the last model, two frames among 0..=k with the same latch values.
    pub fn repeated_states(&self, k: usize) -> Option<(usize, usize)> {
        let states: Vec<Vec<bool>> = (0..=k).map(|i| self.state(i)).collect();
        for j in 1..=k {
            for i in 0..j {
                if states[i] == states[j] {
                    return Some((i, j));
                }
            }
        }
        None
    }

    /// Require frames `i` and `j` to differ in some latch.
    pub fn differ(&mut self, i: usize, j: usize) {
        let n = self.g.latches.len();
        self.sat.reserve(n as i32);
        let mut any = Vec::with_capacity(n);
        for l in &self.g.latches {
            let (x, y) = (self.lit(i, l.lit), self.lit(j, l.lit));
            let d = self.fresh();
            self.sat.add_clause(&[-d, x, y]);
            self.sat.add_clause(&[-d, -x, -y]);
            any.push(d);
        }
        self.sat.add_clause(&any);
    }
}
