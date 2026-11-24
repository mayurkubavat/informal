//! Word-level Transition System (`Wts`), syntax-guided predicate mining,
//! prophecy variables for unblasted arrays, and Bitwuzla frame encoding.

#[cfg(informal_bitwuzla)]
use crate::ffi::{build_sorts, const_bits, Bz, T};
#[cfg(informal_bitwuzla)]
use informal_ir::{Line, Model, Op, Sort};
#[cfg(informal_bitwuzla)]
use std::collections::{BTreeSet, HashMap, HashSet};

/// A prophecy state variable `p` (`p' = p`, unconstrained initial value) bound
/// to an array read index expression `bind_node` inside `Bad_P`.
#[cfg(informal_bitwuzla)]
#[derive(Debug, Clone)]
pub(crate) struct Prophecy {
    pub(crate) sort_id: u32,
    pub(crate) width: u32,
    /// Optional Btor2 node (signed) bound as `p == bind_node` in `Bad_P`.
    pub(crate) bind_node: Option<i64>,
}

/// Reference to a bit-vector term over state and prophecy variables.
#[cfg(informal_bitwuzla)]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum TermRef {
    State(u32),
    Prophecy(usize),
    ArrayRead { arr_id: u32, p_idx: usize },
}

/// Canonical word-level Boolean state atom (evaluates over `(s, p)` only).
#[cfg(informal_bitwuzla)]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Atom {
    /// A `bv1` Btor2 node evaluated on state variables (optionally substituting
    /// one input/index node `from_node` with `prophecy[p_idx]`).
    Syntax {
        node: u32,
        subst: Option<(u32, usize)>,
    },
    /// Bit-vector state equals MSB-first bit constant.
    StateEqConst { state_id: u32, bits: String },
    /// Prophecy-indexed array read equals MSB-first bit constant.
    ArrayReadEqConst {
        arr_id: u32,
        p_idx: usize,
        bits: String,
    },
    /// Equality between two state/prophecy/array-read terms of matching sort.
    TermEq(TermRef, TermRef),
}

/// Signed word-level literal over [`Atom`].
#[cfg(informal_bitwuzla)]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Lit {
    pub(crate) atom: Atom,
    pub(crate) neg: bool,
}

#[cfg(informal_bitwuzla)]
impl Lit {
    pub(crate) fn pos(atom: Atom) -> Self {
        Self { atom, neg: false }
    }
    pub(crate) fn not(self) -> Self {
        Self {
            atom: self.atom,
            neg: !self.neg,
        }
    }
    pub(crate) fn is_concrete(&self) -> bool {
        matches!(
            self.atom,
            Atom::StateEqConst { .. } | Atom::ArrayReadEqConst { .. }
        )
    }
}

/// A conjunction of state literals (blocking a cube adds the clause `\/ !lit`).
#[cfg(informal_bitwuzla)]
pub(crate) type Cube = Vec<Lit>;

/// Word-level transition system metadata + syntax-guided atom vocabulary.
#[cfg(informal_bitwuzla)]
pub(crate) struct Wts {
    pub(crate) bv_states: Vec<(u32, u32)>,
    pub(crate) arr_states: Vec<(u32, u32, u32)>, // (state_id, idx_sort, elem_sort)
    pub(crate) init: HashMap<u32, i64>,
    pub(crate) next: HashMap<u32, i64>,
    pub(crate) prophecies: Vec<Prophecy>,
    pub(crate) syntax_atoms: Vec<Atom>,
}

#[cfg(informal_bitwuzla)]
fn collect_deps(m: &Model) -> HashMap<u32, BTreeSet<u32>> {
    let mut deps: HashMap<u32, BTreeSet<u32>> = HashMap::new();
    for l in m.lines() {
        let mut s = BTreeSet::new();
        match l.op {
            Op::Input => {
                s.insert(l.id);
            }
            Op::State
            | Op::Zero
            | Op::One
            | Op::Ones
            | Op::Const
            | Op::Constd
            | Op::Consth
            | Op::Init
            | Op::Next
            | Op::Bad
            | Op::Constraint
            | Op::Output
            | Op::Fair
            | Op::Justice => {}
            _ => {
                for &a in &l.args {
                    if let Some(d) = deps.get(&(a.unsigned_abs() as u32)) {
                        s.extend(d.iter().copied());
                    }
                }
            }
        }
        deps.insert(l.id, s);
    }
    deps
}

#[cfg(informal_bitwuzla)]
fn cone_of(m: &Model, roots: &[u32]) -> HashSet<u32> {
    let mut seen = HashSet::new();
    let mut stack: Vec<u32> = roots.to_vec();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Some(l) = m.line(id) {
            for &a in &l.args {
                stack.push(a.unsigned_abs() as u32);
            }
        }
    }
    seen
}

#[cfg(informal_bitwuzla)]
fn is_predicate_op(op: Op) -> bool {
    matches!(
        op,
        Op::Eq
            | Op::Neq
            | Op::Ult
            | Op::Ulte
            | Op::Ugt
            | Op::Ugte
            | Op::Slt
            | Op::Slte
            | Op::Sgt
            | Op::Sgte
            | Op::Iff
            | Op::Implies
    )
}

#[cfg(informal_bitwuzla)]
fn is_const_op(op: Op) -> bool {
    matches!(
        op,
        Op::Zero | Op::One | Op::Ones | Op::Const | Op::Constd | Op::Consth
    )
}

#[cfg(informal_bitwuzla)]
impl Wts {
    /// Build a word-level transition system and mine syntax-guided predicates
    /// plus initial prophecy variables from array reads in `Bad`.
    pub(crate) fn from_model(m: &Model, extra_prophecies: usize) -> anyhow::Result<Self> {
        let mut bv_states = Vec::new();
        let mut arr_states = Vec::new();
        for l in m.with_op(Op::State) {
            let sid = l
                .sort
                .ok_or_else(|| anyhow::anyhow!("state without sort"))?;
            match m.sort(sid) {
                Some(Sort::BitVec(w)) => bv_states.push((l.id, w)),
                Some(Sort::Array { index, element }) => arr_states.push((l.id, index, element)),
                None => anyhow::bail!("unknown sort {sid}"),
            }
        }
        let init: HashMap<u32, i64> = m
            .with_op(Op::Init)
            .map(|l| (l.args[0] as u32, l.args[1]))
            .collect();
        let next: HashMap<u32, i64> = m
            .with_op(Op::Next)
            .map(|l| (l.args[0] as u32, l.args[1]))
            .collect();

        let input_deps = collect_deps(m);
        let bad_roots: Vec<u32> = m
            .with_op(Op::Bad)
            .map(|l| l.args[0].unsigned_abs() as u32)
            .collect();
        let bad_cone = cone_of(m, &bad_roots);

        // 1. Seed prophecy variables for every distinct array read index in `Bad`'s cone
        //    that depends on an input (capped at 4).
        let mut prophecies: Vec<Prophecy> = Vec::new();
        let mut bound_idx_nodes: HashSet<i64> = HashSet::new();
        for l in m.lines() {
            if l.op == Op::Read && bad_cone.contains(&l.id) && prophecies.len() < 4 {
                let idx_arg = l.args[1];
                let idx_id = idx_arg.unsigned_abs() as u32;
                let has_inp = input_deps.get(&idx_id).is_some_and(|d| !d.is_empty());
                if has_inp && bound_idx_nodes.insert(idx_arg) {
                    if let Some(sid) = m.line(idx_id).and_then(|il| il.sort) {
                        if let Some(w) = m.width(sid) {
                            prophecies.push(Prophecy {
                                sort_id: sid,
                                width: w,
                                bind_node: Some(idx_arg),
                            });
                        }
                    }
                }
            }
        }
        // Extra refinement prophecies (unconstrained `p' = p`) for array states if requested.
        if extra_prophecies > 0 && !arr_states.is_empty() {
            let mut added = 0;
            for &(_, idx_sort, _) in &arr_states {
                if added >= extra_prophecies || prophecies.len() >= 4 {
                    break;
                }
                if let Some(w) = m.width(idx_sort) {
                    prophecies.push(Prophecy {
                        sort_id: idx_sort,
                        width: w,
                        bind_node: None,
                    });
                    added += 1;
                }
            }
        }

        let mut atoms_set: BTreeSet<Atom> = BTreeSet::new();
        let mut add_atom = |a: Atom| {
            atoms_set.insert(a);
        };

        // 2. State-only Boolean predicates & single-bit states from model syntax.
        for &(sid, w) in &bv_states {
            if w == 1 {
                add_atom(Atom::Syntax {
                    node: sid,
                    subst: None,
                });
            }
        }
        for l in m.lines() {
            let is_bv1 = l.sort.and_then(|s| m.width(s)) == Some(1);
            let deps = input_deps.get(&l.id);
            let state_only = deps.is_none_or(|d| d.is_empty());

            // `ite` condition is an important control predicate.
            if l.op == Op::Ite {
                let cond_id = l.args[0].unsigned_abs() as u32;
                if input_deps.get(&cond_id).is_none_or(|d| d.is_empty()) {
                    add_atom(Atom::Syntax {
                        node: cond_id,
                        subst: None,
                    });
                }
            }

            if is_bv1 && is_predicate_op(l.op) {
                if state_only {
                    add_atom(Atom::Syntax {
                        node: l.id,
                        subst: None,
                    });
                } else if let Some(d) = deps {
                    // Prophecy-lift any predicate depending on exactly ONE input variable
                    // whose sort matches a prophecy variable (e.g. `waddr < 16` -> `p0 < 16`,
                    // `read(mem, raddr) != 0` -> `read(mem, p0) != 0`)!
                    if d.len() == 1 {
                        let only_inp = *d.iter().next().unwrap();
                        if let Some(inp_sort) = m.line(only_inp).and_then(|il| il.sort) {
                            for (p_idx, p) in prophecies.iter().enumerate() {
                                if p.sort_id == inp_sort {
                                    add_atom(Atom::Syntax {
                                        node: l.id,
                                        subst: Some((only_inp, p_idx)),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        // Also lift bad roots themselves if they depend on a single input of prophecy sort.
        for &br in &bad_roots {
            if let Some(d) = input_deps.get(&br) {
                if d.is_empty() {
                    add_atom(Atom::Syntax {
                        node: br,
                        subst: None,
                    });
                } else if d.len() == 1 {
                    let only_inp = *d.iter().next().unwrap();
                    if let Some(inp_sort) = m.line(only_inp).and_then(|il| il.sort) {
                        for (p_idx, p) in prophecies.iter().enumerate() {
                            if p.sort_id == inp_sort {
                                add_atom(Atom::Syntax {
                                    node: br,
                                    subst: Some((only_inp, p_idx)),
                                });
                            }
                        }
                    }
                }
            }
        }

        // 3. Array constants & equalities projected through prophecies.
        let mut consts_by_width: HashMap<u32, BTreeSet<String>> = HashMap::new();
        for l in m.lines() {
            if is_const_op(l.op) {
                if let Some(w) = l.sort.and_then(|s| m.width(s)) {
                    if let Ok(bits) = const_bits(l, w) {
                        consts_by_width.entry(w).or_default().insert(bits);
                    }
                }
            }
        }
        for &(arr_id, idx_sort, elem_sort) in &arr_states {
            let elem_w = m.width(elem_sort);
            for (p_idx, p) in prophecies.iter().enumerate() {
                if p.sort_id != idx_sort {
                    continue;
                }
                if let Some(ew) = elem_w {
                    if let Some(cset) = consts_by_width.get(&ew) {
                        for bits in cset.iter().take(8) {
                            add_atom(Atom::ArrayReadEqConst {
                                arr_id,
                                p_idx,
                                bits: bits.clone(),
                            });
                        }
                    }
                    // Relate `read(arr, p)` to BV states of matching element width.
                    for &(sv, sw) in &bv_states {
                        if sw == ew {
                            add_atom(Atom::TermEq(
                                TermRef::ArrayRead { arr_id, p_idx },
                                TermRef::State(sv),
                            ));
                        }
                    }
                }
            }
        }
        // Relate pairs of arrays of matching sort through prophecies.
        for i in 0..arr_states.len() {
            for j in (i + 1)..arr_states.len() {
                let (a1, is1, es1) = arr_states[i];
                let (a2, is2, es2) = arr_states[j];
                if is1 == is2 && es1 == es2 {
                    for (p_idx, p) in prophecies.iter().enumerate() {
                        if p.sort_id == is1 {
                            add_atom(Atom::TermEq(
                                TermRef::ArrayRead { arr_id: a1, p_idx },
                                TermRef::ArrayRead { arr_id: a2, p_idx },
                            ));
                        }
                    }
                }
            }
        }
        // Relate prophecies to BV states of matching index width (e.g., `p == wr_ptr`).
        for (p_idx, p) in prophecies.iter().enumerate() {
            for &(sv, sw) in &bv_states {
                if sw == p.width && sw > 1 {
                    add_atom(Atom::TermEq(TermRef::Prophecy(p_idx), TermRef::State(sv)));
                }
            }
        }
        // Relate BV states of matching width if there are few of them.
        if bv_states.len() <= 16 {
            for i in 0..bv_states.len() {
                for j in (i + 1)..bv_states.len() {
                    if bv_states[i].1 == bv_states[j].1 && bv_states[i].1 > 1 {
                        add_atom(Atom::TermEq(
                            TermRef::State(bv_states[i].0),
                            TermRef::State(bv_states[j].0),
                        ));
                    }
                }
            }
        }

        Ok(Wts {
            bv_states,
            arr_states,
            init,
            next,
            prophecies,
            syntax_atoms: atoms_set.into_iter().collect(),
        })
    }
}

/// One frame's encoded Bitwuzla terms (`nodes`, `prophecies`, `constraints`, `bad`, `bad_p`).
#[cfg(informal_bitwuzla)]
pub(crate) struct FrameEnv {
    pub(crate) sorts: HashMap<u32, i32>,
    pub(crate) nodes: HashMap<u32, T>,
    pub(crate) prophecies: Vec<T>,
    pub(crate) bad_p: T,
    atom_cache: HashMap<Atom, T>,
}

#[cfg(informal_bitwuzla)]
impl FrameEnv {
    /// Encode one frame of `m` in `bz`.
    /// - If `state_override` is `Some`, uses those pre-built terms for `Op::State`
    ///   (e.g. next-state expressions from frame 0). Otherwise allocates fresh state constants.
    /// - If `prophecy_override` is `Some`, reuses those terms for prophecies (`p' = p`).
    pub(crate) fn encode(
        m: &Model,
        wts: &Wts,
        bz: &Bz,
        sorts: &HashMap<u32, i32>,
        state_override: Option<&HashMap<u32, T>>,
        prophecy_override: Option<&[T]>,
        tag: &str,
    ) -> anyhow::Result<Self> {
        let mut nodes: HashMap<u32, T> = HashMap::new();
        for l in m.lines() {
            let t = encode_line(m, bz, sorts, &nodes, state_override, l, tag)?;
            if let Some(t) = t {
                nodes.insert(l.id, t);
            }
        }

        let prophecies: Vec<T> = match prophecy_override {
            Some(ps) => ps.to_vec(),
            None => wts
                .prophecies
                .iter()
                .enumerate()
                .map(|(idx, p)| bz.konst(sorts[&p.sort_id], &format!("_p{idx}_{tag}")))
                .collect::<anyhow::Result<Vec<_>>>()?,
        };

        let bads: Vec<T> = m
            .with_op(Op::Bad)
            .map(|l| eval_signed(bz, &nodes, l.args[0]))
            .collect::<anyhow::Result<_>>()?;
        let mut bad = bads[0];
        for &b in &bads[1..] {
            bad = bz.op("or", &[bad, b], 0, 0)?;
        }

        let mut bad_p = bad;
        for (p, &pt) in wts.prophecies.iter().zip(&prophecies) {
            if let Some(bind) = p.bind_node {
                let idx_t = eval_signed(bz, &nodes, bind)?;
                let eq = bz.op("eq", &[pt, idx_t], 0, 0)?;
                bad_p = bz.op("and", &[bad_p, eq], 0, 0)?;
            }
        }

        Ok(FrameEnv {
            sorts: sorts.clone(),
            nodes,
            prophecies,
            bad_p,
            atom_cache: HashMap::new(),
        })
    }

    /// Assert `Init(s)` on this frame.
    pub(crate) fn assert_init(&self, m: &Model, wts: &Wts, bz: &Bz) -> anyhow::Result<()> {
        for (&s, &v) in &wts.init {
            let st = self.nodes[&s];
            let mut val = eval_signed(bz, &self.nodes, v)?;
            let ss = m.line(s).and_then(|l| l.sort).unwrap();
            if matches!(m.sort(ss), Some(Sort::Array { .. }))
                && m.line(v.unsigned_abs() as u32)
                    .and_then(|l| l.sort)
                    .is_some_and(|vs| m.width(vs).is_some())
            {
                val = bz.const_array(self.sorts[&ss], val)?;
            }
            let eq = bz.op("eq", &[st, val], 0, 0)?;
            bz.assert(eq)?;
        }
        Ok(())
    }

    /// Assert all `Constraint` lines on this frame.
    pub(crate) fn assert_constraints(&self, m: &Model, bz: &Bz) -> anyhow::Result<()> {
        for l in m.with_op(Op::Constraint) {
            bz.assert(eval_signed(bz, &self.nodes, l.args[0])?)?;
        }
        Ok(())
    }

    /// Compute the next-state term map `{ state_id -> next_term }` from this frame.
    pub(crate) fn next_state_terms(
        &self,
        m: &Model,
        wts: &Wts,
        bz: &Bz,
        tag: &str,
    ) -> anyhow::Result<HashMap<u32, T>> {
        let mut out = HashMap::new();
        for l in m.with_op(Op::State) {
            let t = match wts.next.get(&l.id) {
                Some(&n) => eval_signed(bz, &self.nodes, n)?,
                None => bz.konst(
                    self.sorts[&l.sort.unwrap()],
                    &format!("_free_{}_{tag}", l.id),
                )?,
            };
            out.insert(l.id, t);
        }
        Ok(out)
    }

    /// Encode a [`TermRef`] in this frame.
    fn encode_term_ref(&self, bz: &Bz, tr: &TermRef) -> anyhow::Result<T> {
        match tr {
            TermRef::State(id) => Ok(self.nodes[id]),
            TermRef::Prophecy(p) => Ok(self.prophecies[*p]),
            TermRef::ArrayRead { arr_id, p_idx } => {
                bz.op("read", &[self.nodes[arr_id], self.prophecies[*p_idx]], 0, 0)
            }
        }
    }

    /// Encode an [`Atom`] as a `bv1` Bitwuzla term in this frame (cached).
    pub(crate) fn encode_atom(&mut self, m: &Model, bz: &Bz, atom: &Atom) -> anyhow::Result<T> {
        if let Some(&t) = self.atom_cache.get(atom) {
            return Ok(t);
        }
        let t = match atom {
            Atom::Syntax { node, subst: None } => self.nodes[node],
            Atom::Syntax {
                node,
                subst: Some((from_id, p_idx)),
            } => {
                let mut memo: HashMap<u32, T> = HashMap::new();
                memo.insert(*from_id, self.prophecies[*p_idx]);
                eval_with_subst(m, bz, &self.sorts, &self.nodes, *node, &mut memo)?
            }
            Atom::StateEqConst { state_id, bits } => {
                let s = self.nodes[state_id];
                let sid = m.line(*state_id).and_then(|l| l.sort).unwrap();
                let val = bz.value(self.sorts[&sid], bits)?;
                bz.op("eq", &[s, val], 0, 0)?
            }
            Atom::ArrayReadEqConst {
                arr_id,
                p_idx,
                bits,
            } => {
                let r = bz.op("read", &[self.nodes[arr_id], self.prophecies[*p_idx]], 0, 0)?;
                let sid = m.line(*arr_id).and_then(|l| l.sort).unwrap();
                let elem_sort = match m.sort(sid) {
                    Some(Sort::Array { element, .. }) => element,
                    _ => anyhow::bail!("expected array sort"),
                };
                let val = bz.value(self.sorts[&elem_sort], bits)?;
                bz.op("eq", &[r, val], 0, 0)?
            }
            Atom::TermEq(a, b) => {
                let ta = self.encode_term_ref(bz, a)?;
                let tb = self.encode_term_ref(bz, b)?;
                bz.op("eq", &[ta, tb], 0, 0)?
            }
        };
        self.atom_cache.insert(atom.clone(), t);
        Ok(t)
    }

    /// Encode a signed [`Lit`] as a `bv1` term.
    pub(crate) fn encode_lit(&mut self, m: &Model, bz: &Bz, lit: &Lit) -> anyhow::Result<T> {
        let a = self.encode_atom(m, bz, &lit.atom)?;
        if lit.neg {
            bz.op("not", &[a], 0, 0)
        } else {
            Ok(a)
        }
    }

    /// Encode the blocking clause `!cube = \/_{l in cube} !l` as a `bv1` term.
    pub(crate) fn encode_clause(&mut self, m: &Model, bz: &Bz, cube: &[Lit]) -> anyhow::Result<T> {
        assert!(!cube.is_empty(), "empty cube");
        let mut acc = self.encode_lit(m, bz, &cube[0].clone().not())?;
        for l in &cube[1..] {
            let nl = self.encode_lit(m, bz, &l.clone().not())?;
            acc = bz.op("or", &[acc, nl], 0, 0)?;
        }
        Ok(acc)
    }
}

#[cfg(informal_bitwuzla)]
fn eval_signed(bz: &Bz, nodes: &HashMap<u32, T>, a: i64) -> anyhow::Result<T> {
    let t = *nodes
        .get(&(a.unsigned_abs() as u32))
        .ok_or_else(|| anyhow::anyhow!("node {} used before definition", a.abs()))?;
    if a < 0 {
        bz.op("not", &[t], 0, 0)
    } else {
        Ok(t)
    }
}

#[cfg(informal_bitwuzla)]
fn encode_line(
    m: &Model,
    bz: &Bz,
    sorts: &HashMap<u32, i32>,
    nodes: &HashMap<u32, T>,
    state_override: Option<&HashMap<u32, T>>,
    l: &Line,
    tag: &str,
) -> anyhow::Result<Option<T>> {
    let sort = l.sort.map(|s| sorts[&s]);
    let width = l.sort.and_then(|s| m.width(s));
    let t = match l.op {
        Op::State => {
            if let Some(ov) = state_override {
                ov[&l.id]
            } else {
                let nm = l.symbol.as_deref().unwrap_or("");
                bz.konst(sort.unwrap(), &format!("{nm}_{tag}"))?
            }
        }
        Op::Input => {
            let nm = l.symbol.as_deref().unwrap_or("");
            bz.konst(sort.unwrap(), &format!("{nm}_{tag}"))?
        }
        Op::Zero | Op::One | Op::Ones | Op::Const | Op::Constd | Op::Consth => {
            bz.value(sort.unwrap(), &const_bits(l, width.unwrap())?)?
        }
        Op::Sext | Op::Uext => bz.op(
            l.op.name(),
            &[eval_signed(bz, nodes, l.args[0])?],
            l.nums[0].parse()?,
            0,
        )?,
        Op::Slice => bz.op(
            "slice",
            &[eval_signed(bz, nodes, l.args[0])?],
            l.nums[0].parse()?,
            l.nums[1].parse()?,
        )?,
        Op::Init | Op::Next | Op::Bad | Op::Constraint | Op::Output | Op::Fair | Op::Justice => {
            return Ok(None)
        }
        op => {
            let args = l
                .args
                .iter()
                .map(|&a| eval_signed(bz, nodes, a))
                .collect::<anyhow::Result<Vec<_>>>()?;
            bz.op(op.name(), &args, 0, 0)?
        }
    };
    Ok(Some(t))
}

#[cfg(informal_bitwuzla)]
fn eval_with_subst(
    m: &Model,
    bz: &Bz,
    sorts: &HashMap<u32, i32>,
    base_nodes: &HashMap<u32, T>,
    id: u32,
    memo: &mut HashMap<u32, T>,
) -> anyhow::Result<T> {
    if let Some(&t) = memo.get(&id) {
        return Ok(t);
    }
    let l = m
        .line(id)
        .ok_or_else(|| anyhow::anyhow!("missing node {id}"))?;
    let t = match l.op {
        Op::State
        | Op::Input
        | Op::Zero
        | Op::One
        | Op::Ones
        | Op::Const
        | Op::Constd
        | Op::Consth => base_nodes[&id],
        Op::Sext | Op::Uext => {
            let a0 = eval_signed_subst(m, bz, sorts, base_nodes, l.args[0], memo)?;
            bz.op(l.op.name(), &[a0], l.nums[0].parse()?, 0)?
        }
        Op::Slice => {
            let a0 = eval_signed_subst(m, bz, sorts, base_nodes, l.args[0], memo)?;
            bz.op("slice", &[a0], l.nums[0].parse()?, l.nums[1].parse()?)?
        }
        op => {
            let args = l
                .args
                .iter()
                .map(|&a| eval_signed_subst(m, bz, sorts, base_nodes, a, memo))
                .collect::<anyhow::Result<Vec<_>>>()?;
            bz.op(op.name(), &args, 0, 0)?
        }
    };
    memo.insert(id, t);
    Ok(t)
}

#[cfg(informal_bitwuzla)]
fn eval_signed_subst(
    m: &Model,
    bz: &Bz,
    sorts: &HashMap<u32, i32>,
    base_nodes: &HashMap<u32, T>,
    a: i64,
    memo: &mut HashMap<u32, T>,
) -> anyhow::Result<T> {
    let t = eval_with_subst(m, bz, sorts, base_nodes, a.unsigned_abs() as u32, memo)?;
    if a < 0 {
        bz.op("not", &[t], 0, 0)
    } else {
        Ok(t)
    }
}

#[cfg(informal_bitwuzla)]
pub(crate) fn fresh_sorts(m: &Model, bz: &Bz) -> anyhow::Result<HashMap<u32, i32>> {
    build_sorts(m, bz)
}
