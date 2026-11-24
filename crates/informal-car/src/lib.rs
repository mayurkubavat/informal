//! Native Complementary Approximate Reachability engines (`informal-car`,
//! `informal-bcar`).
//!
//! - [`CarEngine`] with `backward: false` (`informal-car`): Forward CAR with a
//!   persistent backward `U`-sequence of bad-reaching states and monotone
//!   forward `O`-frames.
//! - [`CarEngine`] with `backward: true` (`informal-bcar`): Backward CAR with
//!   a persistent forward `U`-sequence of reachable states and backward
//!   `O`-frames of safe cubes.

pub mod bcar;
pub mod fcar;

use bcar::run_bcar;
use fcar::{run_fcar, CarOutcome};
use informal_bmc::{
    certify_lemmas, fold_on, prepare_ctx_fold, result, unknown, unsafe_certified_at, FORMATS,
};
use informal_engine::{
    Capabilities, CheckRequest, Effort, Engine, EngineDomain, EngineKind, EngineResult, RunContext,
    Verdict,
};
use std::sync::Arc;
use std::time::Instant;

/// Complementary Approximate Reachability engine (Forward or Backward mode).
pub struct CarEngine {
    pub backward: bool,
}

impl Engine for CarEngine {
    fn name(&self) -> &str {
        if self.backward {
            "informal-bcar"
        } else {
            "informal-car"
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            formats: FORMATS.to_vec(),
            proves: true,
            finds_bugs: true,
            liveness: true,
            lemma_export: true,
            lemma_import: !self.backward,
            domain: EngineDomain::BitLevel,
            kind: EngineKind::Car,
            min_effort: if self.backward {
                Effort::High
            } else {
                Effort::Standard
            },
            certifies: true,
            ..Default::default()
        }
    }

    fn shed_rank(&self) -> u32 {
        3
    }

    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let g = match prepare_ctx_fold(req, ctx, fold_on()) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(e), t0)),
        };
        let max_depth = req.max_depth.map(|d| d as usize);
        let outcome = if self.backward {
            run_bcar(&g, max_depth, ctx)
        } else {
            run_fcar(&g, max_depth, ctx)
        };
        match outcome {
            CarOutcome::Safe(lemmas) => {
                let checked = certify_lemmas(req, &g, &lemmas, ctx);
                Ok(result(self.name(), checked, t0))
            }
            CarOutcome::Unsafe {
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
            CarOutcome::Aborted(reason) => Ok(result(self.name(), unknown(reason), t0)),
        }
    }
}

/// The in-house CAR engine fleet (Forward + Backward CAR).
pub fn engines() -> Vec<Arc<dyn Engine>> {
    if informal_sat::Cadical::signature().is_none() {
        return Vec::new();
    }
    vec![
        Arc::new(CarEngine { backward: false }),
        Arc::new(CarEngine { backward: true }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use informal_engine::{Certificate, ModelFormat};
    use std::time::Duration;

    fn run_both(tag: &str, text: &str) -> (EngineResult, EngineResult) {
        let dir = std::env::temp_dir().join(format!("informal-car-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.btor2");
        std::fs::write(&path, text).unwrap();
        let req = CheckRequest::new(path, ModelFormat::Btor2, Duration::from_secs(15));
        let rf = CarEngine { backward: false }
            .check(&req, &RunContext::new(req.timeout))
            .unwrap();
        let rb = CarEngine { backward: true }
            .check(&req, &RunContext::new(req.timeout))
            .unwrap();
        (rf, rb)
    }

    #[test]
    fn fcar_and_bcar_prove_safe_and_find_deep_bugs_with_certificates() {
        if informal_sat::Cadical::signature().is_none() {
            return;
        }
        // 1. Safe wrapping 4-bit counter (0..=9, bad == 12).
        let safe_cnt = "\
1 sort bitvec 4
2 sort bitvec 1
3 zero 1
4 one 1
5 constd 1 9
6 state 1 cnt
7 init 1 6 3
8 eq 2 6 5
9 add 1 6 4
10 ite 1 8 3 9
11 next 1 6 10
12 constd 1 12
13 eq 2 6 12
14 bad 13
";
        let (rf, rb) = run_both("safe-cnt", safe_cnt);
        assert_eq!(rf.verdict, Verdict::Safe);
        assert_eq!(rb.verdict, Verdict::Safe);
        assert!(matches!(
            rf.certificate,
            Some(Certificate::Invariant { .. })
        ));
        assert!(matches!(
            rb.certificate,
            Some(Certificate::Invariant { .. })
        ));

        // 2. Buggy 5-bit counter hitting 20 at depth 20.
        let bug_cnt = "\
1 sort bitvec 5
2 sort bitvec 1
3 zero 1
4 one 1
5 state 1 cnt
6 init 1 5 3
7 add 1 5 4
8 next 1 5 7
9 constd 1 20
10 eq 2 5 9
11 bad 10
";
        let (rf, rb) = run_both("bug-20", bug_cnt);
        assert_eq!(rf.verdict, Verdict::Unsafe { depth: Some(20) });
        assert_eq!(rb.verdict, Verdict::Unsafe { depth: Some(20) });
        assert!(matches!(rf.certificate, Some(Certificate::Trace { .. })));
        assert!(matches!(rb.certificate, Some(Certificate::Trace { .. })));

        // 3. Constrained model where constraint holds cnt == 0.
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
13 constd 1 5
14 eq 2 5 13
15 bad 14
";
        let (rf, rb) = run_both("constrained", constrained);
        assert_eq!(rf.verdict, Verdict::Safe);
        assert_eq!(rb.verdict, Verdict::Safe);
    }
}
