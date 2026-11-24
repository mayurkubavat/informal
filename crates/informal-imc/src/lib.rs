//! Native Craig Interpolation-Based Model Checking engine (`informal-imc`).
//!
//! Runs McMillan's interpolation loop ([`imc`]) over CaDiCaL's proof-tracing
//! Craig interpolator ([`informal_sat::Interpolator`]), certifying every
//! `Safe` fixpoint via [`informal_bmc::certify_lemmas`] and every `Unsafe`
//! counterexample via [`informal_bmc::unsafe_certified_at`].

pub mod imc;

use imc::{run_imc, ImcOutcome};
use informal_bmc::{
    certify_lemmas, fold_on, prepare_ctx_fold, result, unknown, unsafe_certified_at, FORMATS,
};
use informal_engine::{
    Capabilities, CheckRequest, Effort, Engine, EngineDomain, EngineKind, EngineResult, RunContext,
    Verdict,
};
use std::sync::Arc;
use std::time::Instant;

/// Native McMillan interpolation-based model checker.
pub struct ImcEngine;

impl Engine for ImcEngine {
    fn name(&self) -> &str {
        "informal-imc"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            formats: FORMATS.to_vec(),
            proves: true,
            finds_bugs: true,
            liveness: true,
            lemma_export: true,
            lemma_import: true,
            domain: EngineDomain::BitLevel,
            kind: EngineKind::Interpolation,
            min_effort: Effort::Standard,
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
        match run_imc(&g, max_depth, ctx) {
            ImcOutcome::Safe(lemmas) => {
                let checked = certify_lemmas(req, &g, &lemmas, ctx);
                Ok(result(self.name(), checked, t0))
            }
            ImcOutcome::Unsafe {
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
            ImcOutcome::Aborted(reason) => Ok(result(self.name(), unknown(reason), t0)),
        }
    }
}

/// The in-house interpolation engine fleet (empty if CaDiCaL proof tracing is unavailable).
pub fn engines() -> Vec<Arc<dyn Engine>> {
    if !informal_sat::Interpolator::available() {
        return Vec::new();
    }
    vec![Arc::new(ImcEngine)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use informal_engine::{Certificate, ModelFormat};
    use std::time::Duration;

    fn run_btor2(tag: &str, text: &str) -> EngineResult {
        let dir = std::env::temp_dir().join(format!("informal-imc-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.btor2");
        std::fs::write(&path, text).unwrap();
        let req = CheckRequest::new(path, ModelFormat::Btor2, Duration::from_secs(15));
        ImcEngine
            .check(&req, &RunContext::new(req.timeout))
            .unwrap()
    }

    #[test]
    fn imc_proves_safe_invariants_and_refutes_bugs_with_certificates() {
        if !informal_sat::Interpolator::available() {
            return;
        }
        // 1. Safe wrapping 4-bit counter (0..=10, bad == 13).
        let safe_cnt = "\
1 sort bitvec 4
2 sort bitvec 1
3 zero 1
4 one 1
5 constd 1 10
6 state 1 cnt
7 init 1 6 3
8 eq 2 6 5
9 add 1 6 4
10 ite 1 8 3 9
11 next 1 6 10
12 constd 1 13
13 eq 2 6 12
14 bad 13
";
        let r_safe = run_btor2("safe-cnt", safe_cnt);
        assert_eq!(r_safe.verdict, Verdict::Safe);
        assert!(matches!(
            r_safe.certificate,
            Some(Certificate::Invariant { .. })
        ));

        // 2. Buggy 4-bit counter hitting 9 at depth 9.
        let bug_cnt = "\
1 sort bitvec 4
2 sort bitvec 1
3 zero 1
4 one 1
5 state 1 cnt
6 init 1 5 3
7 add 1 5 4
8 next 1 5 7
9 constd 1 9
10 eq 2 5 9
11 bad 10
";
        let r_bug = run_btor2("bug-9", bug_cnt);
        assert_eq!(r_bug.verdict, Verdict::Unsafe { depth: Some(9) });
        assert!(matches!(r_bug.certificate, Some(Certificate::Trace { .. })));
        assert!(r_bug.witness.is_some());

        // 3. Constrained model where constraint blocks incrementing.
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
        let r_con = run_btor2("constrained", constrained);
        assert_eq!(r_con.verdict, Verdict::Safe);
        assert!(matches!(
            r_con.certificate,
            Some(Certificate::Invariant { .. })
        ));
    }
}
