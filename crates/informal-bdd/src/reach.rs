//! Exact symbolic forward reachability with Burch–Clarke–Long partitioned
//! transition clusters, early quantification, shortest-CEX backward extraction,
//! and Minato–Morreale ISOP invariant extraction.

use crate::bdd::{neg, Abort, Bdd, BddMgr, FALSE, TRUE};
use informal_aig::{is_neg, var, Aig, Init, Lit, Node};
use informal_bmc::{ic3, PreparedTrace};
use informal_engine::{Progress, RunContext};

/// Default ROBDD node cap before aborting gracefully so BDD blowup never
/// starves SAT/IC3 portfolio peers.
pub const DEFAULT_MAX_NODES: usize = 500_000;
/// Maximum number of ISOP blocked cubes to export as a CNF invariant certificate.
pub const DEFAULT_MAX_ISOP_CUBES: usize = 8_192;
/// Target BDD size threshold when grouping per-latch transition relations into
/// partitioned clusters (Burch–Clarke–Long).
const CLUSTER_NODE_THRESHOLD: usize = 1024;

/// Outcome of exact symbolic reachability on a prepared AIG.
pub enum ReachOutcome {
    /// Fixpoint reached with zero bad states; `lemmas` are blocked cubes over
    /// `g.latches` whose negation equals the exact reachable state set.
    Safe(Vec<ic3::Cube>),
    /// Minimum-depth counterexample found and traced backward through onion rings.
    Unsafe {
        depth: usize,
        trace: PreparedTrace,
        witness: String,
    },
    /// Aborted due to cancellation, timeout, memory limit, node cap, or cube cap.
    Aborted(&'static str),
}

/// Variable layout mapping AIG latches and inputs onto interleaved ROBDD levels.
struct Layout {
    num_vars: usize,
    /// BDD variable index for current-state latch `k`.
    curr_var: Vec<u32>,
    /// BDD variable index for next-state latch `k` (`curr_var[k] + 1`).
    next_var: Vec<u32>,
    /// BDD variable index for primary input `m`.
    input_var: Vec<u32>,
    /// Reverse map from BDD variable index -> latch index (for current-state vars).
    var_to_latch: Vec<Option<usize>>,
    /// Order-preserving rename map (`next_var[k] -> curr_var[k]`).
    next_to_curr: Vec<u32>,
}

impl Layout {
    /// Interleave `(s_k, s'_k)` pairs so `s'_k` is always directly below `s_k`
    /// (`level(s'_k) = level(s_k) + 1`), placing each primary input immediately
    /// before the first latch whose cone uses it. Because every `(s_k, s'_k)`
    /// pair keeps the same relative order, renaming `s' -> s` is strictly
    /// order-preserving in `O(|BDD|)`.
    fn build(g: &Aig) -> Self {
        let n_latches = g.latches.len();
        let n_inputs = g.inputs.len();

        let mut input_of_node = vec![None; g.nodes.len()];
        for (mi, &ilit) in g.inputs.iter().enumerate() {
            input_of_node[var(ilit) as usize] = Some(mi);
        }

        // Find the earliest latch whose next-state cone mentions each input.
        let mut first_latch_for_input = vec![n_latches; n_inputs];
        let mut seen = vec![0u32; g.nodes.len()];
        let mut stack = Vec::new();
        for (li, l) in g.latches.iter().enumerate() {
            let epoch = (li + 1) as u32;
            stack.clear();
            stack.push(var(l.next) as usize);
            while let Some(v) = stack.pop() {
                if seen[v] == epoch {
                    continue;
                }
                seen[v] = epoch;
                if let Some(mi) = input_of_node[v] {
                    if first_latch_for_input[mi] == n_latches {
                        first_latch_for_input[mi] = li;
                    }
                }
                if let Node::And(a, b) = g.nodes[v] {
                    stack.push(var(a) as usize);
                    stack.push(var(b) as usize);
                }
            }
        }

        let mut inputs_before: Vec<Vec<usize>> = vec![Vec::new(); n_latches + 1];
        for (mi, &li) in first_latch_for_input.iter().enumerate() {
            inputs_before[li].push(mi);
        }

        let mut curr_var = vec![0u32; n_latches];
        let mut next_var = vec![0u32; n_latches];
        let mut input_var = vec![0u32; n_inputs];
        let mut next_level = 0u32;

        for li in 0..n_latches {
            for &mi in &inputs_before[li] {
                input_var[mi] = next_level;
                next_level += 1;
            }
            curr_var[li] = next_level;
            next_var[li] = next_level + 1;
            next_level += 2;
        }
        for &mi in &inputs_before[n_latches] {
            input_var[mi] = next_level;
            next_level += 1;
        }

        let num_vars = next_level as usize;
        let mut var_to_latch = vec![None; num_vars];
        let mut next_to_curr: Vec<u32> = (0..num_vars as u32).collect();
        for li in 0..n_latches {
            var_to_latch[curr_var[li] as usize] = Some(li);
            next_to_curr[next_var[li] as usize] = curr_var[li];
        }

        Layout {
            num_vars,
            curr_var,
            next_var,
            input_var,
            var_to_latch,
            next_to_curr,
        }
    }
}

/// Evaluate every AIG node reachable from `roots` into a BDD over `(s, i)`.
struct AigEval<'a, 'ctx> {
    g: &'a Aig,
    layout: &'a Layout,
    mgr: &'a mut BddMgr<'ctx>,
    cache: Vec<Option<Bdd>>,
    latch_idx_of: Vec<Option<usize>>,
    input_idx_of: Vec<Option<usize>>,
}

impl<'a, 'ctx> AigEval<'a, 'ctx> {
    fn new(g: &'a Aig, layout: &'a Layout, mgr: &'a mut BddMgr<'ctx>) -> Self {
        let mut latch_idx_of = vec![None; g.nodes.len()];
        for (li, l) in g.latches.iter().enumerate() {
            latch_idx_of[var(l.lit) as usize] = Some(li);
        }
        let mut input_idx_of = vec![None; g.nodes.len()];
        for (mi, &i) in g.inputs.iter().enumerate() {
            input_idx_of[var(i) as usize] = Some(mi);
        }
        Self {
            g,
            layout,
            mgr,
            cache: vec![None; g.nodes.len()],
            latch_idx_of,
            input_idx_of,
        }
    }

    fn eval_lit(&mut self, lit: Lit) -> Result<Bdd, Abort> {
        let b = self.eval_var(var(lit) as usize)?;
        Ok(if is_neg(lit) { neg(b) } else { b })
    }

    fn eval_var(&mut self, v: usize) -> Result<Bdd, Abort> {
        if let Some(b) = self.cache[v] {
            return Ok(b);
        }
        let b = match self.g.nodes[v] {
            Node::Const => FALSE,
            Node::Input => {
                let mi = self.input_idx_of[v].expect("input index");
                self.mgr.var(self.layout.input_var[mi])?
            }
            Node::Latch => {
                let li = self.latch_idx_of[v].expect("latch index");
                self.mgr.var(self.layout.curr_var[li])?
            }
            Node::And(a, b) => {
                let ba = self.eval_lit(a)?;
                let bb = self.eval_lit(b)?;
                self.mgr.and(ba, bb)?
            }
        };
        self.cache[v] = Some(b);
        Ok(b)
    }
}

/// Partitioned transition cluster with its early-quantification schedule.
struct Cluster {
    bdd: Bdd,
    quant_mask: Vec<bool>,
    quant_id: u32,
}

/// Run exact symbolic forward reachability on `g` up to `max_depth` (if any).
pub fn run_reachability(
    g: &Aig,
    max_depth: Option<usize>,
    max_nodes: usize,
    max_isop_cubes: usize,
    ctx: &RunContext,
) -> ReachOutcome {
    match run_inner(g, max_depth, max_nodes, max_isop_cubes, ctx) {
        Ok(outcome) => outcome,
        Err(Abort::Stopped) => ReachOutcome::Aborted(ctx.stop_reason()),
        Err(Abort::NodeLimit) => ReachOutcome::Aborted("bdd node limit reached"),
        Err(Abort::CubeLimit) => ReachOutcome::Aborted("bdd invariant cube limit reached"),
    }
}

fn run_inner(
    g: &Aig,
    max_depth: Option<usize>,
    max_nodes: usize,
    max_isop_cubes: usize,
    ctx: &RunContext,
) -> Result<ReachOutcome, Abort> {
    let layout = Layout::build(g);
    let mut mgr = BddMgr::new(ctx, max_nodes);

    // 1. Build BDDs for constraints, bads, next-state functions, and init.
    let (c_bdd, bad_bdd, next_bdds, init_raw) = {
        let mut ev = AigEval::new(g, &layout, &mut mgr);
        let mut c_bdd = TRUE;
        for &c in &g.constraints {
            let bc = ev.eval_lit(c)?;
            c_bdd = ev.mgr.and(c_bdd, bc)?;
        }
        let mut bad_bdd = FALSE;
        for &b in &g.bads {
            let bb = ev.eval_lit(b)?;
            bad_bdd = ev.mgr.or(bad_bdd, bb)?;
        }
        let mut next_bdds = Vec::with_capacity(g.latches.len());
        for l in &g.latches {
            next_bdds.push(ev.eval_lit(l.next)?);
        }
        let mut init_raw = TRUE;
        for (li, l) in g.latches.iter().enumerate() {
            let sv = ev.mgr.var(layout.curr_var[li])?;
            let cond = match l.init {
                Init::Zero => neg(sv),
                Init::One => sv,
                Init::Free => TRUE,
                Init::Expr(e) => {
                    let be = ev.eval_lit(e)?;
                    ev.mgr.xnor(sv, be)?
                }
            };
            init_raw = ev.mgr.and(init_raw, cond)?;
        }
        (c_bdd, bad_bdd, next_bdds, init_raw)
    };

    // Input quantification mask (`∃ i. ...`).
    let mut input_mask = vec![false; layout.num_vars];
    for &iv in &layout.input_var {
        input_mask[iv as usize] = true;
    }
    let qid_inputs = mgr.fresh_quant_id();

    let c_state = mgr.exists(c_bdd, &input_mask, qid_inputs)?;
    let init_state = mgr.exists(init_raw, &input_mask, qid_inputs)?;
    let r0 = mgr.and(init_state, c_state)?;
    let bad_c_bdd = mgr.and(c_bdd, bad_bdd)?;
    let bad_state = mgr.exists(bad_c_bdd, &input_mask, qid_inputs)?;

    // Check depth 0 immediately (handles combinational 0-latch models cleanly too).
    if mgr.and(r0, bad_state)? != FALSE {
        return extract_cex(g, &layout, &mut mgr, &[r0], c_bdd, bad_c_bdd, &next_bdds);
    }
    if g.latches.is_empty() {
        return extract_safe_certificate(g, &layout, &mut mgr, r0, max_isop_cubes);
    }

    // 2. Build Burch–Clarke–Long partitioned transition clusters + early-quantification schedule.
    let (pre_mask, pre_qid, clusters) = build_clusters(&layout, &mut mgr, &next_bdds)?;

    // 3. Frontier forward symbolic reachability with shortest-path onion rings.
    let mut reached = r0;
    let mut frontier = r0;
    let mut rings = vec![r0];
    let mut depth = 0usize;

    while frontier != FALSE {
        if ctx.should_stop() {
            return Err(Abort::Stopped);
        }
        if max_depth.is_some_and(|m| depth >= m) {
            return Ok(ReachOutcome::Aborted("depth bound reached"));
        }
        ctx.report(Progress {
            engine: "informal-bdd".into(),
            bound: Some(depth as u32),
            note: format!("bdd nodes {}", mgr.num_nodes()),
            output: None,
        });

        let img = image_step(
            &layout, &mut mgr, frontier, c_bdd, c_state, &pre_mask, pre_qid, &clusters,
        )?;
        let new_states = mgr.and(img, neg(reached))?;
        depth += 1;

        if mgr.and(new_states, bad_state)? != FALSE {
            rings.push(new_states);
            return extract_cex(g, &layout, &mut mgr, &rings, c_bdd, bad_c_bdd, &next_bdds);
        }
        if new_states == FALSE {
            break;
        }
        reached = mgr.or(reached, new_states)?;
        rings.push(new_states);
        frontier = new_states;
    }

    extract_safe_certificate(g, &layout, &mut mgr, reached, max_isop_cubes)
}

fn build_clusters(
    layout: &Layout,
    mgr: &mut BddMgr<'_>,
    next_bdds: &[Bdd],
) -> Result<(Vec<bool>, u32, Vec<Cluster>), Abort> {
    // Build per-latch transition relation `s'_k <=> next_k(s, i)` and greedily
    // conjoin into clusters up to `CLUSTER_NODE_THRESHOLD` growth.
    let mut raw_clusters: Vec<Bdd> = Vec::new();
    let mut cur = TRUE;
    let mut cur_start_nodes = mgr.num_nodes();

    for (li, &nb) in next_bdds.iter().enumerate() {
        let nv = mgr.var(layout.next_var[li])?;
        let rel = mgr.xnor(nv, nb)?;
        let merged = mgr.and(cur, rel)?;
        let grew = mgr.num_nodes().saturating_sub(cur_start_nodes);
        if cur != TRUE && grew > CLUSTER_NODE_THRESHOLD {
            raw_clusters.push(cur);
            cur = rel;
            cur_start_nodes = mgr.num_nodes();
        } else {
            cur = merged;
        }
    }
    if cur != TRUE {
        raw_clusters.push(cur);
    }

    // Determine variables to existentially quantify: all `curr_var` and `input_var`.
    let mut is_quant = vec![false; layout.num_vars];
    for &cv in &layout.curr_var {
        is_quant[cv as usize] = true;
    }
    for &iv in &layout.input_var {
        is_quant[iv as usize] = true;
    }

    // Compute the last cluster whose support mentions each quantifiable variable.
    let mut last_use: Vec<Option<usize>> = vec![None; layout.num_vars];
    for (ci, &cb) in raw_clusters.iter().enumerate() {
        let sup = mgr.support(cb, layout.num_vars);
        for (v, &used) in sup.iter().enumerate() {
            if used && is_quant[v] {
                last_use[v] = Some(ci);
            }
        }
    }

    // Variables never referenced by any transition cluster are quantified before step 0.
    let mut pre_mask = vec![false; layout.num_vars];
    for v in 0..layout.num_vars {
        if is_quant[v] && last_use[v].is_none() {
            pre_mask[v] = true;
        }
    }
    let pre_qid = mgr.fresh_quant_id();

    let mut clusters = Vec::with_capacity(raw_clusters.len());
    for (ci, bdd) in raw_clusters.into_iter().enumerate() {
        let mut quant_mask = vec![false; layout.num_vars];
        for v in 0..layout.num_vars {
            if last_use[v] == Some(ci) {
                quant_mask[v] = true;
            }
        }
        let quant_id = mgr.fresh_quant_id();
        clusters.push(Cluster {
            bdd,
            quant_mask,
            quant_id,
        });
    }

    Ok((pre_mask, pre_qid, clusters))
}

#[allow(clippy::too_many_arguments)]
fn image_step(
    layout: &Layout,
    mgr: &mut BddMgr<'_>,
    frontier: Bdd,
    c_bdd: Bdd,
    c_state: Bdd,
    pre_mask: &[bool],
    pre_qid: u32,
    clusters: &[Cluster],
) -> Result<Bdd, Abort> {
    let mut acc = mgr.and_exists(frontier, c_bdd, pre_mask, pre_qid)?;
    for cl in clusters {
        if acc == FALSE {
            return Ok(FALSE);
        }
        acc = mgr.and_exists(acc, cl.bdd, &cl.quant_mask, cl.quant_id)?;
    }
    let renamed = mgr.rename_preserving_order(acc, &layout.next_to_curr)?;
    mgr.and(renamed, c_state)
}

fn extract_safe_certificate(
    g: &Aig,
    layout: &Layout,
    mgr: &mut BddMgr<'_>,
    reached: Bdd,
    max_isop_cubes: usize,
) -> Result<ReachOutcome, Abort> {
    let unreach = neg(reached);
    let raw_cubes = mgr.isop(unreach, max_isop_cubes)?;
    let mut lemmas: Vec<ic3::Cube> = Vec::with_capacity(raw_cubes.len());
    for cube in raw_cubes {
        let mut lits = Vec::with_capacity(cube.len());
        for (v, pol) in cube {
            let li = layout.var_to_latch[v as usize]
                .expect("reachable state BDD must depend only on current-state latches");
            let l = g.latches[li].lit;
            lits.push(if pol { l } else { informal_aig::neg(l) });
        }
        lits.sort_unstable();
        lemmas.push(lits);
    }
    Ok(ReachOutcome::Safe(lemmas))
}

fn extract_cex(
    g: &Aig,
    layout: &Layout,
    mgr: &mut BddMgr<'_>,
    rings: &[Bdd],
    c_bdd: Bdd,
    bad_c_bdd: Bdd,
    next_bdds: &[Bdd],
) -> Result<ReachOutcome, Abort> {
    let d = rings.len() - 1;
    let mut inputs: Vec<Vec<bool>> = vec![Vec::new(); d + 1];

    // Pick a bad (s_d, i_d) in the final onion ring.
    let end_set = mgr.and(rings[d], bad_c_bdd)?;
    let assign = mgr
        .any_sat(end_set)
        .expect("non-empty bad intersection must have a satisfying assignment");
    let (mut cur_state, end_in) = unpack_assignment(g, layout, &assign);
    inputs[d] = end_in;

    // Walk backward through onion rings `t = d-1 ..= 0`.
    for t in (0..d).rev() {
        let mut pre = mgr.and(rings[t], c_bdd)?;
        for (li, &nb) in next_bdds.iter().enumerate() {
            let target = if cur_state[li] { nb } else { neg(nb) };
            pre = mgr.and(pre, target)?;
        }
        let step_assign = mgr
            .any_sat(pre)
            .expect("onion ring predecessor must be satisfiable");
        let (prev_state, step_in) = unpack_assignment(g, layout, &step_assign);
        cur_state = prev_state;
        inputs[t] = step_in;
    }

    let witness = format_named_witness(g, &cur_state, &inputs);
    let trace = PreparedTrace {
        init: cur_state,
        inputs,
    };
    Ok(ReachOutcome::Unsafe {
        depth: d,
        trace,
        witness,
    })
}

fn unpack_assignment(g: &Aig, layout: &Layout, assign: &[(u32, bool)]) -> (Vec<bool>, Vec<bool>) {
    let mut vals = vec![false; layout.num_vars];
    for &(v, b) in assign {
        vals[v as usize] = b;
    }
    let state = (0..g.latches.len())
        .map(|li| vals[layout.curr_var[li] as usize])
        .collect();
    let inp = (0..g.inputs.len())
        .map(|mi| vals[layout.input_var[mi] as usize])
        .collect();
    (state, inp)
}

fn format_named_witness(g: &Aig, init: &[bool], inputs: &[Vec<bool>]) -> String {
    let bit = |b: bool| if b { '1' } else { '0' };
    let d = inputs.len().saturating_sub(1);
    let mut s = format!("informalw 1\ndepth {d}\ninit");
    for (l, &val) in g.latches.iter().zip(init) {
        if matches!(l.init, Init::Zero | Init::One) {
            continue;
        }
        if let Some(name) = g.names.get(&var(l.lit)) {
            s += &format!(" {name}={}", bit(val));
        }
    }
    for (f, frame_in) in inputs.iter().enumerate() {
        s += &format!("\n@{f}");
        for (&i, &val) in g.inputs.iter().zip(frame_in) {
            if let Some(name) = g.names.get(&var(i)) {
                s += &format!(" {name}={}", bit(val));
            }
        }
    }
    s.push('\n');
    s
}
