/// Sanitizes a miter tag into a safe filename stem matching `crate::trace::file_name`.
pub fn sanitize_tag(tag: &str) -> String {
    let safe: String = tag
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_-.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = safe.trim_start_matches('.');
    if trimmed.is_empty() {
        "miter".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Writes deterministic `<workdir>/<safe_tag>.btor2` and `<safe_tag>.trace.btor2`
/// and replays the counterexample into `<workdir>/<safe_tag>.vcd` via `Session::write_trace`
/// (automatically falling back to BMC if `witness` is `None` or fails to replay).
/// Cleans up `.btor2` / `.trace.btor2` on `Err(Some(why))`.
pub fn replay_miter_cex(
    s: &crate::session::Session,
    tag: &str,
    full_miter_btor: &str,
    witness: Option<&str>,
    depth: Option<u32>,
) -> Result<(String, usize), Option<String>> {
    let safe_tag = sanitize_tag(tag);
    if let Err(e) = std::fs::create_dir_all(&s.workdir) {
        s.log(&format!(
            "{safe_tag}: creating workdir {}: {e}",
            s.workdir.display()
        ));
        return Err(None);
    }
    let (engine_btor, _) = informal_svlang::split_trace(full_miter_btor);
    let btor_path = s.workdir.join(format!("{safe_tag}.btor2"));
    let trace_btor_path = s.workdir.join(format!("{safe_tag}.trace.btor2"));
    if let Err(e) = std::fs::write(&btor_path, engine_btor) {
        s.log(&format!("{safe_tag}: writing {}: {e}", btor_path.display()));
        return Err(None);
    }
    if let Err(e) = std::fs::write(&trace_btor_path, full_miter_btor) {
        s.log(&format!(
            "{safe_tag}: writing {}: {e}",
            trace_btor_path.display()
        ));
        let _ = std::fs::remove_file(&btor_path);
        return Err(None);
    }
    match s.write_trace(&safe_tag, &btor_path, witness, depth) {
        Ok(res) => Ok(res),
        Err(Some(why)) => {
            let _ = std::fs::remove_file(&btor_path);
            let _ = std::fs::remove_file(&trace_btor_path);
            Err(Some(why))
        }
        Err(None) => Err(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::miter::{
        build_miter_btor2, parse_full_model, sanitize_btor_sym, CondDivergence, MiterSpec,
    };
    use informal_engine::{CheckRequest, Engine, ModelFormat, RunContext, Verdict};
    use informal_ir::Op;
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    static TEST_SEQ: AtomicUsize = AtomicUsize::new(0);

    fn temp_workdir(tag: &str) -> PathBuf {
        let n = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "informal-miter-test-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run_prove(btor: &str) -> (Verdict, Option<String>) {
        let dir = temp_workdir("prove");
        let path = dir.join("model.btor2");
        let (engine_btor, _) = informal_svlang::split_trace(btor);
        std::fs::write(&path, engine_btor).unwrap();
        let engines = informal_bmc::select_for_btor(&informal_bmc::engines(), engine_btor, false);
        let req = CheckRequest::new(path, ModelFormat::Btor2, Duration::from_secs(10));
        let run = informal_engine::portfolio::run_until(&engines, &req, None, None);
        let verdict = informal_engine::portfolio::verdict(&run);
        let witness = run.winner.and_then(|w| w.witness);
        let _ = std::fs::remove_dir_all(&dir);
        (verdict, witness)
    }

    fn run_bmc(btor: &str, max_depth: usize) -> (Verdict, Option<String>) {
        let dir = temp_workdir("bmc");
        let path = dir.join("model.btor2");
        let (engine_btor, _) = informal_svlang::split_trace(btor);
        std::fs::write(&path, engine_btor).unwrap();
        let bmc = informal_bmc::Bmc {
            max_depth: Some(max_depth),
            step: 1,
        };
        let req = CheckRequest::new(path, ModelFormat::Btor2, Duration::from_secs(10));
        let res = bmc.check(&req, &RunContext::new(req.timeout)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        (res.verdict, res.witness)
    }

    fn base_spec(target: (i64, u32, String)) -> MiterSpec {
        MiterSpec {
            diverging_inputs: HashSet::new(),
            diverging_nodes: HashMap::new(),
            conditional_diverging_nodes: Vec::new(),
            conditional_state_corruptions: Vec::new(),
            uninit_states_diverge: false,
            tied_uninit_states: HashSet::new(),
            skip_steps: 0,
            guard_ref: None,
            target,
            emit_trace_outputs: false,
        }
    }

    #[test]
    fn cond_diverge_combinational_and_negative_cond() {
        // Node 3 (8-bit `din`) is defined BEFORE node 6 (`pwr_on` 1-bit input),
        // and `corrupt_cond = -6` (!pwr_on) is a forward negative reference.
        let btor = "\
1 sort bitvec 1
2 sort bitvec 8
3 input 2 din
4 zero 2
5 add 2 3 4
6 input 1 pwr_on
7 output 5 obs
";
        let m = parse_full_model(btor).unwrap();
        let mut spec = base_spec((5, 8, "obs".into()));
        spec.conditional_diverging_nodes.push(CondDivergence {
            node_id: 5,
            corrupt_cond: -6,
            label: "pd off wire".into(),
        });
        let miter = build_miter_btor2(&m, &spec).unwrap();
        let parsed = parse_full_model(&miter).unwrap();
        informal_aig::btor::from_btor2(&parsed).unwrap();

        // Fresh corruption inputs must keep the 8-bit sort of node 5, not 1-bit.
        let c_in = parsed
            .lines()
            .find(|l| l.op == Op::Input && l.symbol.as_deref() == Some("pd_off_wire__m0_corrupt"))
            .unwrap();
        assert_eq!(parsed.width(c_in.sort.unwrap()), Some(8));

        // Semicolons in `CondDivergence::label` or `target_sym` must be sanitized so they cannot
        // start a Btor2 comment.
        assert_eq!(sanitize_btor_sym("pd;semi"), "pd_semi");
        let mut semi_spec = base_spec((5, 8, "obs;semi".into()));
        semi_spec.conditional_diverging_nodes.push(CondDivergence {
            node_id: 5,
            corrupt_cond: -6,
            label: "pd;semi".into(),
        });
        let semi_miter = build_miter_btor2(&m, &semi_spec).unwrap();
        let semi_parsed = parse_full_model(&semi_miter).unwrap();
        assert!(semi_parsed
            .lines()
            .any(|l| l.op == Op::Input && l.symbol.as_deref() == Some("pd_semi__m0_corrupt")));
        assert!(semi_parsed
            .lines()
            .any(|l| l.op == Op::Bad && l.symbol.as_deref() == Some("obs_semi")));

        // When pwr_on is unconstrained (can be 0 -> -6 is 1), solver must find CEX.
        let (v, _) = run_bmc(&miter, 4);
        assert!(
            matches!(v, Verdict::Unsafe { .. }),
            "expected CEX, got {v:?}"
        );

        // Constrain pwr_on == 1 (so corrupt_cond = -6 is always 0): must prove Safe!
        let btor_safe = format!("{btor}8 constraint 6\n");
        let m_safe = parse_full_model(&btor_safe).unwrap();
        let miter_safe = build_miter_btor2(&m_safe, &spec).unwrap();
        let (v_safe, _) = run_prove(&miter_safe);
        assert_eq!(v_safe, Verdict::Safe);
    }

    #[test]
    fn cond_diverge_state_q_obs_and_next_post_wakeup_leak() {
        // 8-bit register `r` init 0, holds value unless `write_en == 1` (`next = ite(write_en, din, r)`).
        // `sleep` corrupts `r` via `conditional_state_corruptions`.
        // `sleep_d1` tracks previous-cycle `sleep`; `obs` masks `r` while `sleep || sleep_d1`.
        // Post-wakeup (`sleep=0, sleep_d1=0`), if `write_en` wasn't asserted during `sleep_d1`,
        // `r` leaks its corrupted value!
        let btor_leak = "\
1 sort bitvec 1
2 sort bitvec 8
3 zero 2
4 input 1 write_en
5 input 2 din
6 state 2 r
7 init 2 6 3
8 ite 2 4 5 6
9 next 2 6 8
10 input 1 sleep
11 zero 1
12 state 1 sleep_d1
13 init 1 12 11
14 next 1 12 10
15 or 1 10 12
16 ite 2 15 3 6
17 output 16 obs
";
        let m = parse_full_model(btor_leak).unwrap();
        let mut spec = base_spec((16, 8, "obs".into()));
        spec.conditional_state_corruptions.push(CondDivergence {
            node_id: 6,
            corrupt_cond: 10,
            label: "r_ret".into(),
        });
        let miter = build_miter_btor2(&m, &spec).unwrap();
        let parsed = parse_full_model(&miter).unwrap();
        informal_aig::btor::from_btor2(&parsed).unwrap();

        // Cycle 0: sleep=1 corrupts next(r); Cycle 1: sleep=0, sleep_d1=1, write_en=0;
        // Cycle 2: sleep=0, sleep_d1=0 -> obs leaks corrupted r!
        let (v, _) = run_bmc(&miter, 4);
        assert!(
            matches!(v, Verdict::Unsafe { depth: Some(d) } if d >= 2),
            "expected post-wakeup state corruption CEX at depth >= 2, got {v:?}"
        );

        // If `write_en` is forced 1 whenever `sleep || sleep_d1` (`implies(sleep || sleep_d1, write_en)`),
        // wakeup always reloads `r` with `din` before `obs` unmasks, so `obs` is proven Safe!
        let btor_clean = format!("{btor_leak}18 implies 1 15 4\n19 constraint 18\n");
        let m_clean = parse_full_model(&btor_clean).unwrap();
        let miter_clean = build_miter_btor2(&m_clean, &spec).unwrap();
        let (v_clean, _) = run_prove(&miter_clean);
        assert_eq!(v_clean, Verdict::Safe);
    }

    #[test]
    fn cond_diverge_topological_ordering_and_self_driving_pmu() {
        // Tests:
        // 1. `Init::Expr` referencing a later-defined expression (`init 6 11` where 11 is defined after 6).
        // 2. Chained CondDivergences (`node 7` -> `node 9` whose `corrupt_cond` depends on `node 7`).
        // 3. Self-driving PMU cycle: `state 6` has `corrupt_cond` depending on `state 6` (`node 12`),
        //    and `node 8` has `corrupt_cond` depending on `node 8` (`node 13`), feeding downstream `node 14`.
        let btor = "\
1 sort bitvec 1
2 sort bitvec 4
3 input 1 en
4 input 2 din
5 zero 2
6 state 1 pmu_fsm
7 and 1 3 6
8 add 2 4 5
9 slice 1 8 0 0
10 one 1
11 not 1 3
12 eq 1 6 10
13 redor 1 8
14 and 1 9 13
15 init 1 6 11
16 next 1 6 7
17 output 14 z
";
        let m = parse_full_model(btor).unwrap();
        let mut spec = base_spec((14, 1, "z".into()));
        // Self-driving state corruption on `pmu_fsm` (6) via `node 12` (which depends on 6).
        spec.conditional_state_corruptions.push(CondDivergence {
            node_id: 6,
            corrupt_cond: 12,
            label: "pmu_self".into(),
        });
        // Self-driving combinational corruption on `node 8` via `node 13` (which depends on 8).
        spec.conditional_diverging_nodes.push(CondDivergence {
            node_id: 8,
            corrupt_cond: 13,
            label: "comb_self".into(),
        });
        // Chained CondDivergence on `node 9` whose condition depends on `node 7` (and thus `pmu_fsm`).
        spec.conditional_diverging_nodes.push(CondDivergence {
            node_id: 9,
            corrupt_cond: 7,
            label: "chained".into(),
        });

        let miter = build_miter_btor2(&m, &spec).unwrap();
        let parsed = parse_full_model(&miter).unwrap();
        // Must bit-blast cleanly with zero "used before it is defined" or combinational cycle errors.
        informal_aig::btor::from_btor2(&parsed).unwrap();

        // Validation error checks.
        let mut bad_zero = base_spec((14, 1, "z".into()));
        bad_zero.conditional_diverging_nodes.push(CondDivergence {
            node_id: 8,
            corrupt_cond: 0,
            label: "bad".into(),
        });
        assert!(build_miter_btor2(&m, &bad_zero).is_err());

        let mut bad_width = base_spec((14, 1, "z".into()));
        bad_width.conditional_diverging_nodes.push(CondDivergence {
            node_id: 9,
            corrupt_cond: 4, // 4-bit, not 1-bit
            label: "bad_w".into(),
        });
        assert!(build_miter_btor2(&m, &bad_width).is_err());

        let mut bad_state = base_spec((14, 1, "z".into()));
        bad_state
            .conditional_state_corruptions
            .push(CondDivergence {
                node_id: 8, // Op::Add, not Op::State
                corrupt_cond: 3,
                label: "bad_st".into(),
            });
        assert!(build_miter_btor2(&m, &bad_state).is_err());
    }

    #[test]
    fn emit_trace_outputs_and_replay_miter_cex_vcd_and_bmc_fallback() {
        let btor = "\
1 sort bitvec 1
2 sort bitvec 8
3 input 1 clk
4 input 1 pwr_off
5 input 2 din
6 state 2 q
7 zero 2
8 init 2 6 7
9 next 2 6 5
10 ite 2 4 7 6
11 output 6 q_out
12 output 4 informal_helper
";
        let m = parse_full_model(btor).unwrap();
        let mut spec = base_spec((6, 8, "leaky_q".into()));
        spec.conditional_state_corruptions.push(CondDivergence {
            node_id: 6,
            corrupt_cond: 4,
            label: "q_pd".into(),
        });
        spec.emit_trace_outputs = true;

        let full_miter = build_miter_btor2(&m, &spec).unwrap();
        assert!(full_miter.contains(informal_svlang::TRACE_MARKER));
        let (engine_btor, trace_sec) = informal_svlang::split_trace(&full_miter);
        assert!(!engine_btor.contains("output "));
        assert!(trace_sec.contains("m0.q_out"));
        assert!(trace_sec.contains("m1.q_out"));
        assert!(trace_sec.contains("m0.din"));
        assert!(trace_sec.contains("m1.din"));
        assert!(trace_sec.contains("m0.leaky_q"));
        assert!(trace_sec.contains("m1.leaky_q"));
        assert!(!trace_sec.contains("informal_helper"));

        // Verify full miter parses cleanly.
        let full_parsed = parse_full_model(&full_miter).unwrap();
        informal_aig::btor::from_btor2(&full_parsed).unwrap();

        let (verdict, witness) = run_bmc(&full_miter, 4);
        let depth = match verdict {
            Verdict::Unsafe { depth } => depth,
            other => panic!("expected Unsafe, got {other:?}"),
        };
        assert!(witness.is_some());

        let work = temp_workdir("replay");
        let eng = informal_bmc::engines();
        let mut sess = crate::session::Session::new(eng, work.clone());
        sess.clocks.push(crate::session::Clock {
            signal: "clk".into(),
            edge: "posedge",
            factor: 1,
            phase: 0,
        });

        // 1. Direct witness replay with tag sanitization.
        let raw_tag = "xprop_dut.leaky[0]";
        let safe_tag = sanitize_tag(raw_tag);
        assert_eq!(safe_tag, "xprop_dut.leaky_0_");
        let (vcd_direct, d_direct) =
            replay_miter_cex(&sess, raw_tag, &full_miter, witness.as_deref(), depth).unwrap();
        assert!(work.join(format!("{safe_tag}.btor2")).is_file());
        assert!(work.join(format!("{safe_tag}.trace.btor2")).is_file());
        assert!(std::path::Path::new(&vcd_direct).is_file());
        let vcd_text = std::fs::read_to_string(&vcd_direct).unwrap();
        assert!(vcd_text.contains("$scope module m0 $end"));
        assert!(vcd_text.contains("$scope module m1 $end"));
        assert!(d_direct <= 4);

        // 2. `witness = None` automatic BMC fallback.
        let before_reruns = sess.trace_reruns.load(Ordering::SeqCst);
        let (vcd_fallback, _) =
            replay_miter_cex(&sess, "fallback_tag", &full_miter, None, depth).unwrap();
        assert_eq!(sess.trace_reruns.load(Ordering::SeqCst), before_reruns + 1);
        assert!(std::path::Path::new(&vcd_fallback).is_file());
        assert!(work.join("fallback_tag.trace.btor2").is_file());

        // 3. Unreproducible model cleans up `.btor2` and `.trace.btor2` on `Err(Some(_))`.
        let safe_btor = "\
1 sort bitvec 1
2 zero 1
3 bad 2 always_safe
";
        let err = replay_miter_cex(&sess, "cleanup_tag", safe_btor, None, Some(2));
        assert!(matches!(err, Err(Some(_))));
        assert!(!work.join("cleanup_tag.btor2").exists());
        assert!(!work.join("cleanup_tag.trace.btor2").exists());

        let _ = std::fs::remove_dir_all(&work);
    }
}
