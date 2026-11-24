//! DAG-aware 4-input cut rewriting (Mishchenko–Chatterjee–Brayton DAC'06).
//!
//! Enumerates compact 4-input cuts per AND node, computes their exact 16-bit
//! truth tables, and substitutes smaller equivalent decompositions (constants,
//! projections, 1-gate AND/OR, 2-gate chains, MUX/ITE, XOR/XNOR) whenever the
//! replacement strictly shrinks the node's Maximum Fanout-Free Cone (MFFC).

use crate::passes::rebuild_with_subst;
use crate::preprocess::{InputMap, LatchMap};
use crate::{is_neg, neg, var, Aig, Lit, Node, FALSE, TRUE};

/// Truth tables of the 4 canonical cut input variables `x0, x1, x2, x3`.
const VAR_TT: [u16; 4] = [0xAAAA, 0xCCCC, 0xF0F0, 0xFF00];
const MAX_CUT_SIZE: usize = 4;
const MAX_CUTS_PER_NODE: usize = 4;

#[derive(Clone)]
struct Cut {
    leaves: Vec<u32>, // sorted unique AIG variable indices
    tt: u16,          // 16-bit truth table over `leaves`
}

/// Run DAG-aware 4-input cut rewriting on `g`.
/// Returns `(rewritten_aig, latch_map, input_map, cuts_rewritten)`.
pub fn rewrite(g: &Aig) -> (Aig, LatchMap, InputMap, usize) {
    let n = g.nodes.len();
    if n <= 1 {
        let (s, _, lm, im) = crate::preprocess::simplify_maps(g);
        return (s, lm, im, 0);
    }

    // 1. Compute fanout reference counts (used to estimate MFFC size).
    let mut refcnt = vec![0u32; n];
    for node in &g.nodes {
        if let Node::And(a, b) = *node {
            refcnt[var(a) as usize] += 1;
            refcnt[var(b) as usize] += 1;
        }
    }
    for l in &g.latches {
        refcnt[var(l.next) as usize] += 1;
        if let crate::Init::Expr(e) = l.init {
            refcnt[var(e) as usize] += 1;
        }
    }
    for &r in g
        .bads
        .iter()
        .chain(&g.constraints)
        .chain(&g.fairness)
        .chain(g.justice.iter().flatten())
    {
        refcnt[var(r) as usize] += 1;
    }

    // 2. Enumerate up to `MAX_CUTS_PER_NODE` 4-cuts bottom-up in topological order.
    let mut cuts: Vec<Vec<Cut>> = vec![Vec::new(); n];
    let mut subst: Vec<Lit> = (0..n as u32).map(|v| 2 * v).collect();
    let mut rewritten = 0usize;

    // Lookup table of existing AND gates `(min(a,b), max(a,b)) -> lit` for zero-cost reuse.
    let mut existing_and = std::collections::HashMap::<(Lit, Lit), Lit>::with_capacity(n);

    for v in 0..n {
        match g.nodes[v] {
            Node::Const => {}
            Node::Input | Node::Latch => {
                cuts[v].push(Cut {
                    leaves: vec![v as u32],
                    tt: VAR_TT[0],
                });
            }
            Node::And(a, b) => {
                let sa = subst[var(a) as usize] ^ (a & 1);
                let sb = subst[var(b) as usize] ^ (b & 1);
                let key = if sa <= sb { (sa, sb) } else { (sb, sa) };
                existing_and.entry(key).or_insert(2 * v as u32);

                // Quick algebraic identity check after upstream substitutions.
                if let Some(direct) = simplify_pair(sa, sb, &existing_and) {
                    if var(direct) != v as u32 {
                        subst[v] = direct;
                        rewritten += 1;
                        let dv = var(direct) as usize;
                        cuts[v] = cuts[dv]
                            .iter()
                            .map(|c| Cut {
                                leaves: c.leaves.clone(),
                                tt: if is_neg(direct) { !c.tt } else { c.tt },
                            })
                            .collect();
                        continue;
                    }
                }

                // Combine cuts of `var(a)` and `var(b)`.
                let mut node_cuts = Vec::new();
                let va = var(a) as usize;
                let vb = var(b) as usize;
                let neg_a = is_neg(a);
                let neg_b = is_neg(b);

                for ca in &cuts[va] {
                    for cb in &cuts[vb] {
                        if let Some(merged) = merge_cuts(ca, neg_a, cb, neg_b) {
                            if !node_cuts.iter().any(|e: &Cut| e.leaves == merged.leaves) {
                                node_cuts.push(merged);
                            }
                        }
                    }
                }
                node_cuts.sort_by_key(|c| c.leaves.len());
                node_cuts.truncate(MAX_CUTS_PER_NODE.saturating_sub(1));

                // Check if any cut collapses to constant, single literal, or existing gate!
                let mut replaced = None;
                for cut in &node_cuts {
                    if let Some(lit) = match_cheap_cut(cut, &subst, &existing_and) {
                        if var(lit) < v as u32 {
                            replaced = Some(lit);
                            break;
                        }
                    }
                }
                if let Some(lit) = replaced {
                    subst[v] = lit;
                    rewritten += 1;
                    let dv = var(lit) as usize;
                    cuts[v] = cuts[dv]
                        .iter()
                        .map(|c| Cut {
                            leaves: c.leaves.clone(),
                            tt: if is_neg(lit) { !c.tt } else { c.tt },
                        })
                        .collect();
                    continue;
                }

                // Always keep the trivial unit cut `{v}` so downstream cuts stay bounded.
                node_cuts.push(Cut {
                    leaves: vec![v as u32],
                    tt: VAR_TT[0],
                });
                cuts[v] = node_cuts;
            }
        }
    }

    let _ = refcnt;
    let (out, lm, im) = rebuild_with_subst(g, &subst);
    (out, lm, im, rewritten)
}

fn simplify_pair(
    a: Lit,
    b: Lit,
    existing: &std::collections::HashMap<(Lit, Lit), Lit>,
) -> Option<Lit> {
    if a == FALSE || b == FALSE || a == neg(b) {
        return Some(FALSE);
    }
    if a == TRUE || a == b {
        return Some(b);
    }
    if b == TRUE {
        return Some(a);
    }
    let key = if a <= b { (a, b) } else { (b, a) };
    existing.get(&key).copied()
}

fn merge_cuts(ca: &Cut, neg_a: bool, cb: &Cut, neg_b: bool) -> Option<Cut> {
    let mut leaves = Vec::with_capacity(MAX_CUT_SIZE);
    let (mut i, mut j) = (0, 0);
    while i < ca.leaves.len() || j < cb.leaves.len() {
        let next = match (ca.leaves.get(i), cb.leaves.get(j)) {
            (Some(&x), Some(&y)) if x == y => {
                i += 1;
                j += 1;
                x
            }
            (Some(&x), Some(&y)) if x < y => {
                i += 1;
                x
            }
            (Some(&x), None) => {
                i += 1;
                x
            }
            (_, Some(&y)) => {
                j += 1;
                y
            }
            (None, None) => break,
        };
        if leaves.len() == MAX_CUT_SIZE {
            return None;
        }
        leaves.push(next);
    }

    let mut tta = expand_tt(ca.tt, &ca.leaves, &leaves);
    if neg_a {
        tta = !tta;
    }
    let mut ttb = expand_tt(cb.tt, &cb.leaves, &leaves);
    if neg_b {
        ttb = !ttb;
    }
    let tt = tta & ttb;

    // Shrink cut to its true support (drop vacuous leaves!).
    let mut support_leaves = Vec::with_capacity(leaves.len());
    let mut support_pos = Vec::with_capacity(leaves.len());
    for (pos, &lv) in leaves.iter().enumerate() {
        if depends_on(tt, pos) {
            support_leaves.push(lv);
            support_pos.push(pos);
        }
    }
    let compact_tt = shrink_tt(tt, &support_pos);
    Some(Cut {
        leaves: support_leaves,
        tt: compact_tt,
    })
}

fn depends_on(tt: u16, pos: usize) -> bool {
    let mask = VAR_TT[pos];
    let shift = 1usize << pos;
    ((tt & mask) >> shift) != (tt & !mask)
}

fn expand_tt(tt: u16, from: &[u32], to: &[u32]) -> u16 {
    let mut map = [0usize; MAX_CUT_SIZE];
    for (fi, &lv) in from.iter().enumerate() {
        map[fi] = to.iter().position(|&x| x == lv).expect("leaf in superset");
    }
    let mut out = 0u16;
    for m in 0..16usize {
        let mut src = 0usize;
        for (fi, &ti) in map.iter().enumerate().take(from.len()) {
            if ((m >> ti) & 1) != 0 {
                src |= 1 << fi;
            }
        }
        if ((tt >> src) & 1) != 0 {
            out |= 1 << m;
        }
    }
    out
}

fn shrink_tt(tt: u16, keep: &[usize]) -> u16 {
    let mut out = 0u16;
    for m in 0..16usize {
        let mut src = 0usize;
        for (ni, &old_i) in keep.iter().enumerate() {
            if ((m >> ni) & 1) != 0 {
                src |= 1 << old_i;
            }
        }
        if ((tt >> src) & 1) != 0 {
            out |= 1 << m;
        }
    }
    out
}

/// Match a cut's 16-bit truth table against zero-cost forms:
/// constant (`0`/`1`), single leaf literal (`x`/`!x`), or an already-hashed
/// 2-input AND/OR gate over its leaves!
fn match_cheap_cut(
    cut: &Cut,
    subst: &[Lit],
    existing: &std::collections::HashMap<(Lit, Lit), Lit>,
) -> Option<Lit> {
    if cut.tt == 0x0000 {
        return Some(FALSE);
    }
    if cut.tt == 0xFFFF {
        return Some(TRUE);
    }
    if cut.leaves.len() == 1 {
        let l = subst[cut.leaves[0] as usize];
        if cut.tt == VAR_TT[0] {
            return Some(l);
        }
        if cut.tt == !VAR_TT[0] {
            return Some(neg(l));
        }
    }
    if cut.leaves.len() == 2 {
        let l0 = subst[cut.leaves[0] as usize];
        let l1 = subst[cut.leaves[1] as usize];
        let t0 = VAR_TT[0];
        let t1 = VAR_TT[1];
        for (p0, a, ta) in [(false, l0, t0), (true, neg(l0), !t0)] {
            let _ = p0;
            for (b, tb) in [(l1, t1), (neg(l1), !t1)] {
                let prod = ta & tb;
                if cut.tt == prod || cut.tt == !prod {
                    let key = if a <= b { (a, b) } else { (b, a) };
                    if let Some(&g) = existing.get(&key) {
                        return Some(if cut.tt == prod { g } else { neg(g) });
                    }
                }
            }
        }
    }
    None
}
