//! Modular next-generation IC3 engines for InFormal:
//!
//! - [`Ic3InnEngine`] (`informal-ic3-inn`): IC3 with internal-signal shadow
//!   latch promotion + DynAMic/EXCTG/coupled generalization, projecting learned
//!   invariants back to concrete latches for independent certification.
//! - [`GlaEngine`] (`informal-gla`): Gate-Level Abstraction with UNSAT-core
//!   CEGAR localization over DynAMic/EXCTG IC3.

pub mod gla;
pub mod ic3_inn;

use gla::{run_gla, GlaOutcome};
use ic3_inn::{run_ic3_inn, InnOutcome};
use informal_bmc::{
    certify_lemmas, fold_on, prepare_ctx_fold, result, unknown, unsafe_certified_at, FORMATS,
};
use informal_engine::{
    Capabilities, CheckRequest, Effort, Engine, EngineDomain, EngineKind, EngineResult, RunContext,
    Verdict,
};
use std::sync::Arc;
use std::time::Instant;

/// IC3 with internal-node (AND-gate) generalization and DynAMic/EXCTG.
pub struct Ic3InnEngine;

impl Engine for Ic3InnEngine {
    fn name(&self) -> &str {
        "informal-ic3-inn"
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
            kind: EngineKind::Ic3,
            min_effort: Effort::Standard,
            certifies: true,
            ..Default::default()
        }
    }

    fn shed_rank(&self) -> u32 {
        2
    }

    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let g = match prepare_ctx_fold(req, ctx, fold_on()) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(e), t0)),
        };
        match run_ic3_inn(&g, ctx) {
            InnOutcome::Safe(lemmas) => {
                let checked = certify_lemmas(req, &g, &lemmas, ctx);
                Ok(result(self.name(), checked, t0))
            }
            InnOutcome::Unsafe { depth } => {
                let checked = unsafe_certified_at(
                    req,
                    Verdict::Unsafe { depth: Some(depth) },
                    ctx,
                    None,
                    g.folded.is_some(),
                );
                Ok(result(self.name(), checked, t0))
            }
            InnOutcome::Aborted(reason) => Ok(result(self.name(), unknown(reason), t0)),
        }
    }
}

/// Gate-Level Abstraction + UNSAT-core CEGAR engine (`informal-gla`).
pub struct GlaEngine;

impl Engine for GlaEngine {
    fn name(&self) -> &str {
        "informal-gla"
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
            kind: EngineKind::Hybrid,
            min_effort: Effort::Standard,
            certifies: true,
            ..Default::default()
        }
    }

    fn shed_rank(&self) -> u32 {
        2
    }

    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let g = match prepare_ctx_fold(req, ctx, fold_on()) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(e), t0)),
        };
        let max_depth = req.max_depth.map(|d| d as usize);
        match run_gla(&g, max_depth, ctx) {
            GlaOutcome::Safe(lemmas) => {
                let checked = certify_lemmas(req, &g, &lemmas, ctx);
                Ok(result(self.name(), checked, t0))
            }
            GlaOutcome::Unsafe {
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
            GlaOutcome::Aborted(reason) => Ok(result(self.name(), unknown(reason), t0)),
        }
    }
}

/// Next-generation IC3 engine fleet (`informal-ic3-inn`, `informal-gla`).
pub fn engines() -> Vec<Arc<dyn Engine>> {
    if informal_sat::Cadical::signature().is_none() {
        return Vec::new();
    }
    vec![Arc::new(Ic3InnEngine), Arc::new(GlaEngine)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use informal_engine::{Certificate, ModelFormat};
    use std::time::Duration;

    fn run_both(tag: &str, text: &str) -> (EngineResult, EngineResult) {
        let dir = std::env::temp_dir().join(format!("informal-ic3-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.btor2");
        std::fs::write(&path, text).unwrap();
        let req = CheckRequest::new(path, ModelFormat::Btor2, Duration::from_secs(15));
        let r_inn = Ic3InnEngine
            .check(&req, &RunContext::new(req.timeout))
            .unwrap();
        let r_gla = GlaEngine
            .check(&req, &RunContext::new(req.timeout))
            .unwrap();
        (r_inn, r_gla)
    }

    #[test]
    fn gla_localizes_away_32bit_bystander_and_ic3_inn_proves_comparator_invariants() {
        if informal_sat::Cadical::signature().is_none() {
            return;
        }
        // 1. Safe model with a 32-bit unrelated counter + a 2-bit safe control FSM
        //    coupled through a guarded mux so COI keeps the 32-bit counter, while
        //    GLA abstracts the 32-bit counter out immediately!
        let gla_bystander = "\
1 sort bitvec 32
2 sort bitvec 2
3 sort bitvec 1
4 zero 1
5 one 1
6 state 1 big_cnt
7 init 1 6 4
8 add 1 6 5
9 next 1 6 8
10 zero 2
11 one 2
12 state 2 fsm
13 init 2 12 10
14 constd 2 2
15 eq 3 12 14
16 add 2 12 11
17 ite 2 15 10 16
18 next 2 12 17
19 constd 2 3
20 eq 3 12 19
21 slice 3 6 31 31
22 and 3 20 21
23 bad 22
";
        let (r_inn, r_gla) = run_both("bystander", gla_bystander);
        assert_eq!(r_inn.verdict, Verdict::Safe);
        assert_eq!(r_gla.verdict, Verdict::Safe);
        assert!(matches!(
            r_inn.certificate,
            Some(Certificate::Invariant { .. } | Certificate::TwoStep { .. })
        ));
        assert!(matches!(
            r_gla.certificate,
            Some(Certificate::Invariant { .. } | Certificate::TwoStep { .. })
        ));

        // 2. Buggy model requiring CEGAR refinement across two coupled registers.
        let refine_bug = "\
1 sort bitvec 4
2 sort bitvec 1
3 zero 1
4 one 1
5 state 1 a
6 init 1 5 3
7 add 1 5 4
8 next 1 5 7
9 state 1 b
10 init 1 9 3
11 next 1 9 5
12 constd 1 5
13 eq 2 9 12
14 bad 13
";
        let (r_inn, r_gla) = run_both("refine-bug", refine_bug);
        assert_eq!(r_inn.verdict, Verdict::Unsafe { depth: Some(6) });
        assert_eq!(r_gla.verdict, Verdict::Unsafe { depth: Some(6) });
        assert!(matches!(r_gla.certificate, Some(Certificate::Trace { .. })));
    }
}
