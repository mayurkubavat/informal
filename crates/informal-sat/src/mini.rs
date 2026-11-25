//! A small CDCL solver for the many short queries IC3 makes.
//!
//! Designed around GipSAT-style incremental IC3 workloads: flat clause arena
//! (no per-clause heap allocations), inline binary-clause watch fast path,
//! grouped frame watch lists (`add_clause_grouped`), COI-restricted decisions
//! and domain propagation (`solve_domain`) without global-heap churn, first-UIP
//! learning with recursive clause minimization, temporary clauses (`retire`),
//! phase saving, Luby restarts, and level-0 compaction.
//!
//! Hot loops index slices directly; no `unsafe`.

use crate::{Answer, Lit, Sat};

type Var = usize;

#[inline]
fn var(l: Lit) -> Var {
    l.unsigned_abs() as usize
}

/// Internal literal code: 2*var + (negative).
#[inline]
fn code(l: Lit) -> usize {
    2 * var(l) + usize::from(l < 0)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Val {
    True,
    False,
    Undef,
}

/// Clause metadata in [`Mini::clauses`]; its literals sit contiguously in
/// [`Mini::lits`]`[start .. start + len]`.
#[derive(Clone, Copy)]
struct ClauseHdr {
    start: u32,
    len: u32,
    /// Watch-list group ([`Sat::add_clause_grouped`]); `BASE` for none.
    group: u32,
    activity: f32,
    learnt: bool,
    deleted: bool,
}

/// High bit of [`Watch::clause`] marks a binary clause whose other literal is
/// stored directly in [`Watch::blocker`] (zero arena dereference on propagation).
const BIN_BIT: u32 = 1 << 31;

#[derive(Clone, Copy)]
struct Watch {
    clause: u32,
    blocker: Lit,
}

/// Learned clauses kept before the first reduction of a solve.
const MAX_LEARNTS: f64 = 2000.0;

/// The group of clauses that are always scanned.
const BASE: u32 = u32::MAX;

pub struct Mini {
    /// Watch lists of grouped clauses: by literal code, the (group, list)
    /// pairs that exist for it, sorted by group. Above level 0 in a
    /// restricted solve only groups from `active_from` up are scanned.
    gw: Vec<Vec<(u32, Vec<Watch>)>>,
    active_from: usize,
    clauses: Vec<ClauseHdr>,
    /// Flat literal arena shared by every clause.
    lits: Vec<Lit>,
    watches: Vec<Vec<Watch>>, // by literal code: clauses watching ¬lit
    assign: Vec<Val>,         // by variable
    level: Vec<u32>,
    reason: Vec<Option<u32>>,
    phase: Vec<bool>,
    activity: Vec<f64>,
    var_inc: f64,
    cla_inc: f32,
    heap: Heap,
    /// Set when `activity` changed during a restricted solve (where `heap` is
    /// bypassed); triggers an O(N) rebuild before the next unrestricted solve.
    heap_dirty: bool,
    trail: Vec<Lit>,
    trail_lim: Vec<usize>,
    qhead: usize,
    /// Scratch marks during conflict analysis / minimization:
    /// `0` = unseen, `1` = in conflict/learnt, `2` = redundant, `3` = non-redundant.
    seen: Vec<u8>,
    ok: bool,
    /// Last SAT model via epoch stamping (avoids copying level-0 trail).
    model_val: Vec<bool>,
    model_stamp: Vec<u32>,
    model_id: u32,
    /// Outstanding temporary clause: `(clause_mark, act_lit, false_at_lvl0)`.
    temp: Option<(usize, Lit, bool)>,
    /// Clauses marked deleted since the last compaction.
    garbage: usize,
    /// Initial phase hash seed (`0` => all false).
    seed: u64,
    decisions: u64,
    conflict: Vec<Lit>, // failed assumptions (negated), after Unsat
    terminate: Option<Box<dyn Fn() -> bool + Send>>,
    units_since_simplify: usize,
    /// Decision domain for the current solve (empty: every variable).
    in_domain: Vec<bool>,
    restricted: bool,
    /// Domain variables in caller order for restricted solves.
    dom: Vec<Var>,
    dom_head: usize,
    learnts: usize,
    max_learnts: f64,
    pub conflicts: u64,
    /// Reusable scratch buffers (zero heap churn per clause/conflict).
    add_buf: Vec<Lit>,
    ana_learnt: Vec<Lit>,
    ana_clear: Vec<Var>,
    ana_stack: Vec<Var>,
}

/// Process-wide counters for profiling (INFORMAL_IC3_STATS).
pub static DECISIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static DOMAIN_VARS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static SOLVES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static PROPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static SAT_SOLVES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static SAT_DECISIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static SAT_NANOS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static MAX_VARS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static ALL_NANOS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// SplitMix64 of `seed + i`.
fn mix(seed: u64, i: u64) -> u64 {
    let mut z = seed.wrapping_add(i.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

impl Mini {
    /// The solver with a seed for the initial phases (see `seed`).
    pub fn with_seed(seed: u64) -> Self {
        Mini {
            seed,
            ..Default::default()
        }
    }
}

impl Default for Mini {
    fn default() -> Self {
        Mini {
            clauses: Vec::new(),
            lits: Vec::new(),
            gw: vec![Vec::new(); 2],
            active_from: 0,
            watches: vec![Vec::new(); 2],
            assign: vec![Val::Undef],
            level: vec![0],
            reason: vec![None],
            phase: vec![false],
            activity: vec![0.0],
            var_inc: 1.0,
            cla_inc: 1.0,
            heap: Heap::default(),
            heap_dirty: false,
            trail: Vec::new(),
            trail_lim: Vec::new(),
            qhead: 0,
            seen: vec![0],
            ok: true,
            model_val: Vec::new(),
            model_stamp: Vec::new(),
            model_id: 0,
            decisions: 0,
            temp: None,
            garbage: 0,
            seed: 0,
            conflict: Vec::new(),
            terminate: None,
            units_since_simplify: 0,
            in_domain: Vec::new(),
            restricted: false,
            dom: Vec::new(),
            dom_head: 0,
            learnts: 0,
            max_learnts: MAX_LEARNTS,
            conflicts: 0,
            add_buf: Vec::new(),
            ana_learnt: Vec::new(),
            ana_clear: Vec::new(),
            ana_stack: Vec::new(),
        }
    }
}

impl Mini {
    fn ensure(&mut self, v: Var) {
        while self.assign.len() <= v {
            let n = self.assign.len();
            self.assign.push(Val::Undef);
            self.level.push(0);
            self.reason.push(None);
            self.phase
                .push(self.seed != 0 && mix(self.seed, n as u64) & 1 == 1);
            self.activity.push(0.0);
            self.seen.push(0);
            self.watches.push(Vec::new());
            self.watches.push(Vec::new());
            self.gw.push(Vec::new());
            self.gw.push(Vec::new());
            if !self.heap_dirty {
                self.heap.insert(n, &self.activity);
            }
        }
    }

    #[inline]
    fn lit_value(&self, l: Lit) -> Val {
        match self.assign[var(l)] {
            Val::Undef => Val::Undef,
            Val::True => {
                if l > 0 {
                    Val::True
                } else {
                    Val::False
                }
            }
            Val::False => {
                if l > 0 {
                    Val::False
                } else {
                    Val::True
                }
            }
        }
    }

    #[inline]
    fn decision_level(&self) -> u32 {
        self.trail_lim.len() as u32
    }

    #[inline]
    fn enqueue(&mut self, l: Lit, reason: Option<u32>) {
        let v = var(l);
        self.assign[v] = if l > 0 { Val::True } else { Val::False };
        self.level[v] = self.decision_level();
        self.reason[v] = reason;
        self.trail.push(l);
    }

    #[inline]
    fn push_clause_raw(&mut self, lits: &[Lit], group: u32, learnt: bool) -> u32 {
        let ci = self.clauses.len() as u32;
        debug_assert!(ci < BIN_BIT, "clause index overflow");
        let start = self.lits.len() as u32;
        self.lits.extend_from_slice(lits);
        self.clauses.push(ClauseHdr {
            start,
            len: lits.len() as u32,
            group,
            activity: 0.0,
            learnt,
            deleted: false,
        });
        self.attach(ci);
        ci
    }

    fn attach(&mut self, ci: u32) {
        let h = self.clauses[ci as usize];
        let s = h.start as usize;
        let (a, b) = (self.lits[s], self.lits[s + 1]);
        let tag = if h.len == 2 { ci | BIN_BIT } else { ci };
        // Watch lists are indexed by the literal whose falsification wakes
        // the clause.
        self.wl(h.group, code(-a)).push(Watch {
            clause: tag,
            blocker: b,
        });
        self.wl(h.group, code(-b)).push(Watch {
            clause: tag,
            blocker: a,
        });
    }

    /// The watch list of `group` for a literal code.
    #[inline]
    fn wl(&mut self, group: u32, code: usize) -> &mut Vec<Watch> {
        if group == BASE {
            return &mut self.watches[code];
        }
        let lists = &mut self.gw[code];
        let i = match lists.binary_search_by_key(&group, |e| e.0) {
            Ok(i) => i,
            Err(i) => {
                lists.insert(i, (group, Vec::new()));
                i
            }
        };
        &mut lists[i].1
    }

    #[inline]
    fn outside(&self, dom: bool, l: Lit) -> bool {
        dom && !self.in_domain.get(var(l)).copied().unwrap_or(false)
    }

    /// Propagate one watch list (`g_idx == None` for `watches[c_p]`, else
    /// `gw[c_p][gi].1`) when `p` (`false_lit == -p`) is propagated.
    fn propagate_list(
        &mut self,
        c_p: usize,
        g_idx: Option<usize>,
        false_lit: Lit,
        dom: bool,
    ) -> Option<u32> {
        let mut ws = match g_idx {
            None => std::mem::take(&mut self.watches[c_p]),
            Some(gi) => std::mem::take(&mut self.gw[c_p][gi].1),
        };
        let mut i = 0;
        let mut j = 0;
        let mut confl = None;
        while i < ws.len() {
            let w = ws[i];
            i += 1;
            match self.lit_value(w.blocker) {
                Val::True => {
                    ws[j] = w;
                    j += 1;
                    continue;
                }
                Val::Undef if self.outside(dom, w.blocker) => {
                    ws[j] = w;
                    j += 1;
                    continue;
                }
                _ => {}
            }
            if (w.clause & BIN_BIT) != 0 {
                let ci = (w.clause & !BIN_BIT) as usize;
                if self.clauses[ci].deleted {
                    continue;
                }
                ws[j] = w;
                j += 1;
                if self.lit_value(w.blocker) == Val::False {
                    confl = Some(ci as u32);
                    while i < ws.len() {
                        ws[j] = ws[i];
                        i += 1;
                        j += 1;
                    }
                } else {
                    self.enqueue(w.blocker, Some(ci as u32));
                }
                continue;
            }
            let ci = w.clause as usize;
            let hdr = self.clauses[ci];
            if hdr.deleted {
                continue;
            }
            let start = hdr.start as usize;
            let end = start + hdr.len as usize;
            if self.lits[start] == false_lit {
                self.lits.swap(start, start + 1);
            }
            let first = self.lits[start];
            if first != w.blocker {
                match self.lit_value(first) {
                    Val::True => {
                        ws[j] = Watch {
                            clause: w.clause,
                            blocker: first,
                        };
                        j += 1;
                        continue;
                    }
                    Val::Undef if self.outside(dom, first) => {
                        ws[j] = Watch {
                            clause: w.clause,
                            blocker: first,
                        };
                        j += 1;
                        continue;
                    }
                    _ => {}
                }
            }
            // Look for a new watch.
            let mut found = false;
            for k in (start + 2)..end {
                let l = self.lits[k];
                if self.lit_value(l) != Val::False {
                    self.lits.swap(start + 1, k);
                    self.wl(hdr.group, code(-l)).push(Watch {
                        clause: w.clause,
                        blocker: first,
                    });
                    found = true;
                    break;
                }
            }
            if found {
                continue;
            }
            ws[j] = Watch {
                clause: w.clause,
                blocker: first,
            };
            j += 1;
            match self.lit_value(first) {
                Val::False => {
                    confl = Some(w.clause);
                    while i < ws.len() {
                        ws[j] = ws[i];
                        i += 1;
                        j += 1;
                    }
                }
                _ => self.enqueue(first, Some(w.clause)),
            }
        }
        ws.truncate(j);
        let dst = match g_idx {
            None => &mut self.watches[c_p],
            Some(gi) => &mut self.gw[c_p][gi].1,
        };
        if !dst.is_empty() {
            ws.extend(std::mem::take(dst));
        }
        *dst = ws;
        confl
    }

    /// Unit propagation; returns the conflicting clause index on failure.
    fn propagate(&mut self) -> Option<u32> {
        let dom = self.restricted && self.decision_level() > 0;
        let first_group = if dom { self.active_from as u32 } else { 0 };
        while self.qhead < self.trail.len() {
            let p = self.trail[self.qhead];
            self.qhead += 1;
            let c_p = code(p);
            let false_lit = -p;
            if let Some(confl) = self.propagate_list(c_p, None, false_lit, dom) {
                self.qhead = self.trail.len();
                return Some(confl);
            }
            let start_gi = self.gw[c_p].partition_point(|e| e.0 < first_group);
            let ng = self.gw[c_p].len();
            for gi in start_gi..ng {
                if let Some(confl) = self.propagate_list(c_p, Some(gi), false_lit, dom) {
                    self.qhead = self.trail.len();
                    return Some(confl);
                }
            }
        }
        None
    }

    fn bump_var(&mut self, v: Var) {
        self.activity[v] += self.var_inc;
        if self.activity[v] > 1e100 {
            for a in self.activity.iter_mut() {
                *a *= 1e-100;
            }
            self.var_inc *= 1e-100;
        }
        if self.restricted {
            self.heap_dirty = true;
        } else {
            self.heap.update(v, &self.activity);
        }
    }

    fn bump_clause(&mut self, ci: usize) {
        let c = &mut self.clauses[ci];
        c.activity += self.cla_inc;
        if c.activity > 1e20 {
            for c in self.clauses.iter_mut().filter(|c| c.learnt) {
                c.activity *= 1e-20;
            }
            self.cla_inc *= 1e-20;
        }
    }

    /// First-UIP analysis into `self.ana_learnt` (asserting literal at `[0]`),
    /// returning the backjump level.
    fn analyze(&mut self, mut confl: u32) -> u32 {
        self.ana_learnt.clear();
        self.ana_learnt.push(0);
        self.ana_clear.clear();
        let mut path = 0usize;
        let mut p: Lit = 0;
        let mut idx = self.trail.len();
        let cur_lvl = self.decision_level();
        loop {
            let hdr = self.clauses[confl as usize];
            if hdr.learnt {
                self.bump_clause(confl as usize);
            }
            let start = hdr.start as usize;
            let end = start + hdr.len as usize;
            for k in start..end {
                let q = self.lits[k];
                if q == p {
                    continue;
                }
                let v = var(q);
                if self.seen[v] == 0 && self.level[v] > 0 {
                    self.seen[v] = 1;
                    self.ana_clear.push(v);
                    self.bump_var(v);
                    if self.level[v] >= cur_lvl {
                        path += 1;
                    } else {
                        self.ana_learnt.push(q);
                    }
                }
            }
            loop {
                idx -= 1;
                if self.seen[var(self.trail[idx])] != 0 {
                    break;
                }
            }
            p = self.trail[idx];
            self.seen[var(p)] = 0;
            path -= 1;
            if path == 0 {
                break;
            }
            confl = self.reason[var(p)].expect("propagated literal has a reason");
        }
        self.ana_learnt[0] = -p;

        // Recursive clause minimization (MiniSat `litRedundant`) with level mask.
        let mut abstract_levels = 0u64;
        for i in 1..self.ana_learnt.len() {
            abstract_levels |= 1u64 << (self.level[var(self.ana_learnt[i])] & 63);
        }
        let mut write = 1usize;
        for read in 1..self.ana_learnt.len() {
            let l = self.ana_learnt[read];
            if self.reason[var(l)].is_none() || !self.lit_redundant(l, abstract_levels) {
                self.ana_learnt[write] = l;
                write += 1;
            }
        }
        self.ana_learnt.truncate(write);

        for &v in &self.ana_clear {
            self.seen[v] = 0;
        }
        self.ana_clear.clear();

        // Backjump level: the highest level among the rest; put it second.
        let mut bt = 0;
        if self.ana_learnt.len() > 1 {
            let mut max_i = 1;
            let mut max_lvl = self.level[var(self.ana_learnt[1])];
            for i in 2..self.ana_learnt.len() {
                let lv = self.level[var(self.ana_learnt[i])];
                if lv > max_lvl {
                    max_lvl = lv;
                    max_i = i;
                }
            }
            self.ana_learnt.swap(1, max_i);
            bt = max_lvl;
        }
        bt
    }

    /// Recursive minimization: `l` is redundant if every path through reason
    /// clauses terminates in literals already in `ana_learnt` (`seen == 1`),
    /// previously confirmed redundant (`seen == 2`), or fixed at level 0.
    fn lit_redundant(&mut self, l: Lit, abstract_levels: u64) -> bool {
        let top = self.ana_clear.len();
        self.ana_stack.clear();
        self.ana_stack.push(var(l));
        while let Some(v) = self.ana_stack.pop() {
            let Some(r) = self.reason[v] else {
                for &u in &self.ana_clear[top..] {
                    self.seen[u] = 3;
                }
                return false;
            };
            let hdr = self.clauses[r as usize];
            let start = hdr.start as usize;
            let end = start + hdr.len as usize;
            for k in start..end {
                let q = self.lits[k];
                let qv = var(q);
                if qv == v {
                    continue;
                }
                let lv = self.level[qv];
                if lv == 0 || self.seen[qv] == 1 || self.seen[qv] == 2 {
                    continue;
                }
                if self.seen[qv] == 3
                    || self.reason[qv].is_none()
                    || (abstract_levels & (1u64 << (lv & 63))) == 0
                {
                    for &u in &self.ana_clear[top..] {
                        self.seen[u] = 3;
                    }
                    return false;
                }
                self.seen[qv] = 2;
                self.ana_clear.push(qv);
                self.ana_stack.push(qv);
            }
        }
        true
    }

    /// The assumptions responsible for a conflict on `p` (MiniSat's
    /// analyzeFinal): stored negated in `self.conflict`.
    fn analyze_final(&mut self, p: Lit) {
        self.conflict.clear();
        self.conflict.push(p);
        if self.decision_level() == 0 {
            return;
        }
        self.seen[var(p)] = 1;
        for i in (self.trail_lim[0]..self.trail.len()).rev() {
            let x = var(self.trail[i]);
            if self.seen[x] == 0 {
                continue;
            }
            match self.reason[x] {
                None => {
                    if self.level[x] > 0 {
                        self.conflict.push(-self.trail[i]);
                    }
                }
                Some(r) => {
                    let hdr = self.clauses[r as usize];
                    let start = hdr.start as usize;
                    let end = start + hdr.len as usize;
                    for k in start..end {
                        let q = self.lits[k];
                        let qv = var(q);
                        if qv != x && self.level[qv] > 0 {
                            self.seen[qv] = 1;
                        }
                    }
                }
            }
            self.seen[x] = 0;
        }
        self.seen[var(p)] = 0;
    }

    fn cancel_until(&mut self, lvl: u32) {
        if self.decision_level() <= lvl {
            return;
        }
        self.dom_head = 0;
        let lim = self.trail_lim[lvl as usize];
        let reinsert = !self.restricted && !self.heap_dirty;
        for i in (lim..self.trail.len()).rev() {
            let v = var(self.trail[i]);
            self.phase[v] = self.trail[i] > 0;
            self.assign[v] = Val::Undef;
            self.reason[v] = None;
            if reinsert {
                self.heap.insert(v, &self.activity);
            }
        }
        self.trail.truncate(lim);
        self.trail_lim.truncate(lvl as usize);
        self.qhead = lim;
    }

    fn pick(&mut self) -> Option<Lit> {
        if self.restricted {
            // IC3 queries make few decisions; a scan of the (small) domain
            // beats draining a heap of every variable in the solver.
            while self.dom_head < self.dom.len()
                && self.assign[self.dom[self.dom_head]] != Val::Undef
            {
                self.dom_head += 1;
            }
            let v = *self.dom.get(self.dom_head)? as Lit;
            return Some(if self.phase[v as usize] { v } else { -v });
        }
        while let Some(v) = self.heap.pop(&self.activity) {
            if self.assign[v] == Val::Undef {
                let v = v as Lit;
                return Some(if self.phase[v as usize] { v } else { -v });
            }
        }
        None
    }

    fn reduce_learnts(&mut self) {
        let mut idx: Vec<usize> = (0..self.clauses.len())
            .filter(|&i| {
                let c = &self.clauses[i];
                c.learnt && !c.deleted && c.len > 2 && !self.locked(i)
            })
            .collect();
        idx.sort_by(|&a, &b| {
            self.clauses[a]
                .activity
                .total_cmp(&self.clauses[b].activity)
        });
        for &i in idx.iter().take(idx.len() / 2) {
            self.clauses[i].deleted = true;
            self.learnts -= 1;
            self.garbage += 1;
        }
    }

    fn locked(&self, ci: usize) -> bool {
        let l = self.lits[self.clauses[ci].start as usize];
        self.lit_value(l) == Val::True && self.reason[var(l)] == Some(ci as u32)
    }

    /// At level 0: drop satisfied clauses, compact the literal arena, and
    /// rebuild watches.
    fn simplify(&mut self) {
        debug_assert_eq!(self.decision_level(), 0);
        for ci in 0..self.clauses.len() {
            let h = self.clauses[ci];
            if h.deleted {
                continue;
            }
            let s = h.start as usize;
            let e = s + h.len as usize;
            if self.lits[s..e]
                .iter()
                .any(|&l| self.lit_value(l) == Val::True)
            {
                self.clauses[ci].deleted = true;
                if h.learnt {
                    self.learnts -= 1;
                }
            }
        }
        for r in self.reason.iter_mut() {
            *r = None; // level-0 reasons are never needed
        }
        let mut new_clauses = Vec::with_capacity(self.clauses.len());
        let mut new_lits = Vec::with_capacity(self.lits.len());
        for h in &self.clauses {
            if h.deleted {
                continue;
            }
            let s = h.start as usize;
            let e = s + h.len as usize;
            let new_start = new_lits.len() as u32;
            new_lits.extend_from_slice(&self.lits[s..e]);
            new_clauses.push(ClauseHdr {
                start: new_start,
                ..*h
            });
        }
        self.clauses = new_clauses;
        self.lits = new_lits;
        for w in self.watches.iter_mut() {
            w.clear();
        }
        for lists in self.gw.iter_mut() {
            lists.clear();
        }
        for ci in 0..self.clauses.len() {
            self.attach(ci as u32);
        }
        self.units_since_simplify = 0;
        self.garbage = 0;
    }

    fn luby(i: u64) -> u64 {
        let (mut size, mut seq) = (1u64, 0u32);
        while size < i + 1 {
            seq += 1;
            size = 2 * size + 1;
        }
        let mut x = i;
        while size - 1 != x {
            size = (size - 1) >> 1;
            seq -= 1;
            x %= size;
        }
        1u64 << seq
    }

    fn search(&mut self, assumptions: &[Lit], budget: u64) -> Option<Answer> {
        let mut conflicts = 0u64;
        let mut decs_here = 0u64;
        loop {
            if let Some(confl) = self.propagate() {
                self.conflicts += 1;
                conflicts += 1;
                if self.decision_level() == 0 {
                    self.ok = false;
                    return Some(Answer::Unsat);
                }
                let bt = self.analyze(confl);
                self.cancel_until(bt);
                if self.ana_learnt.len() == 1 {
                    let unit = self.ana_learnt[0];
                    self.enqueue(unit, None);
                } else {
                    let asserting = self.ana_learnt[0];
                    let ci = self.clauses.len() as u32;
                    let start = self.lits.len() as u32;
                    let len = self.ana_learnt.len() as u32;
                    self.lits.extend_from_slice(&self.ana_learnt);
                    self.clauses.push(ClauseHdr {
                        start,
                        len,
                        group: BASE,
                        activity: 0.0,
                        learnt: true,
                        deleted: false,
                    });
                    self.attach(ci);
                    self.bump_clause(ci as usize);
                    self.learnts += 1;
                    self.enqueue(asserting, Some(ci));
                }
                self.var_inc /= 0.95;
                self.cla_inc /= 0.999;
                if (conflicts & 63) == 0 && self.terminate.as_ref().is_some_and(|f| f()) {
                    return None;
                }
                continue;
            }
            if conflicts >= budget {
                self.cancel_until(0);
                return Some(Answer::Unknown); // restart
            }
            if self.learnts as f64 >= self.max_learnts + self.trail.len() as f64 {
                self.reduce_learnts();
                self.max_learnts *= 1.1;
            }
            // Assumptions first, one decision level each.
            let mut next = None;
            while (self.decision_level() as usize) < assumptions.len() {
                let a = assumptions[self.decision_level() as usize];
                match self.lit_value(a) {
                    Val::True => self.trail_lim.push(self.trail.len()),
                    Val::False => {
                        self.analyze_final(-a);
                        return Some(Answer::Unsat);
                    }
                    Val::Undef => {
                        next = Some(a);
                        break;
                    }
                }
            }
            let decision = match next {
                Some(a) => a,
                None => match self.pick() {
                    Some(l) => l,
                    None => return Some(Answer::Sat),
                },
            };
            self.trail_lim.push(self.trail.len());
            self.decisions += 1;
            decs_here += 1;
            if (decs_here & 4095) == 0 && self.terminate.as_ref().is_some_and(|f| f()) {
                return None;
            }
            self.enqueue(decision, None);
        }
    }
}

impl Sat for Mini {
    fn add_clause(&mut self, lits: &[Lit]) {
        self.add_clause_grouped(lits, BASE);
    }

    fn set_active_groups_from(&mut self, group: u32) {
        self.active_from = group as usize;
    }

    fn add_clause_grouped(&mut self, lits: &[Lit], group: u32) {
        if !self.ok {
            return;
        }
        self.cancel_until(0);
        for &l in lits {
            self.ensure(var(l));
        }
        self.add_buf.clear();
        for &l in lits {
            match self.lit_value(l) {
                Val::True => return,
                Val::False => continue,
                Val::Undef => {
                    if self.add_buf.contains(&-l) {
                        return;
                    }
                    if !self.add_buf.contains(&l) {
                        self.add_buf.push(l);
                    }
                }
            }
        }
        match self.add_buf.len() {
            0 => self.ok = false,
            1 => {
                let unit = self.add_buf[0];
                self.enqueue(unit, None);
                if self.propagate().is_some() {
                    self.ok = false;
                }
                self.units_since_simplify += 1;
            }
            _ => {
                let ci = self.clauses.len() as u32;
                let start = self.lits.len() as u32;
                let len = self.add_buf.len() as u32;
                self.lits.extend_from_slice(&self.add_buf);
                self.clauses.push(ClauseHdr {
                    start,
                    len,
                    group,
                    activity: 0.0,
                    learnt: false,
                    deleted: false,
                });
                self.attach(ci);
            }
        }
    }

    fn solve(&mut self, assumptions: &[Lit]) -> Answer {
        self.conflict.clear();
        if !self.ok {
            return Answer::Unsat;
        }
        for &a in assumptions {
            self.ensure(var(a));
        }
        self.cancel_until(0);
        if !self.restricted && self.heap_dirty {
            self.heap
                .rebuild(self.assign.len(), &self.assign, &self.activity);
            self.heap_dirty = false;
        }
        if let Some((_, act, true)) = self.temp {
            if assumptions.contains(&act) {
                self.conflict.push(-act);
                return Answer::Unsat;
            }
        }
        if self.units_since_simplify > 64 && self.temp.is_none() {
            self.simplify();
        }
        self.max_learnts = MAX_LEARNTS;
        let t0 = std::time::Instant::now();
        let d0 = self.decisions;
        let mut restart = 0;
        let ans = loop {
            match self.search(assumptions, 100 * Mini::luby(restart)) {
                None => break Answer::Unknown,
                Some(Answer::Unknown) => restart += 1,
                Some(a) => break a,
            }
        };
        SOLVES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        DECISIONS.fetch_add(self.decisions - d0, std::sync::atomic::Ordering::Relaxed);
        {
            use std::sync::atomic::Ordering::Relaxed;
            let ns = t0.elapsed().as_nanos() as u64;
            ALL_NANOS.fetch_add(ns, Relaxed);
            MAX_VARS.fetch_max(self.assign.len() as u64, Relaxed);
            if ans == Answer::Sat {
                SAT_SOLVES.fetch_add(1, Relaxed);
                SAT_NANOS.fetch_add(ns, Relaxed);
                SAT_DECISIONS.fetch_add(self.decisions - d0, Relaxed);
            }
        }
        PROPS.fetch_add(
            self.trail.len() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        if ans == Answer::Sat {
            self.model_id = self.model_id.wrapping_add(1);
            if self.model_id == 0 {
                self.model_stamp.iter_mut().for_each(|x| *x = 0);
                self.model_id = 1;
            }
            let n = self.assign.len();
            if self.model_val.len() < n {
                self.model_val.resize(n, false);
                self.model_stamp.resize(n, 0);
            }
            let from = self.trail_lim.first().copied().unwrap_or(self.trail.len());
            for &l in &self.trail[from..] {
                let v = var(l);
                self.model_stamp[v] = self.model_id;
                self.model_val[v] = l > 0;
            }
        }
        self.cancel_until(0);
        ans
    }

    fn value(&self, lit: Lit) -> bool {
        let v = var(lit);
        let val = if self.model_stamp.get(v) == Some(&self.model_id) {
            self.model_val[v]
        } else {
            self.assign.get(v) == Some(&Val::True)
        };
        val == (lit > 0)
    }

    fn add_temp_clause(&mut self, lits: &[Lit], act: Lit) {
        if !self.ok {
            return;
        }
        self.cancel_until(0);
        self.ensure(var(act));
        for &l in lits {
            self.ensure(var(l));
        }
        debug_assert!(self.temp.is_none(), "one temporary clause at a time");
        debug_assert!(self.assign[var(act)] == Val::Undef);
        let mark = self.clauses.len();
        self.temp = Some((mark, act, false));
        self.add_buf.clear();
        self.add_buf.push(-act);
        for &l in lits {
            match self.lit_value(l) {
                Val::True => return, // holds anyway
                Val::False => continue,
                Val::Undef => {
                    if self.add_buf[1..].contains(&-l) {
                        return;
                    }
                    if !self.add_buf[1..].contains(&l) {
                        self.add_buf.push(l);
                    }
                }
            }
        }
        if self.add_buf.len() == 1 {
            // False at level 0: assuming `act` is a conflict. Not stored,
            // so that `act` is not fixed and can be reused.
            self.temp = Some((mark, act, true));
            return;
        }
        let buf = std::mem::take(&mut self.add_buf);
        self.push_clause_raw(&buf, BASE, false);
        self.add_buf = buf;
    }

    fn retire(&mut self, act: Lit) -> bool {
        self.cancel_until(0);
        let Some((mark, a, _)) = self.temp.take() else {
            return true;
        };
        debug_assert_eq!(a, act);
        for ci in mark..self.clauses.len() {
            let h = self.clauses[ci];
            if h.deleted {
                continue;
            }
            let s = h.start as usize;
            let e = s + h.len as usize;
            if self.lits[s..e].contains(&-act) {
                self.clauses[ci].deleted = true;
                if h.learnt {
                    self.learnts -= 1;
                }
                self.garbage += 1;
            }
        }
        let v = var(act);
        if self.assign[v] != Val::Undef {
            if let Some(pos) = self.trail.iter().rposition(|&l| var(l) == v) {
                self.trail.remove(pos);
                if self.qhead > pos {
                    self.qhead -= 1;
                }
            }
            self.assign[v] = Val::Undef;
            self.reason[v] = None;
            if !self.heap_dirty {
                self.heap.insert(v, &self.activity);
            }
        }
        if self.garbage > 4096 && 2 * self.garbage > self.clauses.len() {
            self.simplify();
        }
        true
    }

    fn solve_domain(&mut self, assumptions: &[Lit], domain: &[Lit]) -> Answer {
        for &d in domain.iter().chain(assumptions) {
            self.ensure(var(d));
        }
        if self.in_domain.len() < self.assign.len() {
            self.in_domain.resize(self.assign.len(), false);
        }
        let mut dom = std::mem::take(&mut self.dom);
        dom.clear();
        for &d in domain.iter().chain(assumptions) {
            let v = var(d);
            if !self.in_domain[v] {
                self.in_domain[v] = true;
                dom.push(v);
            }
        }
        self.dom_head = 0;
        DOMAIN_VARS.fetch_add(dom.len() as u64, std::sync::atomic::Ordering::Relaxed);
        self.dom = dom;
        self.restricted = true;
        let r = self.solve(assumptions);
        self.restricted = false;
        for &v in &self.dom {
            self.in_domain[v] = false;
        }
        r
    }

    fn failed(&self, lit: Lit) -> bool {
        self.conflict.contains(&-lit)
    }

    fn set_terminate(&mut self, f: Box<dyn Fn() -> bool + Send>) {
        self.terminate = Some(f);
    }

    fn name(&self) -> &'static str {
        "mini"
    }

    fn reserve(&mut self, n: i32) {
        let target = self.assign.len() + n.max(0) as usize;
        self.ensure(target.saturating_sub(1));
    }
}

/// Max-heap of variables by activity.
#[derive(Default)]
struct Heap {
    heap: Vec<Var>,
    pos: Vec<Option<usize>>,
}

impl Heap {
    fn rebuild(&mut self, nvars: usize, assign: &[Val], act: &[f64]) {
        self.heap.clear();
        self.pos.clear();
        self.pos.resize(nvars, None);
        for v in 1..nvars {
            if assign[v] == Val::Undef {
                let i = self.heap.len();
                self.heap.push(v);
                self.pos[v] = Some(i);
            }
        }
        let n = self.heap.len();
        for i in (0..n / 2).rev() {
            self.down(i, act);
        }
    }

    fn insert(&mut self, v: Var, act: &[f64]) {
        if self.pos.len() <= v {
            self.pos.resize(v + 1, None);
        }
        if v == 0 || self.pos[v].is_some() {
            return;
        }
        self.heap.push(v);
        let i = self.heap.len() - 1;
        self.pos[v] = Some(i);
        self.up(i, act);
    }

    fn update(&mut self, v: Var, act: &[f64]) {
        if let Some(Some(i)) = self.pos.get(v) {
            self.up(*i, act);
        }
    }

    fn pop(&mut self, act: &[f64]) -> Option<Var> {
        if self.heap.is_empty() {
            return None;
        }
        let top = self.heap[0];
        let last = self.heap.pop().unwrap();
        self.pos[top] = None;
        if !self.heap.is_empty() {
            self.heap[0] = last;
            self.pos[last] = Some(0);
            self.down(0, act);
        }
        Some(top)
    }

    fn up(&mut self, mut i: usize, act: &[f64]) {
        let v = self.heap[i];
        while i > 0 {
            let p = (i - 1) / 2;
            if act[self.heap[p]] >= act[v] {
                break;
            }
            self.heap[i] = self.heap[p];
            self.pos[self.heap[i]] = Some(i);
            i = p;
        }
        self.heap[i] = v;
        self.pos[v] = Some(i);
    }

    fn down(&mut self, mut i: usize, act: &[f64]) {
        let v = self.heap[i];
        loop {
            let l = 2 * i + 1;
            if l >= self.heap.len() {
                break;
            }
            let r = l + 1;
            let c = if r < self.heap.len() && act[self.heap[r]] > act[self.heap[l]] {
                r
            } else {
                l
            };
            if act[self.heap[c]] <= act[v] {
                break;
            }
            self.heap[i] = self.heap[c];
            self.pos[self.heap[i]] = Some(i);
            i = c;
        }
        self.heap[i] = v;
        self.pos[v] = Some(i);
    }
}
