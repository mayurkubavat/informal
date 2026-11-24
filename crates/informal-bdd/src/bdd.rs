//! Zero-dependency pure-Rust complemented-edge ROBDD manager.
//!
//! Representation (Brace–Rudell–Bryant):
//! - `Bdd = u32`: bit 0 is the complement flag (`neg(f) = f ^ 1`), bits `31..1`
//!   index `nodes[f >> 1]`. Node `0` is the terminal (`FALSE = 0`, `TRUE = 1`).
//! - Canonical invariant: every internal node stores `lo != hi` and an
//!   **uncomplemented** low edge `(lo & 1) == 0`. Negation is `O(1)`, equality
//!   is integer equality, and variables are ordered directly by `var: u32`
//!   (`0` closest to the root).
//! - Every recursive operation polls [`RunContext::should_stop`] and a node
//!   ceiling every 1,024 steps so cancellation completes well under 1 s even on
//!   hard arithmetic models.

use informal_engine::RunContext;
use std::collections::HashMap;

/// Complemented-edge BDD handle (`0 = FALSE`, `1 = TRUE`).
pub type Bdd = u32;

/// A product cube of `(bdd_var, polarity)` literals.
pub type Cube = Vec<(u32, bool)>;

pub const FALSE: Bdd = 0;
pub const TRUE: Bdd = 1;

#[inline]
pub const fn neg(f: Bdd) -> Bdd {
    f ^ 1
}

#[inline]
pub const fn is_const(f: Bdd) -> bool {
    (f >> 1) == 0
}

#[inline]
const fn node_idx(f: Bdd) -> usize {
    (f >> 1) as usize
}

/// Reason a BDD operation stopped early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Abort {
    Stopped,
    NodeLimit,
    CubeLimit,
}

#[derive(Clone, Copy)]
struct Node {
    var: u32,
    lo: Bdd,
    hi: Bdd,
}

#[derive(Clone, Copy)]
struct BinEntry {
    a: Bdd,
    b: Bdd,
    res: Bdd,
}

impl Default for BinEntry {
    fn default() -> Self {
        Self {
            a: u32::MAX,
            b: u32::MAX,
            res: FALSE,
        }
    }
}

#[derive(Clone, Copy)]
struct TriEntry {
    a: Bdd,
    b: Bdd,
    c: Bdd,
    res: Bdd,
}

impl Default for TriEntry {
    fn default() -> Self {
        Self {
            a: u32::MAX,
            b: u32::MAX,
            c: u32::MAX,
            res: FALSE,
        }
    }
}

const BIN_CACHE_BITS: usize = 15; // 32,768 entries
const BIN_CACHE_MASK: usize = (1 << BIN_CACHE_BITS) - 1;
const TRI_CACHE_BITS: usize = 15; // 32,768 entries
const TRI_CACHE_MASK: usize = (1 << TRI_CACHE_BITS) - 1;

#[inline]
fn hash2(a: u32, b: u32) -> usize {
    let mut x = (a as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (b as u64);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    (x ^ (x >> 27)) as usize
}

#[inline]
fn hash3(a: u32, b: u32, c: u32) -> usize {
    let mut x = (a as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add((b as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9))
        ^ (c as u64).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 30;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    (x ^ (x >> 27)) as usize
}

/// Complemented-edge ROBDD manager bound to a [`RunContext`] for cancellation
/// and memory checks.
pub struct BddMgr<'ctx> {
    nodes: Vec<Node>,
    unique: HashMap<(u32, Bdd, Bdd), u32>,
    and_cache: Vec<BinEntry>,
    xor_cache: Vec<BinEntry>,
    ite_cache: Vec<TriEntry>,
    ex_cache: Vec<BinEntry>,
    rel_cache: Vec<TriEntry>,
    ctx: &'ctx RunContext,
    max_nodes: usize,
    ops: u64,
    next_qid: u32,
}

impl<'ctx> BddMgr<'ctx> {
    pub fn new(ctx: &'ctx RunContext, max_nodes: usize) -> Self {
        let nodes = vec![Node {
            var: u32::MAX,
            lo: FALSE,
            hi: FALSE,
        }];
        Self {
            nodes,
            unique: HashMap::with_capacity(4096),
            and_cache: vec![BinEntry::default(); 1 << BIN_CACHE_BITS],
            xor_cache: vec![BinEntry::default(); 1 << BIN_CACHE_BITS],
            ite_cache: vec![TriEntry::default(); 1 << TRI_CACHE_BITS],
            ex_cache: vec![BinEntry::default(); 1 << BIN_CACHE_BITS],
            rel_cache: vec![TriEntry::default(); 1 << TRI_CACHE_BITS],
            ctx,
            max_nodes,
            ops: 0,
            next_qid: 1,
        }
    }

    pub fn num_nodes(&self) -> usize {
        self.nodes.len()
    }

    /// Allocate a fresh cache tag for a distinct variable-quantification mask.
    pub fn fresh_quant_id(&mut self) -> u32 {
        let id = self.next_qid;
        self.next_qid += 1;
        id
    }

    #[inline]
    fn tick(&mut self) -> Result<(), Abort> {
        self.ops += 1;
        if (self.ops & 0x3FF) == 0 && self.ctx.should_stop() {
            return Err(Abort::Stopped);
        }
        Ok(())
    }

    #[inline]
    pub fn var_of(&self, f: Bdd) -> u32 {
        self.nodes[node_idx(f)].var
    }

    #[inline]
    fn cofactors(&self, f: Bdd, v: u32) -> (Bdd, Bdd) {
        let n = self.nodes[node_idx(f)];
        if n.var > v {
            (f, f)
        } else {
            let c = f & 1;
            (n.lo ^ c, n.hi ^ c)
        }
    }

    /// Create or look up a canonical node `ITE(var, hi, lo)`.
    pub fn mk(&mut self, var: u32, lo: Bdd, hi: Bdd) -> Result<Bdd, Abort> {
        if lo == hi {
            return Ok(lo);
        }
        let flip = lo & 1;
        let lo_c = lo ^ flip;
        let hi_c = hi ^ flip;
        if let Some(&edge) = self.unique.get(&(var, lo_c, hi_c)) {
            return Ok(edge ^ flip);
        }
        if self.nodes.len() >= self.max_nodes {
            return Err(Abort::NodeLimit);
        }
        let idx = self.nodes.len() as u32;
        self.nodes.push(Node {
            var,
            lo: lo_c,
            hi: hi_c,
        });
        let edge = idx << 1;
        self.unique.insert((var, lo_c, hi_c), edge);
        Ok(edge ^ flip)
    }

    /// Positive literal BDD for variable `v`.
    pub fn var(&mut self, v: u32) -> Result<Bdd, Abort> {
        self.mk(v, FALSE, TRUE)
    }

    /// Logical conjunction `a & b`.
    pub fn and(&mut self, a: Bdd, b: Bdd) -> Result<Bdd, Abort> {
        if a == FALSE || b == FALSE {
            return Ok(FALSE);
        }
        if a == TRUE {
            return Ok(b);
        }
        if b == TRUE || a == b {
            return Ok(a);
        }
        if a == neg(b) {
            return Ok(FALSE);
        }
        let (a, b) = if a < b { (a, b) } else { (b, a) };
        let slot = hash2(a, b) & BIN_CACHE_MASK;
        let hit = self.and_cache[slot];
        if hit.a == a && hit.b == b {
            return Ok(hit.res);
        }
        self.tick()?;
        let v = self.var_of(a).min(self.var_of(b));
        let (a0, a1) = self.cofactors(a, v);
        let (b0, b1) = self.cofactors(b, v);
        let lo = self.and(a0, b0)?;
        let hi = self.and(a1, b1)?;
        let res = self.mk(v, lo, hi)?;
        self.and_cache[slot] = BinEntry { a, b, res };
        Ok(res)
    }

    /// Logical disjunction `a | b`.
    #[inline]
    pub fn or(&mut self, a: Bdd, b: Bdd) -> Result<Bdd, Abort> {
        Ok(neg(self.and(neg(a), neg(b))?))
    }

    /// Logical exclusive-OR `a ^ b`.
    pub fn xor(&mut self, a: Bdd, b: Bdd) -> Result<Bdd, Abort> {
        if a == FALSE {
            return Ok(b);
        }
        if a == TRUE {
            return Ok(neg(b));
        }
        if b == FALSE {
            return Ok(a);
        }
        if b == TRUE {
            return Ok(neg(a));
        }
        if a == b {
            return Ok(FALSE);
        }
        if a == neg(b) {
            return Ok(TRUE);
        }
        let pol = (a ^ b) & 1;
        let (x, y) = (a & !1, b & !1);
        let (x, y) = if x < y { (x, y) } else { (y, x) };
        let slot = hash2(x, y) & BIN_CACHE_MASK;
        let hit = self.xor_cache[slot];
        if hit.a == x && hit.b == y {
            return Ok(hit.res ^ pol);
        }
        self.tick()?;
        let v = self.var_of(x).min(self.var_of(y));
        let (x0, x1) = self.cofactors(x, v);
        let (y0, y1) = self.cofactors(y, v);
        let lo = self.xor(x0, y0)?;
        let hi = self.xor(x1, y1)?;
        let res = self.mk(v, lo, hi)?;
        self.xor_cache[slot] = BinEntry { a: x, b: y, res };
        Ok(res ^ pol)
    }

    /// Logical equivalence `a <=> b`.
    #[inline]
    pub fn xnor(&mut self, a: Bdd, b: Bdd) -> Result<Bdd, Abort> {
        Ok(neg(self.xor(a, b)?))
    }

    /// If-then-else `ITE(c, t, e)`.
    pub fn ite(&mut self, c: Bdd, t: Bdd, e: Bdd) -> Result<Bdd, Abort> {
        if c == TRUE {
            return Ok(t);
        }
        if c == FALSE {
            return Ok(e);
        }
        if t == e {
            return Ok(t);
        }
        if t == TRUE {
            return self.or(c, e);
        }
        if e == FALSE {
            return self.and(c, t);
        }
        if t == FALSE {
            return self.and(neg(c), e);
        }
        if e == TRUE {
            return self.or(neg(c), t);
        }
        let (c, t, e) = if (c & 1) != 0 {
            (neg(c), e, t)
        } else {
            (c, t, e)
        };
        let slot = hash3(c, t, e) & TRI_CACHE_MASK;
        let hit = self.ite_cache[slot];
        if hit.a == c && hit.b == t && hit.c == e {
            return Ok(hit.res);
        }
        self.tick()?;
        let v = self.var_of(c).min(self.var_of(t)).min(self.var_of(e));
        let (c0, c1) = self.cofactors(c, v);
        let (t0, t1) = self.cofactors(t, v);
        let (e0, e1) = self.cofactors(e, v);
        let lo = self.ite(c0, t0, e0)?;
        let hi = self.ite(c1, t1, e1)?;
        let res = self.mk(v, lo, hi)?;
        self.ite_cache[slot] = TriEntry {
            a: c,
            b: t,
            c: e,
            res,
        };
        Ok(res)
    }

    /// Existential quantification `∃ V. f` over all variables with `mask[v] == true`.
    pub fn exists(&mut self, f: Bdd, mask: &[bool], qid: u32) -> Result<Bdd, Abort> {
        if is_const(f) {
            return Ok(f);
        }
        let slot = hash2(f, qid) & BIN_CACHE_MASK;
        let hit = self.ex_cache[slot];
        if hit.a == f && hit.b == qid {
            return Ok(hit.res);
        }
        self.tick()?;
        let v = self.var_of(f);
        let (f0, f1) = self.cofactors(f, v);
        let res = if mask.get(v as usize).copied().unwrap_or(false) {
            let lo = self.exists(f0, mask, qid)?;
            if lo == TRUE {
                TRUE
            } else {
                let hi = self.exists(f1, mask, qid)?;
                self.or(lo, hi)?
            }
        } else {
            let lo = self.exists(f0, mask, qid)?;
            let hi = self.exists(f1, mask, qid)?;
            self.mk(v, lo, hi)?
        };
        self.ex_cache[slot] = BinEntry { a: f, b: qid, res };
        Ok(res)
    }

    /// Fused relational product `∃ V. (a & b)` in one top-down pass.
    pub fn and_exists(&mut self, a: Bdd, b: Bdd, mask: &[bool], qid: u32) -> Result<Bdd, Abort> {
        if a == FALSE || b == FALSE {
            return Ok(FALSE);
        }
        if a == TRUE {
            return self.exists(b, mask, qid);
        }
        if b == TRUE || a == b {
            return self.exists(a, mask, qid);
        }
        if a == neg(b) {
            return Ok(FALSE);
        }
        let (a, b) = if a < b { (a, b) } else { (b, a) };
        let slot = hash3(a, b, qid) & TRI_CACHE_MASK;
        let hit = self.rel_cache[slot];
        if hit.a == a && hit.b == b && hit.c == qid {
            return Ok(hit.res);
        }
        self.tick()?;
        let v = self.var_of(a).min(self.var_of(b));
        let (a0, a1) = self.cofactors(a, v);
        let (b0, b1) = self.cofactors(b, v);
        let res = if mask.get(v as usize).copied().unwrap_or(false) {
            let lo = self.and_exists(a0, b0, mask, qid)?;
            if lo == TRUE {
                TRUE
            } else {
                let hi = self.and_exists(a1, b1, mask, qid)?;
                self.or(lo, hi)?
            }
        } else {
            let lo = self.and_exists(a0, b0, mask, qid)?;
            let hi = self.and_exists(a1, b1, mask, qid)?;
            self.mk(v, lo, hi)?
        };
        self.rel_cache[slot] = TriEntry { a, b, c: qid, res };
        Ok(res)
    }

    /// Rename every variable `v` in `f` to `map[v]`, assuming `map` preserves the
    /// strict relative variable ordering of all variables occurring in `f`.
    pub fn rename_preserving_order(&mut self, f: Bdd, map: &[u32]) -> Result<Bdd, Abort> {
        let mut memo = HashMap::new();
        self.rename_rec(f, map, &mut memo)
    }

    fn rename_rec(
        &mut self,
        f: Bdd,
        map: &[u32],
        memo: &mut HashMap<Bdd, Bdd>,
    ) -> Result<Bdd, Abort> {
        if is_const(f) {
            return Ok(f);
        }
        let pol = f & 1;
        let fp = f & !1;
        if let Some(&cached) = memo.get(&fp) {
            return Ok(cached ^ pol);
        }
        self.tick()?;
        let n = self.nodes[node_idx(fp)];
        let lo = self.rename_rec(n.lo, map, memo)?;
        let hi = self.rename_rec(n.hi, map, memo)?;
        let res = self.mk(map[n.var as usize], lo, hi)?;
        memo.insert(fp, res);
        Ok(res ^ pol)
    }

    /// Collect all variables appearing in `f`'s support.
    pub fn support(&self, f: Bdd, num_vars: usize) -> Vec<bool> {
        let mut seen = HashMap::new();
        let mut sup = vec![false; num_vars];
        let mut stack = vec![f & !1];
        while let Some(u) = stack.pop() {
            if is_const(u) || seen.insert(u, ()).is_some() {
                continue;
            }
            let n = self.nodes[node_idx(u)];
            if (n.var as usize) < num_vars {
                sup[n.var as usize] = true;
            }
            stack.push(n.lo & !1);
            stack.push(n.hi & !1);
        }
        sup
    }

    /// Return one satisfying partial assignment of `f`, or `None` when `f == FALSE`.
    pub fn any_sat(&self, mut f: Bdd) -> Option<Vec<(u32, bool)>> {
        if f == FALSE {
            return None;
        }
        let mut assign = Vec::new();
        while !is_const(f) {
            let n = self.nodes[node_idx(f)];
            let c = f & 1;
            let lo = n.lo ^ c;
            let hi = n.hi ^ c;
            if lo != FALSE {
                assign.push((n.var, false));
                f = lo;
            } else {
                assign.push((n.var, true));
                f = hi;
            }
        }
        debug_assert_eq!(f, TRUE);
        Some(assign)
    }

    /// Exact Minato–Morreale Irredundant Sum-of-Products (`ISOP`) cover of `f`.
    /// Returns prime-implicant cubes `(var, polarity)` whose disjunction equals
    /// `f`, or `Err(Abort::CubeLimit)` when the cover exceeds `max_cubes`.
    pub fn isop(&mut self, f: Bdd, max_cubes: usize) -> Result<Vec<Cube>, Abort> {
        let mut budget = max_cubes;
        let (_, cubes) = self.isop_rec(f, f, &mut budget)?;
        Ok(cubes)
    }

    fn isop_rec(
        &mut self,
        lo: Bdd,
        hi: Bdd,
        budget: &mut usize,
    ) -> Result<(Bdd, Vec<Cube>), Abort> {
        if lo == FALSE {
            return Ok((FALSE, Vec::new()));
        }
        if hi == TRUE {
            if *budget == 0 {
                return Err(Abort::CubeLimit);
            }
            *budget -= 1;
            return Ok((TRUE, vec![Vec::new()]));
        }
        self.tick()?;
        let v = self.var_of(lo).min(self.var_of(hi));
        let (l0, l1) = self.cofactors(lo, v);
        let (u0, u1) = self.cofactors(hi, v);

        let l0_only = self.and(l0, neg(u1))?;
        let (f0, mut c0) = self.isop_rec(l0_only, u0, budget)?;
        for cube in &mut c0 {
            cube.push((v, false));
        }

        let l1_only = self.and(l1, neg(u0))?;
        let (f1, mut c1) = self.isop_rec(l1_only, u1, budget)?;
        for cube in &mut c1 {
            cube.push((v, true));
        }

        let r0 = self.and(l0, neg(f0))?;
        let r1 = self.and(l1, neg(f1))?;
        let l_dc = self.or(r0, r1)?;
        let u_dc = self.and(u0, u1)?;
        let (f_dc, c_dc) = self.isop_rec(l_dc, u_dc, budget)?;

        let lo_cov = self.or(f0, f_dc)?;
        let hi_cov = self.or(f1, f_dc)?;
        let cov = self.mk(v, lo_cov, hi_cov)?;

        c0.extend(c1);
        c0.extend(c_dc);
        Ok((cov, c0))
    }
}
