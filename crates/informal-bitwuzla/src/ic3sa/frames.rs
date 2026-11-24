//! Word-level IC3/PDR frame manager, CTI extraction, UNSAT-core + MIC
//! generalization, and clause propagation over Bitwuzla.

#[cfg(informal_bitwuzla)]
use crate::ffi::{Bz, T};
#[cfg(informal_bitwuzla)]
use crate::ic3sa::certify::certify_cex;
#[cfg(informal_bitwuzla)]
use crate::ic3sa::wts::{fresh_sorts, Atom, Cube, FrameEnv, Lit, Wts};
#[cfg(informal_bitwuzla)]
use informal_engine::{Progress, RunContext};
#[cfg(informal_bitwuzla)]
use informal_ir::Model;
#[cfg(informal_bitwuzla)]
use std::cmp::Reverse;
#[cfg(informal_bitwuzla)]
use std::collections::{BinaryHeap, HashSet};
#[cfg(informal_bitwuzla)]
use std::sync::atomic::AtomicBool;
#[cfg(informal_bitwuzla)]
use std::time::Instant;

#[cfg(informal_bitwuzla)]
pub(crate) enum Ic3Outcome {
    Safe { invariant: Vec<Cube> },
    Unsafe { depth: u32, witness: Option<String> },
    Spurious,
    Stopped,
}

#[cfg(informal_bitwuzla)]
#[derive(Clone, Eq, PartialEq)]
struct Obligation {
    level: usize,
    depth: usize,
    seq: usize,
    cube: Cube,
}

#[cfg(informal_bitwuzla)]
impl Ord for Obligation {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Min-heap via Reverse in caller; sort by (level, depth, seq).
        (self.level, self.depth, self.seq).cmp(&(other.level, other.depth, other.seq))
    }
}

#[cfg(informal_bitwuzla)]
impl PartialOrd for Obligation {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(informal_bitwuzla)]
pub(crate) struct Ic3Solver<'a> {
    m: &'a Model,
    wts: &'a Wts,
    ctx: &'a RunContext,
    cancel: &'a AtomicBool,
    name: &'a str,

    sol_init: Bz,
    env_init: FrameEnv,

    sol_state: Bz,
    env_state: FrameEnv,
    state_acts: Vec<T>,
    state_act_inf: T,

    sol_trans: Bz,
    env_curr: FrameEnv,
    env_next: FrameEnv,
    trans_acts: Vec<T>,
    trans_act_inf: T,
    trans_act_init: T,

    /// Delta-encoded frames: `frames[i]` stores blocked cubes whose highest
    /// proven frame is `i` (`i >= 1`). `frames[0]` is unused (Init is `act_init`).
    frames: Vec<Vec<Cube>>,
    inf_clauses: Vec<Cube>,
    seq: usize,
}

#[cfg(informal_bitwuzla)]
impl<'a> Ic3Solver<'a> {
    pub(crate) fn new(
        m: &'a Model,
        wts: &'a Wts,
        ctx: &'a RunContext,
        left_ms: i64,
        cancel: &'a AtomicBool,
        name: &'a str,
    ) -> anyhow::Result<Self> {
        // 1. Init solver: asserts Init(s) & C(s, i).
        let sol_init = Bz::new_ic3(left_ms, cancel);
        let sorts_i = fresh_sorts(m, &sol_init)?;
        let env_init = FrameEnv::encode(m, wts, &sol_init, &sorts_i, None, None, "i0")?;
        env_init.assert_init(m, wts, &sol_init)?;
        env_init.assert_constraints(m, &sol_init)?;

        // 2. 1-frame state solver: asserts C(s, i) + frame clauses for F_k & Bad_P checks.
        let sol_state = Bz::new_ic3(left_ms, cancel);
        let sorts_s = fresh_sorts(m, &sol_state)?;
        let env_state = FrameEnv::encode(m, wts, &sol_state, &sorts_s, None, None, "s0")?;
        env_state.assert_constraints(m, &sol_state)?;
        let bv1_s = sol_state.bv_sort(1)?;
        let state_act_inf = sol_state.konst(bv1_s, "_act_inf")?;

        // 3. 2-frame transition solver: asserts C(s, i), T(s, i, s'), p' == p, C(s', i').
        let sol_trans = Bz::new_ic3(left_ms, cancel);
        let sorts_t = fresh_sorts(m, &sol_trans)?;
        let env_curr = FrameEnv::encode(m, wts, &sol_trans, &sorts_t, None, None, "t0")?;
        env_curr.assert_constraints(m, &sol_trans)?;
        let next_map = env_curr.next_state_terms(m, wts, &sol_trans, "t1")?;
        let env_next = FrameEnv::encode(
            m,
            wts,
            &sol_trans,
            &sorts_t,
            Some(&next_map),
            Some(&env_curr.prophecies),
            "t1",
        )?;
        env_next.assert_constraints(m, &sol_trans)?;
        let bv1_t = sol_trans.bv_sort(1)?;
        let trans_act_inf = sol_trans.konst(bv1_t, "_act_inf")?;
        let trans_act_init = sol_trans.konst(bv1_t, "_act_init")?;

        // Guard Init(s) on `trans_act_init`: act_init => Init(s).
        for (&s, &v) in &wts.init {
            let st = env_curr.nodes[&s];
            let v_node = v.unsigned_abs() as u32;
            let mut val = env_curr.nodes[&v_node];
            if v < 0 {
                val = sol_trans.op("not", &[val], 0, 0)?;
            }
            let ss = m.line(s).and_then(|l| l.sort).unwrap();
            if matches!(m.sort(ss), Some(informal_ir::Sort::Array { .. }))
                && m.line(v_node)
                    .and_then(|l| l.sort)
                    .is_some_and(|vs| m.width(vs).is_some())
            {
                val = sol_trans.const_array(env_curr.sorts[&ss], val)?;
            }
            let eq = sol_trans.op("eq", &[st, val], 0, 0)?;
            let imp = sol_trans.op("implies", &[trans_act_init, eq], 0, 0)?;
            sol_trans.assert(imp)?;
        }

        let mut solver = Ic3Solver {
            m,
            wts,
            ctx,
            cancel,
            name,
            sol_init,
            env_init,
            sol_state,
            env_state,
            state_acts: Vec::new(),
            state_act_inf,
            sol_trans,
            env_curr,
            env_next,
            trans_acts: Vec::new(),
            trans_act_inf,
            trans_act_init,
            frames: Vec::new(),
            inf_clauses: Vec::new(),
            seq: 0,
        };
        // Frame 0 placeholder + Frame 1 initial frontier.
        solver.push_frame()?;
        solver.push_frame()?;
        Ok(solver)
    }

    fn left_ms(&self) -> i64 {
        self.ctx
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .max(1) as i64
    }

    fn push_frame(&mut self) -> anyhow::Result<()> {
        let idx = self.frames.len();
        self.frames.push(Vec::new());
        let bv1_s = self.sol_state.bv_sort(1)?;
        let a_s = self.sol_state.konst(bv1_s, &format!("_fs_{idx}"))?;
        self.state_acts.push(a_s);
        let bv1_t = self.sol_trans.bv_sort(1)?;
        let a_t = self.sol_trans.konst(bv1_t, &format!("_ft_{idx}"))?;
        self.trans_acts.push(a_t);
        Ok(())
    }

    /// Run word-level IC3 until fixpoint (`Safe`), certified CEX (`Unsafe`),
    /// spurious abstraction failure (`Spurious`), or cancellation/timeout (`Stopped`).
    pub(crate) fn run(&mut self) -> anyhow::Result<Ic3Outcome> {
        // Depth 0 check: Init(s) & C(s, i) & Bad_P(s, p, i).
        match self.sol_init.solve(&[self.env_init.bad_p])? {
            10 => {
                return match certify_cex(self.m, 0, self.left_ms(), self.cancel) {
                    Ok(Some(witness)) => Ok(Ic3Outcome::Unsafe { depth: 0, witness }),
                    Ok(None) => Ok(Ic3Outcome::Spurious),
                    Err(_) => Ok(Ic3Outcome::Stopped),
                };
            }
            20 => {
                if self.wts.bv_states.is_empty() && self.wts.arr_states.is_empty() {
                    return Ok(Ic3Outcome::Safe {
                        invariant: Vec::new(),
                    });
                }
            }
            _ => return Ok(Ic3Outcome::Stopped),
        }

        let mut k = 1usize;
        loop {
            if self.ctx.should_stop() {
                return Ok(Ic3Outcome::Stopped);
            }
            self.ctx.report(Progress {
                engine: self.name.into(),
                bound: Some(k as u32),
                note: format!("ic3sa frame {k}"),
                output: None,
            });

            // Block all CTIs at frame k (`F_k & C & Bad_P`).
            loop {
                if self.ctx.should_stop() {
                    return Ok(Ic3Outcome::Stopped);
                }
                let mut assumes = Vec::with_capacity(self.state_acts.len() + 2);
                for &a in &self.state_acts[k..] {
                    assumes.push(a);
                }
                assumes.push(self.state_act_inf);
                assumes.push(self.env_state.bad_p);

                match self.sol_state.solve(&assumes)? {
                    20 => break,
                    10 => {
                        let cti =
                            extract_cube(self.m, self.wts, &self.sol_state, &mut self.env_state)?;
                        if cti.is_empty() {
                            return Ok(Ic3Outcome::Spurious);
                        }
                        if !self.is_init_blocked(&cti)? {
                            // Extracted cube intersects Init despite Init & Bad_P UNSAT:
                            // abstraction is too coarse (missing array prophecy).
                            return Ok(Ic3Outcome::Spurious);
                        }
                        match self.block_cube(cti, k)? {
                            BlockResult::Blocked => {}
                            BlockResult::Unsafe { depth, witness } => {
                                return Ok(Ic3Outcome::Unsafe { depth, witness });
                            }
                            BlockResult::Spurious => return Ok(Ic3Outcome::Spurious),
                            BlockResult::Stopped => return Ok(Ic3Outcome::Stopped),
                        }
                    }
                    _ => return Ok(Ic3Outcome::Stopped),
                }
            }

            // Open next frontier frame `k + 1` and propagate clauses.
            self.push_frame()?;
            if let Some(inv) = self.propagate(k)? {
                return Ok(Ic3Outcome::Safe { invariant: inv });
            }
            k += 1;
        }
    }

    fn is_init_blocked(&mut self, cube: &[Lit]) -> anyhow::Result<bool> {
        let mut lits = Vec::with_capacity(cube.len());
        for l in cube {
            lits.push(self.env_init.encode_lit(self.m, &self.sol_init, l)?);
        }
        match self.sol_init.solve(&lits)? {
            20 => Ok(true),
            10 => Ok(false),
            _ => anyhow::bail!("stopped"),
        }
    }

    /// Check `F_{level-1} & !cube(s) & T & cube(s')`.
    /// Returns `Ok(Some(core_indices))` when UNSAT (with indices of `cube`
    /// literals present in Bitwuzla's UNSAT assumptions), `Ok(None)` when SAT.
    fn rel_inductive_query(
        &mut self,
        level: usize,
        cube: &[Lit],
    ) -> anyhow::Result<Option<Vec<usize>>> {
        let mut assumes = Vec::with_capacity(self.trans_acts.len() + cube.len() + 3);
        if level <= 1 {
            assumes.push(self.trans_act_init);
        } else {
            for &a in &self.trans_acts[(level - 1)..] {
                assumes.push(a);
            }
        }
        assumes.push(self.trans_act_inf);

        // Self-strengthening `!cube(s)`.
        let not_cube = self.env_curr.encode_clause(self.m, &self.sol_trans, cube)?;
        assumes.push(not_cube);

        let mut next_lits = Vec::with_capacity(cube.len());
        for l in cube {
            let t = self.env_next.encode_lit(self.m, &self.sol_trans, l)?;
            next_lits.push(t);
            assumes.push(t);
        }

        match self.sol_trans.solve(&assumes)? {
            20 => {
                let mut core = Vec::new();
                for (i, &t) in next_lits.iter().enumerate() {
                    if self.sol_trans.is_unsat_assumption(t)? {
                        core.push(i);
                    }
                }
                Ok(Some(core))
            }
            10 => Ok(None),
            _ => anyhow::bail!("stopped"),
        }
    }

    /// Generalize `cube` relative to `level` using Bitwuzla UNSAT-core shrinking
    /// followed by reverse-order MIC (dropping concrete `StateEqConst` /
    /// `ArrayReadEqConst` literals first so general syntax predicates survive).
    fn generalize(
        &mut self,
        level: usize,
        cube: Cube,
        initial_core: Vec<usize>,
    ) -> anyhow::Result<Cube> {
        let mut cur = cube;

        // 1. Try UNSAT-core reduction first (keeping Init disjointness).
        if !initial_core.is_empty() && initial_core.len() < cur.len() {
            let mut sub: Cube = initial_core.iter().map(|&i| cur[i].clone()).collect();
            if self.is_init_blocked(&sub)? && self.rel_inductive_query(level, &sub)?.is_some() {
                cur = sub;
            } else {
                // Restore minimal Init-blocking literals from `cur` via sol_init's unsat core.
                let mut init_lits = Vec::with_capacity(cur.len());
                for l in &cur {
                    init_lits.push(self.env_init.encode_lit(self.m, &self.sol_init, l)?);
                }
                if self.sol_init.solve(&init_lits)? == 20 {
                    let mut keep: HashSet<usize> = initial_core.into_iter().collect();
                    for (i, &t) in init_lits.iter().enumerate() {
                        if self.sol_init.is_unsat_assumption(t)? {
                            keep.insert(i);
                        }
                    }
                    if keep.len() < cur.len() {
                        sub = cur
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| keep.contains(i))
                            .map(|(_, l)| l.clone())
                            .collect();
                        if !sub.is_empty()
                            && self.is_init_blocked(&sub)?
                            && self.rel_inductive_query(level, &sub)?.is_some()
                        {
                            cur = sub;
                        }
                    }
                }
            }
        }

        // 2. Reverse-order MIC: drop concrete value literals first, then syntax literals.
        let mut idx = cur.len();
        while idx > 0 && cur.len() > 1 {
            if self.ctx.should_stop() {
                break;
            }
            idx -= 1;
            let mut cand = cur.clone();
            cand.remove(idx);
            if self.is_init_blocked(&cand)? {
                if let Some(core) = self.rel_inductive_query(level, &cand)? {
                    cur = cand;
                    // Opportunistic core shrink inside MIC.
                    if !core.is_empty() && core.len() < cur.len() {
                        let shrunk: Cube = core.iter().map(|&i| cur[i].clone()).collect();
                        if self.is_init_blocked(&shrunk)?
                            && self.rel_inductive_query(level, &shrunk)?.is_some()
                        {
                            cur = shrunk;
                            idx = idx.min(cur.len());
                        }
                    }
                }
            }
        }
        Ok(cur)
    }

    fn add_blocked_cube(&mut self, level: usize, cube: Cube) -> anyhow::Result<()> {
        // Subsumption check: if an existing clause at >= level already subsumes `cube`, skip.
        for f in &self.frames[level..] {
            if f.iter().any(|c| is_subset(c, &cube)) {
                return Ok(());
            }
        }
        if self.inf_clauses.iter().any(|c| is_subset(c, &cube)) {
            return Ok(());
        }
        // Drop clauses in frames <= level that `cube` subsumes.
        for f in &mut self.frames[1..=level] {
            f.retain(|c| !is_subset(&cube, c));
        }

        // Assert `act_level => !cube(s)` in both `sol_state` and `sol_trans`.
        let cl_s = self
            .env_state
            .encode_clause(self.m, &self.sol_state, &cube)?;
        let imp_s = self
            .sol_state
            .op("implies", &[self.state_acts[level], cl_s], 0, 0)?;
        self.sol_state.assert(imp_s)?;

        let cl_t = self
            .env_curr
            .encode_clause(self.m, &self.sol_trans, &cube)?;
        let imp_t = self
            .sol_trans
            .op("implies", &[self.trans_acts[level], cl_t], 0, 0)?;
        self.sol_trans.assert(imp_t)?;

        self.frames[level].push(cube);
        Ok(())
    }

    fn block_cube(&mut self, root_cube: Cube, k: usize) -> anyhow::Result<BlockResult> {
        let mut pq: BinaryHeap<Reverse<Obligation>> = BinaryHeap::new();
        self.seq += 1;
        pq.push(Reverse(Obligation {
            level: k,
            depth: 0,
            seq: self.seq,
            cube: root_cube,
        }));

        let mut steps = 0usize;
        while let Some(Reverse(ob)) = pq.pop() {
            if self.ctx.should_stop() {
                return Ok(BlockResult::Stopped);
            }
            steps += 1;
            if steps > 4096 {
                return Ok(BlockResult::Spurious);
            }
            if ob.level == 0 {
                return match certify_cex(self.m, ob.depth, self.left_ms(), self.cancel) {
                    Ok(Some(witness)) => Ok(BlockResult::Unsafe {
                        depth: ob.depth as u32,
                        witness,
                    }),
                    Ok(None) => Ok(BlockResult::Spurious),
                    Err(_) => Ok(BlockResult::Stopped),
                };
            }

            let res = match self.rel_inductive_query(ob.level, &ob.cube) {
                Ok(r) => r,
                Err(_) => return Ok(BlockResult::Stopped),
            };
            match res {
                Some(core) => {
                    let gen = match self.generalize(ob.level, ob.cube.clone(), core) {
                        Ok(g) => g,
                        Err(_) => return Ok(BlockResult::Stopped),
                    };
                    // Push generalized lemma as high as possible up to frame `k`.
                    let mut push = ob.level;
                    while push < k {
                        match self.rel_inductive_query(push + 1, &gen) {
                            Ok(Some(_)) => push += 1,
                            Ok(None) => break,
                            Err(_) => return Ok(BlockResult::Stopped),
                        }
                    }
                    self.add_blocked_cube(push, gen)?;
                    if push < k {
                        self.seq += 1;
                        pq.push(Reverse(Obligation {
                            level: push + 1,
                            depth: ob.depth,
                            seq: self.seq,
                            cube: ob.cube,
                        }));
                    }
                }
                None => {
                    let pred = extract_cube(self.m, self.wts, &self.sol_trans, &mut self.env_curr)?;
                    if pred.is_empty() {
                        return Ok(BlockResult::Spurious);
                    }
                    self.seq += 1;
                    pq.push(Reverse(Obligation {
                        level: ob.level - 1,
                        depth: ob.depth + 1,
                        seq: self.seq,
                        cube: pred,
                    }));
                    self.seq += 1;
                    pq.push(Reverse(Obligation {
                        level: ob.level,
                        depth: ob.depth,
                        seq: self.seq,
                        cube: ob.cube,
                    }));
                }
            }
        }
        Ok(BlockResult::Blocked)
    }

    fn propagate(&mut self, k: usize) -> anyhow::Result<Option<Vec<Cube>>> {
        for i in 1..=k {
            if self.ctx.should_stop() {
                return Ok(None);
            }
            let clauses = std::mem::take(&mut self.frames[i]);
            let mut kept = Vec::new();
            for c in clauses {
                if self.ctx.should_stop() {
                    kept.push(c);
                    continue;
                }
                let pushes = match self.rel_inductive_query(i + 1, &c) {
                    Ok(Some(_)) => true,
                    Ok(None) => false,
                    Err(_) => {
                        kept.push(c);
                        continue;
                    }
                };
                if pushes {
                    self.add_blocked_cube(i + 1, c)?;
                } else {
                    kept.push(c);
                }
            }
            self.frames[i] = kept;
            if self.frames[i].is_empty() && !self.ctx.should_stop() {
                // Fixpoint: `F_i == F_{i+1}`; collect invariant clauses from frames > i.
                let mut inv = self.inf_clauses.clone();
                for f in &self.frames[(i + 1)..] {
                    inv.extend(f.iter().cloned());
                }
                return Ok(Some(inv));
            }
        }
        Ok(None)
    }
}

#[cfg(informal_bitwuzla)]
enum BlockResult {
    Blocked,
    Unsafe { depth: u32, witness: Option<String> },
    Spurious,
    Stopped,
}

#[cfg(informal_bitwuzla)]
fn is_subset(small: &[Lit], big: &[Lit]) -> bool {
    small.len() <= big.len() && small.iter().all(|l| big.contains(l))
}

/// Extract a state-only word-level cube from the current SAT model in `bz`.
/// Syntax / relational predicates are placed first; concrete value equalities
/// (`state == val`, `read(mem, p) == val`) are placed last so reverse-order MIC
/// eliminates concrete values whenever general predicates suffice.
#[cfg(informal_bitwuzla)]
fn extract_cube(m: &Model, wts: &Wts, bz: &Bz, env: &mut FrameEnv) -> anyhow::Result<Cube> {
    let mut cube = Vec::new();
    let mut seen: HashSet<Atom> = HashSet::new();

    // 1. General syntax & relational predicates first.
    for atom in &wts.syntax_atoms {
        let t = env.encode_atom(m, bz, atom)?;
        let bits = bz.get_value_bits(t)?;
        let is_one = bits.ends_with('1');
        if seen.insert(atom.clone()) {
            cube.push(Lit {
                atom: atom.clone(),
                neg: !is_one,
            });
        }
    }

    // 2. Prophecy-projected array read concrete equalities.
    for &(arr_id, idx_sort, _) in &wts.arr_states {
        for (p_idx, p) in wts.prophecies.iter().enumerate() {
            if p.sort_id != idx_sort {
                continue;
            }
            let r = bz.op("read", &[env.nodes[&arr_id], env.prophecies[p_idx]], 0, 0)?;
            let bits = bz.get_value_bits(r)?;
            let atom = Atom::ArrayReadEqConst {
                arr_id,
                p_idx,
                bits,
            };
            if seen.insert(atom.clone()) {
                cube.push(Lit::pos(atom));
            }
        }
    }

    // 3. Concrete BV state value equalities last (shed first by MIC).
    for &(state_id, width) in &wts.bv_states {
        if width == 1 {
            // Already represented as `Atom::Syntax { node: state_id, subst: None }`.
            continue;
        }
        let bits = bz.get_value_bits(env.nodes[&state_id])?;
        let atom = Atom::StateEqConst { state_id, bits };
        if seen.insert(atom.clone()) {
            cube.push(Lit::pos(atom));
        }
    }

    // Ensure abstract literals precede concrete literals cleanly.
    cube.sort_by_key(|l| l.is_concrete());
    Ok(cube)
}
