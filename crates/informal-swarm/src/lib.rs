//! Simulation-guided swarm bug hunter (`informal-swarm`).
//!
//! Pairs 64-lane bit-parallel stratified simulation with novelty-guided
//! frontier seeding and short-horizon SAT bridging to find deep counterexamples
//! and narrow multi-bit triggers without full BMC unrolling. Never claims
//! `Safe`; every `Unsafe` hit is independently replayed and certified via
//! [`informal_bmc::unsafe_certified`].

pub mod swarm;

use informal_bmc::{prepare_ctx, result, unknown, unsafe_certified, FORMATS};
use informal_engine::{
    Capabilities, CheckRequest, Effort, Engine, EngineDomain, EngineKind, EngineResult, RunContext,
    Verdict,
};
use std::sync::Arc;
use std::time::Instant;
use swarm::{run_swarm, SwarmOutcome};

/// Bit-parallel simulation-guided swarm bug-finding engine.
pub struct SwarmEngine;

impl Engine for SwarmEngine {
    fn name(&self) -> &str {
        "informal-swarm"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            formats: FORMATS.to_vec(),
            proves: false,
            finds_bugs: true,
            liveness: true,
            domain: EngineDomain::BitLevel,
            kind: EngineKind::Swarm,
            min_effort: Effort::Standard,
            certifies: true,
            ..Default::default()
        }
    }

    /// Shed swarm bug hunter early under memory pressure (never proves).
    fn shed_rank(&self) -> u32 {
        5
    }

    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let g = match prepare_ctx(req, ctx) {
            Ok(g) => g,
            Err(e) => return Ok(result(self.name(), unknown(e), t0)),
        };
        let max_depth = req.max_depth.map(|d| d as usize);
        match run_swarm(&g, max_depth, ctx) {
            SwarmOutcome::Unsafe {
                depth,
                trace,
                witness,
            } => {
                let checked = unsafe_certified(
                    req,
                    Verdict::Unsafe {
                        depth: Some(depth as u32),
                    },
                    ctx,
                    Some((&g, trace)),
                );
                let mut r = result(self.name(), checked, t0);
                if matches!(r.verdict, Verdict::Unsafe { .. }) {
                    r.witness = Some(witness);
                }
                Ok(r)
            }
            SwarmOutcome::Aborted(reason) => Ok(result(self.name(), unknown(reason), t0)),
        }
    }
}

/// The in-house simulation-guided swarm fleet.
pub fn engines() -> Vec<Arc<dyn Engine>> {
    if informal_sat::Cadical::signature().is_none() {
        return Vec::new();
    }
    vec![Arc::new(SwarmEngine)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use informal_engine::{Certificate, ModelFormat};
    use std::time::Duration;

    fn run_btor2(tag: &str, text: &str) -> EngineResult {
        let dir = std::env::temp_dir().join(format!("informal-swarm-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.btor2");
        std::fs::write(&path, text).unwrap();
        let req = CheckRequest::new(path, ModelFormat::Btor2, Duration::from_secs(15));
        SwarmEngine
            .check(&req, &RunContext::new(req.timeout))
            .unwrap()
    }

    #[test]
    fn swarm_cracks_deep_warmup_plus_16bit_magic_trigger_and_certifies_trace() {
        if informal_sat::Cadical::signature().is_none() {
            return;
        }
        // Deep 10-bit counter warmup (cnt == 327) AND 16-bit magic input key == 0xBEEF!
        // Pure random simulation has 1/65536 chance per step; swarm's 64-lane sim +
        // frontier SAT bridge cracks it immediately and certifies the trace!
        let deep_magic = "\
1 sort bitvec 10
2 sort bitvec 16
3 sort bitvec 1
4 zero 1
5 one 1
6 state 1 cnt
7 init 1 6 4
8 add 1 6 5
9 next 1 6 8
10 input 2 key
11 constd 1 327
12 constd 2 48879
13 eq 3 6 11
14 eq 3 10 12
15 and 3 13 14
16 bad 15
";
        let r = run_btor2("deep-magic", deep_magic);
        assert_eq!(r.verdict, Verdict::Unsafe { depth: Some(327) });
        assert!(matches!(r.certificate, Some(Certificate::Trace { .. })));
        assert!(r.witness.is_some());
    }
}
