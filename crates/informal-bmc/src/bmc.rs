//! Bounded model checking (`informal-bmc`, `informal-bmc-10`, `informal-bmc-65`).

use crate::prep::{prepare_ctx, result, solver, unknown, FORMATS};
use crate::unroll::Unroll;
use crate::verify::{unsafe_certified, PreparedTrace};
use informal_aig::Aig;
use informal_engine::{
    Capabilities, CheckRequest, Effort, Engine, EngineDomain, EngineKind, EngineResult, Progress,
    RunContext, Verdict,
};
use informal_sat::Answer;
use std::time::Instant;

/// Bounded model checking: is a bad state reachable in exactly k steps,
/// for k = 0, 1, 2, ...? Never proves. With `step` > 1 it unrolls `step`
/// frames per SAT call and asks for a bad state in any of them (rIC3's
/// `bmc --step N`): far fewer calls on deep bugs, and the depth reported
/// is the first bad frame of the model found, not always the shortest.
pub struct Bmc {
    pub max_depth: Option<usize>,
    pub step: usize,
}

impl Engine for Bmc {
    fn name(&self) -> &str {
        match self.step {
            0 | 1 => "informal-bmc",
            10 => "informal-bmc-10",
            65 => "informal-bmc-65",
            _ => "informal-bmc-n",
        }
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            formats: FORMATS.to_vec(),
            finds_bugs: true,
            liveness: true,
            domain: EngineDomain::BitLevel,
            kind: EngineKind::Bmc,
            min_effort: if self.step <= 1 {
                Effort::Low
            } else if self.step >= 32 {
                Effort::High
            } else {
                Effort::Standard
            },
            certifies: true,
            deep_step: self.step >= 32,
            ..Default::default()
        }
    }
    /// BMC grows with depth: shed first, deepest steps before shallow.
    fn shed_rank(&self) -> u32 {
        match self.step {
            0 | 1 => 3,
            2..=10 => 4,
            _ => 5,
        }
    }
    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let g = match prepare_ctx(req, ctx) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(e), t0)),
        };
        let step = self.step.max(1);
        let eff_max = self.max_depth.or_else(|| req.max_depth.map(|d| d as usize));
        let mut witness = None;
        let mut cert_trace = None;
        let mut u = Unroll::new(&g, solver(ctx), false);
        let mut k = 0; // first frame not yet checked
        let v = loop {
            if ctx.should_stop() {
                break unknown(ctx.stop_reason());
            }
            if eff_max.is_some_and(|m| k > m) {
                break unknown(format!("bound {} reached", k - 1));
            }
            let mut last = k + step - 1;
            if let Some(m) = eff_max {
                last = last.min(m);
            }
            let mut bads = Vec::new();
            for f in k..=last {
                // A deep step adds many frames before its one SAT call.
                if ctx.should_stop() {
                    break;
                }
                u.add_frame();
                bads.push(u.bad(f));
            }
            if bads.len() < last + 1 - k {
                break unknown(ctx.stop_reason());
            }
            // One call: is any of frames k..=last bad?
            let assume = if let [b] = bads[..] {
                b
            } else {
                let a = u.new_var();
                let mut c = vec![-a];
                c.extend(&bads);
                u.sat.add_clause(&c);
                a
            };
            match u.sat.solve(&[assume]) {
                Answer::Sat => {
                    let d = (k..=last)
                        .zip(&bads)
                        .find(|(_, &b)| u.sat.value(b))
                        .map_or(last, |(f, _)| f);
                    // Never report a trace we cannot replay.
                    if let Err(e) = replay(&g, &u, d) {
                        break unknown(format!(
                            "internal error: counterexample does not replay: {e}"
                        ));
                    }
                    witness = Some(named_witness(&g, &u, d));
                    cert_trace = Some(PreparedTrace::from_unroll(&g, &u, d));
                    break Verdict::Unsafe {
                        depth: Some(d as u32),
                    };
                }
                Answer::Unsat => {
                    for &b in &bads {
                        u.sat.add_clause(&[-b]);
                    }
                    if assume != bads[0] || bads.len() > 1 {
                        u.sat.add_clause(&[-assume]);
                    }
                }
                Answer::Unknown => break unknown(ctx.stop_reason()),
            }
            ctx.report(Progress {
                engine: self.name().into(),
                bound: Some(last as u32),
                note: String::new(),
                output: None,
            });
            k = last + 1;
        };
        let mut r = result(
            self.name(),
            unsafe_certified(req, v, ctx, cert_trace.map(|t| (&*g, t))),
            t0,
        );
        r.witness = witness;
        Ok(r)
    }
}

/// The solver's counterexample, frames 0..=d, by signal name: free initial
/// latch values (`init`) and input values per frame (`@f`), for every input
/// and latch the model names (Btor2 symbols survive bit-blasting,
/// simplification and scorr as `name` or `name[i]`). Unnamed ones are left
/// out. Format `informalw 1`, read by informal-fpv to build a VCD on the original
/// model (docs/ui/trace-format.md). Export only: nothing here is searched.
pub fn named_witness(g: &Aig, u: &Unroll, d: usize) -> String {
    let bit = |b: bool| if b { '1' } else { '0' };
    let mut s = format!("informalw 1\ndepth {d}\ninit");
    for l in &g.latches {
        if matches!(l.init, informal_aig::Init::Zero | informal_aig::Init::One) {
            continue;
        }
        if let Some(name) = g.names.get(&informal_aig::var(l.lit)) {
            s += &format!(" {name}={}", bit(u.sat.value(u.lit(0, l.lit))));
        }
    }
    for f in 0..=d {
        s += &format!("\n@{f}");
        for &i in &g.inputs {
            if let Some(name) = g.names.get(&informal_aig::var(i)) {
                s += &format!(" {name}={}", bit(u.sat.value(u.lit(f, i))));
            }
        }
    }
    s.push('\n');
    s
}

/// Simulate `g` with the inputs (and free initial latch values) of the
/// solver's model, frames 0..=d: the constraints must hold in every frame
/// and some bad output in frame d. Checks the SAT encoding end to end.
pub fn replay(g: &Aig, u: &Unroll, d: usize) -> Result<(), String> {
    let n = g.nodes.len();
    let lv = |val: &[bool], l: informal_aig::Lit| {
        val[informal_aig::var(l) as usize] ^ informal_aig::is_neg(l)
    };
    let mut state: Vec<bool> = g
        .latches
        .iter()
        .map(|l| match l.init {
            informal_aig::Init::Zero => false,
            informal_aig::Init::One => true,
            _ => u.sat.value(u.lit(0, l.lit)),
        })
        .collect();
    let mut val = vec![false; n];
    for f in 0..=d {
        let mut li = 0;
        for v in 0..n {
            val[v] = match g.nodes[v] {
                informal_aig::Node::Const => false,
                informal_aig::Node::Input => u.sat.value(u.lit(f, 2 * v as u32)),
                informal_aig::Node::Latch => {
                    li += 1;
                    state[li - 1]
                }
                informal_aig::Node::And(a, b) => lv(&val, a) && lv(&val, b),
            };
        }
        if f == 0 {
            for l in &g.latches {
                if let informal_aig::Init::Expr(e) = l.init {
                    if lv(&val, l.lit) != lv(&val, e) {
                        return Err("initial value expression".into());
                    }
                }
            }
        }
        if let Some(c) = g.constraints.iter().position(|&c| !lv(&val, c)) {
            return Err(format!("constraint {c} fails at frame {f}"));
        }
        if f == d && !g.bads.iter().any(|&b| lv(&val, b)) {
            return Err(format!("no bad output at frame {d}"));
        }
        for (i, l) in g.latches.iter().enumerate() {
            state[i] = lv(&val, l.next);
        }
    }
    Ok(())
}
