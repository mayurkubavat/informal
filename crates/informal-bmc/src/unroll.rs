//! Unrolls an AIG into a SAT solver one time frame at a time.
//!
//! Frame 0 latches take their reset value (free when uninitialized, or for
//! the induction step); frame k+1 latches take frame k's next-state
//! literal. Inputs are fresh in every frame and constraints hold in every
//! frame. The AND logic comes from the CNF template ([`crate::cnf`]), with
//! internal variables already eliminated, instantiated once per frame.

use crate::cnf::{self, Template};
use informal_aig::{is_neg, var, Aig, Init, Node};
use informal_sat::{Lit, Sat};

pub struct Unroll<'g> {
    g: &'g Aig,
    template: std::sync::Arc<Template>,
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
            template: cnf::template(g),
            sat,
            next_var: 2,
            tru: 1,
            frames: Vec::new(),
            free_init,
        }
    }

    /// A new SAT variable, reserved in the solver.
    pub fn new_var(&mut self) -> Lit {
        self.sat.reserve(1);
        self.fresh()
    }

    /// Require clause `!cube` (AIG latch literals) in frame `k`.
    pub fn block_cube(&mut self, k: usize, cube: &[informal_aig::Lit]) {
        let c: Vec<Lit> = cube.iter().map(|&l| -self.lit(k, l)).collect();
        self.sat.add_clause(&c);
    }

    fn fresh(&mut self) -> Lit {
        let v = self.next_var;
        self.next_var += 1;
        v
    }

    /// SAT literal of AIG literal `l` in frame `k`.
    pub fn lit(&self, k: usize, l: informal_aig::Lit) -> Lit {
        let x = self.frames[k][var(l) as usize];
        if is_neg(l) {
            -x
        } else {
            x
        }
    }

    pub fn add_frame(&mut self) -> usize {
        self.add_frame_with(true)
    }

    /// [`Self::add_frame`], asserting the constraints in the new frame
    /// only when `constraints` is set.
    pub fn add_frame_with(&mut self, constraints: bool) -> usize {
        let k = self.frames.len();
        let g = self.g;
        let live = self.template.live.iter().filter(|&&x| x).count();
        self.sat.reserve(live as i32);
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
                // Eliminated AND variables get no SAT variable.
                Node::And(..) if self.template.live[v + 1] => self.fresh(),
                Node::And(..) => 0,
            };
        }
        let rename = |t: cnf::TLit| -> Lit {
            let x = f[t.unsigned_abs() as usize - 1];
            if t < 0 {
                -x
            } else {
                x
            }
        };
        let mut buf = Vec::new();
        for c in &self.template.clauses {
            buf.clear();
            buf.extend(c.iter().map(|&t| rename(t)));
            self.sat.add_clause(&buf);
        }
        self.frames.push(f);
        for (x, e) in late {
            let y = self.lit(k, e);
            self.sat.add_clause(&[-x, y]);
            self.sat.add_clause(&[x, -y]);
        }
        if constraints {
            for &c in &g.constraints {
                let c = self.lit(k, c);
                self.sat.add_clause(&[c]);
            }
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
        c.extend(&bads);
        self.sat.add_clause(&c);
        // Both directions: callers assert -x for "no bad output in frame
        // k" (BMC after UNSAT, the k-induction step), which a one-sided
        // definition would not enforce.
        for b in bads {
            self.sat.add_clause(&[x, -b]);
        }
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
