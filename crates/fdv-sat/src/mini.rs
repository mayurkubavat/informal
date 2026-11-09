//! A small CDCL solver for the many short queries IC3 makes.
//!
//! The round-2 research found that IC3 queries are millisecond-scale and
//! that a lean solver beats CaDiCaL there (GipSAT is 3.61x faster than
//! MiniSat on them). This is the first step: MiniSat-style CDCL with two
//! watched literals, first-UIP learning with clause minimization, VSIDS on
//! a binary heap, phase saving, Luby restarts, assumptions with a final
//! conflict analysis (for `failed`), and level-0 simplification that drops
//! satisfied clauses, so activation-literal clauses retired by a unit do
//! not pile up. COI-restricted decisions and bucket VSIDS come next.
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

struct Clause {
    lits: Vec<Lit>,
    learnt: bool,
    activity: f32,
    deleted: bool,
}

#[derive(Clone, Copy)]
struct Watch {
    clause: u32,
    blocker: Lit,
}

pub struct Mini {
    clauses: Vec<Clause>,
    watches: Vec<Vec<Watch>>, // by literal code: clauses watching ¬lit... see `watch`
    assign: Vec<Val>,         // by variable
    level: Vec<u32>,
    reason: Vec<Option<u32>>,
    phase: Vec<bool>,
    activity: Vec<f64>,
    var_inc: f64,
    cla_inc: f32,
    heap: Heap,
    trail: Vec<Lit>,
    trail_lim: Vec<usize>,
    qhead: usize,
    seen: Vec<u8>,
    ok: bool,
    model: Vec<bool>,
    conflict: Vec<Lit>, // failed assumptions (negated), after Unsat
    terminate: Option<Box<dyn Fn() -> bool + Send>>,
    units_since_simplify: usize,
    /// Decision domain for the current solve (empty: every variable).
    in_domain: Vec<bool>,
    restricted: bool,
    /// The domain's variables by activity, highest first: restricted
    /// solves decide only on these and leave the global heap alone.
    dom: Vec<Var>,
    /// Every `dom` entry before this index is assigned.
    dom_head: usize,
    learnts: usize,
    max_learnts: f64,
    pub conflicts: u64,
}

/// Process-wide counters for profiling (FDV_IC3_STATS).
pub static DECISIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static DOMAIN_VARS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static SOLVES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static PROPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static SAT_SOLVES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static SAT_DECISIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static SAT_NANOS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static MAX_VARS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static ALL_NANOS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Default for Mini {
    fn default() -> Self {
        Mini {
            clauses: Vec::new(),
            watches: vec![Vec::new(); 2],
            assign: vec![Val::Undef],
            level: vec![0],
            reason: vec![None],
            phase: vec![false],
            activity: vec![0.0],
            var_inc: 1.0,
            cla_inc: 1.0,
            heap: Heap::default(),
            trail: Vec::new(),
            trail_lim: Vec::new(),
            qhead: 0,
            seen: vec![0],
            ok: true,
            model: Vec::new(),
            conflict: Vec::new(),
            terminate: None,
            units_since_simplify: 0,
            in_domain: Vec::new(),
            restricted: false,
            dom: Vec::new(),
            dom_head: 0,
            learnts: 0,
            max_learnts: 2000.0,
            conflicts: 0,
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
            self.phase.push(false);
            self.activity.push(0.0);
            self.seen.push(0);
            self.watches.push(Vec::new());
            self.watches.push(Vec::new());
            self.heap.insert(n, &self.activity);
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

    fn decision_level(&self) -> u32 {
        self.trail_lim.len() as u32
    }

    fn enqueue(&mut self, l: Lit, reason: Option<u32>) {
        let v = var(l);
        self.assign[v] = if l > 0 { Val::True } else { Val::False };
        self.level[v] = self.decision_level();
        self.reason[v] = reason;
        self.trail.push(l);
    }

    fn attach(&mut self, ci: u32) {
        let c = &self.clauses[ci as usize];
        let (a, b) = (c.lits[0], c.lits[1]);
        // Watch lists are indexed by the literal whose falsification wakes
        // the clause.
        self.watches[code(-a)].push(Watch {
            clause: ci,
            blocker: b,
        });
        self.watches[code(-b)].push(Watch {
            clause: ci,
            blocker: a,
        });
    }

    /// Unit propagation; the conflicting clause on failure.
    fn propagate(&mut self) -> Option<u32> {
        while self.qhead < self.trail.len() {
            let p = self.trail[self.qhead];
            self.qhead += 1;
            let mut ws = std::mem::take(&mut self.watches[code(p)]);
            let mut i = 0;
            let mut j = 0;
            let mut confl = None;
            while i < ws.len() {
                let w = ws[i];
                i += 1;
                if self.lit_value(w.blocker) == Val::True {
                    ws[j] = w;
                    j += 1;
                    continue;
                }
                let ci = w.clause as usize;
                if self.clauses[ci].deleted {
                    continue;
                }
                let false_lit = -p;
                {
                    let lits = &mut self.clauses[ci].lits;
                    if lits[0] == false_lit {
                        lits.swap(0, 1);
                    }
                }
                let first = self.clauses[ci].lits[0];
                if first != w.blocker && self.lit_value(first) == Val::True {
                    ws[j] = Watch {
                        clause: w.clause,
                        blocker: first,
                    };
                    j += 1;
                    continue;
                }
                // Look for a new watch.
                let len = self.clauses[ci].lits.len();
                let mut found = false;
                for k in 2..len {
                    let l = self.clauses[ci].lits[k];
                    if self.lit_value(l) != Val::False {
                        self.clauses[ci].lits.swap(1, k);
                        self.watches[code(-l)].push(Watch {
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
            // New watches may have been pushed to this list meanwhile.
            let added = std::mem::take(&mut self.watches[code(p)]);
            ws.extend(added);
            self.watches[code(p)] = ws;
            if confl.is_some() {
                self.qhead = self.trail.len();
                return confl;
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
        self.heap.update(v, &self.activity);
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

    /// First-UIP analysis: (learnt clause with the asserting literal first,
    /// backjump level).
    fn analyze(&mut self, mut confl: u32) -> (Vec<Lit>, u32) {
        let mut learnt = vec![0];
        let mut path = 0;
        let mut p: Lit = 0;
        let mut idx = self.trail.len();
        loop {
            if self.clauses[confl as usize].learnt {
                self.bump_clause(confl as usize);
            }
            let start = usize::from(p != 0);
            let lits = self.clauses[confl as usize].lits.clone();
            for &q in &lits[start..] {
                let v = var(q);
                if self.seen[v] == 0 && self.level[v] > 0 {
                    self.seen[v] = 1;
                    self.bump_var(v);
                    if self.level[v] >= self.decision_level() {
                        path += 1;
                    } else {
                        learnt.push(q);
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
        learnt[0] = -p;
        // Minimize: drop literals implied by others in the clause.
        let keep: Vec<bool> = learnt
            .iter()
            .enumerate()
            .map(|(i, &l)| i == 0 || !self.redundant(l))
            .collect();
        let mut out: Vec<Lit> = learnt
            .iter()
            .zip(&keep)
            .filter(|(_, &k)| k)
            .map(|(&l, _)| l)
            .collect();
        for &l in &learnt {
            self.seen[var(l)] = 0;
        }
        // Backjump level: the highest level among the rest; put it second.
        let mut bt = 0;
        if out.len() > 1 {
            let mut max_i = 1;
            for i in 2..out.len() {
                if self.level[var(out[i])] > self.level[var(out[max_i])] {
                    max_i = i;
                }
            }
            out.swap(1, max_i);
            bt = self.level[var(out[1])];
        }
        (out, bt)
    }

    /// Local minimization: `l` is redundant if its reason's other literals
    /// are all in the clause (marked seen) or at level 0.
    fn redundant(&self, l: Lit) -> bool {
        match self.reason[var(l)] {
            None => false,
            Some(r) => self.clauses[r as usize]
                .lits
                .iter()
                .skip(1)
                .all(|&q| self.seen[var(q)] != 0 || self.level[var(q)] == 0),
        }
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
                    for &q in self.clauses[r as usize].lits.iter().skip(1) {
                        if self.level[var(q)] > 0 {
                            self.seen[var(q)] = 1;
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
        for i in (lim..self.trail.len()).rev() {
            let v = var(self.trail[i]);
            self.phase[v] = self.trail[i] > 0;
            self.assign[v] = Val::Undef;
            self.reason[v] = None;
            self.heap.insert(v, &self.activity);
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
                c.learnt && !c.deleted && c.lits.len() > 2 && !self.locked(i)
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
        }
    }

    fn locked(&self, ci: usize) -> bool {
        let l = self.clauses[ci].lits[0];
        self.lit_value(l) == Val::True && self.reason[var(l)] == Some(ci as u32)
    }

    /// At level 0: drop satisfied clauses and rebuild watches.
    fn simplify(&mut self) {
        debug_assert_eq!(self.decision_level(), 0);
        for ci in 0..self.clauses.len() {
            if self.clauses[ci].deleted {
                continue;
            }
            if self.clauses[ci]
                .lits
                .iter()
                .any(|&l| self.lit_value(l) == Val::True)
            {
                self.clauses[ci].deleted = true;
                if self.clauses[ci].learnt {
                    self.learnts -= 1;
                }
            }
        }
        // Compact: renumber live clauses and rebuild every watch list.
        for r in self.reason.iter_mut() {
            *r = None; // level-0 reasons are never needed
        }
        self.clauses.retain(|c| !c.deleted);
        for w in self.watches.iter_mut() {
            w.clear();
        }
        for ci in 0..self.clauses.len() {
            self.attach(ci as u32);
        }
        self.units_since_simplify = 0;
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
        loop {
            if let Some(confl) = self.propagate() {
                self.conflicts += 1;
                conflicts += 1;
                if self.decision_level() == 0 {
                    self.ok = false;
                    return Some(Answer::Unsat);
                }
                let (learnt, bt) = self.analyze(confl);
                self.cancel_until(bt);
                if learnt.len() == 1 {
                    self.enqueue(learnt[0], None);
                } else {
                    let ci = self.clauses.len() as u32;
                    self.clauses.push(Clause {
                        lits: learnt.clone(),
                        learnt: true,
                        activity: 0.0,
                        deleted: false,
                    });
                    self.attach(ci);
                    self.bump_clause(ci as usize);
                    self.learnts += 1;
                    self.enqueue(learnt[0], Some(ci));
                }
                self.var_inc /= 0.95;
                self.cla_inc /= 0.999;
                if conflicts.is_multiple_of(256) && self.terminate.as_ref().is_some_and(|f| f()) {
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
            DECISIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.enqueue(decision, None);
        }
    }
}

impl Sat for Mini {
    fn add_clause(&mut self, lits: &[Lit]) {
        if !self.ok {
            return;
        }
        self.cancel_until(0);
        let mut c: Vec<Lit> = Vec::with_capacity(lits.len());
        for &l in lits {
            self.ensure(var(l));
        }
        for &l in lits {
            match self.lit_value(l) {
                Val::True => return,
                Val::False => continue,
                Val::Undef => {
                    if c.contains(&-l) {
                        return;
                    }
                    if !c.contains(&l) {
                        c.push(l);
                    }
                }
            }
        }
        match c.len() {
            0 => self.ok = false,
            1 => {
                self.enqueue(c[0], None);
                if self.propagate().is_some() {
                    self.ok = false;
                }
                self.units_since_simplify += 1;
            }
            _ => {
                let ci = self.clauses.len() as u32;
                self.clauses.push(Clause {
                    lits: c,
                    learnt: false,
                    activity: 0.0,
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
        if self.units_since_simplify > 64 {
            self.simplify();
        }
        let t0 = std::time::Instant::now();
        let d0 = DECISIONS.load(std::sync::atomic::Ordering::Relaxed);
        let mut restart = 0;
        let ans = loop {
            match self.search(assumptions, 100 * Mini::luby(restart)) {
                None => break Answer::Unknown,
                Some(Answer::Unknown) => restart += 1,
                Some(a) => break a,
            }
        };
        SOLVES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        {
            use std::sync::atomic::Ordering::Relaxed;
            let ns = t0.elapsed().as_nanos() as u64;
            ALL_NANOS.fetch_add(ns, Relaxed);
            MAX_VARS.fetch_max(self.assign.len() as u64, Relaxed);
            if ans == Answer::Sat {
                SAT_SOLVES.fetch_add(1, Relaxed);
                SAT_NANOS.fetch_add(ns, Relaxed);
                SAT_DECISIONS.fetch_add(DECISIONS.load(Relaxed) - d0, Relaxed);
            }
        }
        PROPS.fetch_add(
            self.trail.len() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        if ans == Answer::Sat {
            self.model = self.assign.iter().map(|&v| v == Val::True).collect();
        }
        self.cancel_until(0);
        ans
    }

    fn value(&self, lit: Lit) -> bool {
        self.model.get(var(lit)).is_some_and(|&v| v == (lit > 0))
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
        // Decisions follow the caller's order (IC3 puts latches and
        // inputs first, so gates mostly follow by propagation).
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
        // `conflict` holds negations of the failed assumptions.
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
