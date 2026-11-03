//! The bit-level model every bit-level engine runs on: an and-inverter
//! graph (AIG) with latches, bad-state and constraint outputs.
//!
//! Models come from AIGER files ([`aiger`]) or from Btor2 by bit-blasting
//! ([`btor`]), and are simplified by [`preprocess`] (cone of influence,
//! sequential constant propagation, structural hashing) before an engine
//! sees them, the way rIC3 does its own simplification in Rust.
//!
//! Literals are AIGER-style: `2 * var + negated`; variable 0 is constant
//! false, so literal 0 is false and 1 is true. Every AND node's inputs are
//! smaller variables, so node order is a topological order.

pub mod aiger;
pub mod btor;
pub mod preprocess;
pub mod words;

use std::collections::HashMap;
use std::hash::BuildHasherDefault;

pub type Lit = u32;
pub const FALSE: Lit = 0;
pub const TRUE: Lit = 1;

#[inline]
pub fn var(l: Lit) -> u32 {
    l >> 1
}
#[inline]
pub fn neg(l: Lit) -> Lit {
    l ^ 1
}
#[inline]
pub fn is_neg(l: Lit) -> bool {
    l & 1 == 1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Node {
    Const,
    Input,
    Latch,
    And(Lit, Lit),
}

/// A latch's value in the first frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Init {
    Zero,
    One,
    /// Any value (AIGER 1.9 uninitialized latch).
    Free,
    /// The value of a combinational literal in frame 0 (Btor2 `init` with a
    /// non-constant value); not expressible in AIGER.
    Expr(Lit),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Latch {
    /// The latch's own (positive) literal.
    pub lit: Lit,
    pub next: Lit,
    pub init: Init,
}

/// Deterministic hashing (fixed keys), so runs are reproducible.
type Fixed = BuildHasherDefault<std::collections::hash_map::DefaultHasher>;

#[derive(Debug, Clone, Default)]
pub struct Aig {
    pub nodes: Vec<Node>,
    pub inputs: Vec<Lit>,
    pub latches: Vec<Latch>,
    pub bads: Vec<Lit>,
    pub constraints: Vec<Lit>,
    /// Names for inputs and latches, by variable (when known).
    pub names: HashMap<u32, String, Fixed>,
    strash: HashMap<(Lit, Lit), Lit, Fixed>,
}

impl Aig {
    pub fn new() -> Self {
        Aig {
            nodes: vec![Node::Const],
            ..Default::default()
        }
    }

    pub fn num_vars(&self) -> u32 {
        self.nodes.len() as u32
    }

    pub fn num_ands(&self) -> usize {
        self.nodes
            .iter()
            .filter(|n| matches!(n, Node::And(..)))
            .count()
    }

    pub fn input(&mut self) -> Lit {
        let l = 2 * self.num_vars();
        self.nodes.push(Node::Input);
        self.inputs.push(l);
        l
    }

    /// A new latch; set its next state with [`Aig::set_next`].
    pub fn latch(&mut self, init: Init) -> Lit {
        let l = 2 * self.num_vars();
        self.nodes.push(Node::Latch);
        self.latches.push(Latch {
            lit: l,
            next: FALSE,
            init,
        });
        l
    }

    pub fn set_next(&mut self, latch: Lit, next: Lit) {
        let l = self
            .latches
            .iter_mut()
            .find(|x| x.lit == latch)
            .expect("not a latch");
        l.next = next;
    }

    /// AND with constant folding and structural hashing.
    pub fn and(&mut self, a: Lit, b: Lit) -> Lit {
        if a == FALSE || b == FALSE || a == neg(b) {
            return FALSE;
        }
        if a == TRUE || a == b {
            return b;
        }
        if b == TRUE {
            return a;
        }
        let key = if a < b { (a, b) } else { (b, a) };
        if let Some(&l) = self.strash.get(&key) {
            return l;
        }
        let l = 2 * self.num_vars();
        self.nodes.push(Node::And(key.0, key.1));
        self.strash.insert(key, l);
        l
    }

    /// Add an AND node exactly as given (readers that must keep variable
    /// numbering); still recorded for structural hashing.
    pub(crate) fn raw_and(&mut self, a: Lit, b: Lit) -> Lit {
        let l = 2 * self.num_vars();
        self.nodes.push(Node::And(a, b));
        self.strash
            .entry(if a < b { (a, b) } else { (b, a) })
            .or_insert(l);
        l
    }

    pub fn or(&mut self, a: Lit, b: Lit) -> Lit {
        neg(self.and(neg(a), neg(b)))
    }

    pub fn xor(&mut self, a: Lit, b: Lit) -> Lit {
        if a == FALSE {
            return b;
        }
        if b == FALSE {
            return a;
        }
        if a == TRUE {
            return neg(b);
        }
        if b == TRUE {
            return neg(a);
        }
        if a == b {
            return FALSE;
        }
        if a == neg(b) {
            return TRUE;
        }
        let x = self.and(a, neg(b));
        let y = self.and(neg(a), b);
        self.or(x, y)
    }

    pub fn ite(&mut self, c: Lit, a: Lit, b: Lit) -> Lit {
        if c == TRUE || a == b {
            return a;
        }
        if c == FALSE {
            return b;
        }
        let x = self.and(c, a);
        let y = self.and(neg(c), b);
        self.or(x, y)
    }

    pub fn and_all(&mut self, ls: &[Lit]) -> Lit {
        ls.iter().fold(TRUE, |acc, &l| self.and(acc, l))
    }

    pub fn or_all(&mut self, ls: &[Lit]) -> Lit {
        ls.iter().fold(FALSE, |acc, &l| self.or(acc, l))
    }

    pub fn is_latch(&self, l: Lit) -> bool {
        matches!(self.nodes.get(var(l) as usize), Some(Node::Latch))
    }

    pub fn summary(&self) -> String {
        format!(
            "inputs {} latches {} ands {} bads {} constraints {}",
            self.inputs.len(),
            self.latches.len(),
            self.num_ands(),
            self.bads.len(),
            self.constraints.len()
        )
    }

    /// Evaluate every node for one frame, given input and latch values.
    pub fn eval(&self, mut leaf: impl FnMut(Lit) -> bool) -> Vec<bool> {
        let mut v = vec![false; self.nodes.len()];
        for (i, n) in self.nodes.iter().enumerate() {
            v[i] = match *n {
                Node::Const => false,
                Node::Input | Node::Latch => leaf(2 * i as u32),
                Node::And(a, b) => lit_val(&v, a) && lit_val(&v, b),
            };
        }
        v
    }
}

#[inline]
pub fn lit_val(vals: &[bool], l: Lit) -> bool {
    vals[var(l) as usize] ^ is_neg(l)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_and_hashing() {
        let mut g = Aig::new();
        let (a, b) = (g.input(), g.input());
        let x = g.and(a, b);
        assert_eq!(g.and(b, a), x);
        assert_eq!(g.and(a, neg(a)), FALSE);
        assert_eq!(g.and(a, TRUE), a);
        let y = g.xor(a, b);
        let vals = g.eval(|l| l == a);
        assert!(lit_val(&vals, y) && !lit_val(&vals, x));
    }
}
