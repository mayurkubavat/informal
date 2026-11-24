//! Exact symbolic ROBDD model checking engine (`informal-bdd`).
//!
//! Uses a zero-dependency pure-Rust complemented-edge ROBDD manager ([`bdd`])
//! with Burch–Clarke–Long partitioned transition clusters and early
//! quantification ([`reach`]). Excels on deep sequential reachability and
//! small-to-medium control/FSM cones (`<= 200` state bits), certifying every
//! `Safe` proof via Minato–Morreale ISOP + [`informal_bmc::certify_lemmas`]
//! and every `Unsafe` trace via [`informal_bmc::unsafe_certified_at`].

pub mod bdd;
pub mod reach;

use informal_bmc::{
    certify_lemmas, fold_on, prepare_ctx_fold, result, unknown, unsafe_certified_at, FORMATS,
};
use informal_engine::{
    Capabilities, CheckRequest, Effort, Engine, EngineDomain, EngineKind, EngineResult, RunContext,
    Verdict,
};
use reach::{run_reachability, ReachOutcome, DEFAULT_MAX_ISOP_CUBES, DEFAULT_MAX_NODES};
use std::sync::Arc;
use std::time::Instant;

/// Maximum state-bit count routed to `informal-bdd` before handing off to
/// SAT/IC3 engines.
pub const MAX_STATE_BITS: u64 = 200;

/// Exact symbolic ROBDD reachability engine.
pub struct BddEngine {
    pub max_nodes: usize,
    pub max_isop_cubes: usize,
}

impl Default for BddEngine {
    fn default() -> Self {
        Self {
            max_nodes: std::env::var("INFORMAL_BDD_MAX_NODES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_MAX_NODES),
            max_isop_cubes: DEFAULT_MAX_ISOP_CUBES,
        }
    }
}

impl Engine for BddEngine {
    fn name(&self) -> &str {
        "informal-bdd"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            formats: FORMATS.to_vec(),
            proves: true,
            finds_bugs: true,
            liveness: true,
            domain: EngineDomain::BitLevel,
            kind: EngineKind::Bdd,
            min_effort: Effort::Standard,
            max_state_bits: Some(MAX_STATE_BITS),
            certifies: true,
            ..Default::default()
        }
    }

    /// Shed BDD before IC3/k-induction under memory pressure.
    fn shed_rank(&self) -> u32 {
        4
    }

    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let g = match prepare_ctx_fold(req, ctx, fold_on()) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(e), t0)),
        };
        if g.latches.len() > MAX_STATE_BITS as usize {
            return Ok(result(
                self.name(),
                unknown(format!(
                    "model has {} state bits (BDD cap {MAX_STATE_BITS})",
                    g.latches.len()
                )),
                t0,
            ));
        }

        let max_depth = req.max_depth.map(|d| d as usize);
        match run_reachability(&g, max_depth, self.max_nodes, self.max_isop_cubes, ctx) {
            ReachOutcome::Safe(lemmas) => {
                let checked = certify_lemmas(req, &g, &lemmas, ctx);
                Ok(result(self.name(), checked, t0))
            }
            ReachOutcome::Unsafe {
                depth,
                trace,
                witness,
            } => {
                let checked = unsafe_certified_at(
                    req,
                    Verdict::Unsafe {
                        depth: Some(depth as u32),
                    },
                    ctx,
                    Some((&g, trace)),
                    g.folded.is_some(),
                );
                let mut r = result(self.name(), checked, t0);
                if matches!(r.verdict, Verdict::Unsafe { .. }) {
                    r.witness = Some(witness);
                }
                Ok(r)
            }
            ReachOutcome::Aborted(reason) => Ok(result(self.name(), unknown(reason), t0)),
        }
    }
}

/// The in-house BDD engine fleet.
pub fn engines() -> Vec<Arc<dyn Engine>> {
    vec![Arc::new(BddEngine::default())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use informal_engine::{Certificate, ModelFormat};
    use std::time::Duration;

    fn run_btor2(tag: &str, text: &str) -> EngineResult {
        let dir = std::env::temp_dir().join(format!("informal-bdd-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.btor2");
        std::fs::write(&path, text).unwrap();
        let req = CheckRequest::new(path, ModelFormat::Btor2, Duration::from_secs(15));
        BddEngine::default()
            .check(&req, &RunContext::new(req.timeout))
            .unwrap()
    }

    #[test]
    fn bdd_solves_deep_counter_bug_and_certifies_safe_invariants() {
        if informal_sat::Cadical::signature().is_none() {
            return;
        }
        // 1. Deep 10-bit counter bug at depth 500 (exact minimum depth!).
        let deep_bug = "\
1 sort bitvec 10
2 sort bitvec 1
3 zero 1
4 one 1
5 state 1 cnt
6 init 1 5 3
7 add 1 5 4
8 next 1 5 7
9 constd 1 500
10 eq 2 5 9
11 bad 10
";
        let r_bug = run_btor2("deep-500", deep_bug);
        assert_eq!(r_bug.verdict, Verdict::Unsafe { depth: Some(500) });
        assert!(matches!(r_bug.certificate, Some(Certificate::Trace { .. })));
        assert!(r_bug.witness.is_some());

        // 2. Safe 10-bit wrapping counter (0..=600, bad == 700): certified ISOP invariant!
        let safe_wrap = "\
1 sort bitvec 10
2 sort bitvec 1
3 zero 1
4 one 1
5 constd 1 600
6 state 1 cnt
7 init 1 6 3
8 eq 2 6 5
9 add 1 6 4
10 ite 1 8 3 9
11 next 1 6 10
12 constd 1 700
13 eq 2 6 12
14 bad 13
";
        let r_safe = run_btor2("safe-wrap-10", safe_wrap);
        assert_eq!(r_safe.verdict, Verdict::Safe);
        assert!(matches!(
            r_safe.certificate,
            Some(Certificate::Invariant { .. })
        ));

        // 3. 16-bit one-hot ring FSM: invariant is popcount == 1 (bad: bit 0 & bit 8).
        let one_hot_16 = "\
1 sort bitvec 16
2 sort bitvec 1
3 one 1
4 state 1 ring
5 init 1 4 3
6 slice 2 4 15 15
7 sort bitvec 15
8 slice 7 4 14 0
9 concat 1 8 6
10 next 1 4 9
11 slice 2 4 0 0
12 slice 2 4 8 8
13 and 2 11 12
14 bad 13
";
        let r_oh = run_btor2("onehot-16", one_hot_16);
        assert_eq!(r_oh.verdict, Verdict::Safe);
        assert!(matches!(
            r_oh.certificate,
            Some(Certificate::Invariant { .. })
        ));

        // 4. Constrained model with input constraint.
        let constrained = "\
1 sort bitvec 3
2 sort bitvec 1
3 zero 1
4 one 1
5 state 1 cnt
6 init 1 5 3
7 input 2 en
8 not 2 7
9 constraint 8
10 add 1 5 4
11 ite 1 7 10 5
12 next 1 5 11
13 constd 1 4
14 eq 2 5 13
15 bad 14
";
        let r_con = run_btor2("constrained", constrained);
        assert_eq!(r_con.verdict, Verdict::Safe);
        assert!(matches!(
            r_con.certificate,
            Some(Certificate::Invariant { .. })
        ));
    }

    #[test]
    fn bdd_honors_cancel_token_under_one_second() {
        let dir = std::env::temp_dir().join(format!("informal-bdd-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hard.btor2");
        std::fs::write(
            &path,
            "\
1 sort bitvec 24
2 sort bitvec 1
3 zero 1
4 one 1
5 state 1 cnt
6 init 1 5 3
7 add 1 5 4
8 next 1 5 7
9 ones 1
10 eq 2 5 9
11 bad 10
",
        )
        .unwrap();
        let req = CheckRequest::new(path, ModelFormat::Btor2, Duration::from_secs(30));
        let ctx = RunContext::new(req.timeout);
        let cancel = ctx.cancel.clone();
        let (t_cancel, res) = std::thread::scope(|s| {
            let h = s.spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                let at = Instant::now();
                cancel.cancel();
                at
            });
            let r = BddEngine::default().check(&req, &ctx).unwrap();
            (h.join().unwrap(), r)
        });
        let latency = Instant::now().saturating_duration_since(t_cancel);
        assert!(!res.verdict.is_definitive(), "{res:?}");
        assert!(latency < Duration::from_secs(1), "latency={latency:?}");
    }
}
