//! The environment check (docs/vacuity.md): assumptions that conflict make
//! every proof vacuous, and the shell says so. Skipped without the in-house
//! engines (CaDiCaL).

use informal_fpv::{Fairness, Flavor, Session, Shell, Vacuity};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A shell on `design` (under tests/fpv/vacuity), elaborated, with clock
/// and reset; `None` without the in-house engines.
fn shell(design: &str, top: &str) -> Option<Shell> {
    let engines = informal_bmc::engines();
    if engines.is_empty() {
        informal_engine::tool_missing("needs the in-house engines (CaDiCaL)");
        return None;
    }
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let id = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let work = std::env::temp_dir().join(format!(
        "informal-vacuity-{top}-{}-{id}",
        std::process::id()
    ));
    let sh = Shell::new(Session::new(engines, work), Flavor::Jg);
    let path = root().join("tests/fpv/vacuity").join(design);
    sh.eval(&format!("analyze -sv12 {{{}}}", path.display()))
        .unwrap();
    sh.eval(&format!(
        "elaborate -top {top}; clock clk; reset -expression {{rst}}; set_prove_time_limit 60s"
    ))
    .unwrap();
    Some(sh)
}

fn a_le(sh: &Shell, name: &str) -> (&'static str, Option<Vacuity>) {
    let s = sh.session.borrow();
    let p = s.props.iter().find(|p| p.name == name).unwrap();
    (p.status.word(), p.vacuity)
}

/// Assumptions in the RTL that conflict once the counter reaches 2.
#[test]
fn conflicting_rtl_assumptions_make_proofs_vacuous() {
    let Some(sh) = shell("conflict.sv", "conflict") else {
        return;
    };
    sh.eval("prove -all").unwrap();
    let env = sh.session.borrow().environment.clone().unwrap();
    assert!(env.conflict(), "{env:?}");
    assert_eq!(env.conflict_set, ["conflict.m_on", "conflict.m_stop"]);
    assert_eq!(
        a_le(&sh, "conflict.a_le"),
        ("proven", Some(Vacuity::VacuousEnvironment))
    );
    assert!(sh
        .eval("report")
        .unwrap()
        .contains("[vacuous (environment)]"));
    // The command on its own finds the same number of steps.
    let allowed: u32 = sh
        .eval("check_assumptions -depth 20")
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(Some(allowed), env.allowed);
}

/// The same environment from the script: switching one assumption off
/// clears the conflict, and the assertion then fails as it should.
#[test]
fn script_assumptions_and_switching_one_off() {
    let Some(sh) = shell("counter.sv", "counter") else {
        return;
    };
    sh.eval("assume -name m_on {en}; assume -name m_stop {cnt == 2 |-> !en}")
        .unwrap();
    sh.eval("prove -all").unwrap();
    assert_eq!(
        a_le(&sh, "counter.a_le"),
        ("proven", Some(Vacuity::VacuousEnvironment))
    );
    sh.eval("assume -disable m_stop").unwrap();
    sh.eval("prove -all").unwrap();
    assert!(!sh.session.borrow().environment.as_ref().unwrap().conflict());
    assert_eq!(a_le(&sh, "counter.a_le"), ("cex", None));
    // Off: no check, no field.
    sh.eval("informal::set_vacuity off; assume -enable m_stop; prove -all")
        .unwrap();
    assert!(sh.session.borrow().environment.is_none());
    assert_eq!(a_le(&sh, "counter.a_le"), ("proven", None));
}

fn vacuity(sh: &Shell, name: &str) -> Option<Vacuity> {
    a_le(sh, name).1
}

/// Precondition covers: a request that can come is checked, one an
/// assumption rules out is not, and a `disable iff` that is always true
/// leaves nothing to check.
#[test]
fn precondition_covers_after_proofs() {
    let Some(sh) = shell("request.sv", "request") else {
        return;
    };
    sh.eval("prove -all").unwrap();
    assert_eq!(
        a_le(&sh, "request.a_ack"),
        ("proven", Some(Vacuity::NonVacuous))
    );
    assert_eq!(
        a_le(&sh, "request.a_off"),
        ("proven", Some(Vacuity::Vacuous))
    );
    let report = sh.eval("report").unwrap();
    assert!(report.contains("[vacuous]"), "{report}");
    assert!(report.contains("[non-vacuous]"), "{report}");

    // An assumption that no request ever comes empties a_ack.
    sh.eval("assume -name m_quiet {!req}; prove -all").unwrap();
    assert_eq!(
        a_le(&sh, "request.a_ack"),
        ("proven", Some(Vacuity::Vacuous))
    );
    assert!(!sh.session.borrow().environment.as_ref().unwrap().conflict());

    // A cut point anywhere in the model qualifies a hit.
    sh.eval("assume -disable m_quiet; stopat spare; prove -all")
        .unwrap();
    assert_eq!(
        vacuity(&sh, "request.a_ack"),
        Some(Vacuity::NonVacuousCutPoints)
    );
}

/// `elaborate -no_preconditions` and `set_vacuity off` leave the covers
/// out; `-create_related_covers {precondition}` asks for them again.
#[test]
fn precondition_covers_switched_off() {
    let Some(sh) = shell("request.sv", "request") else {
        return;
    };
    sh.eval(
        "elaborate -top request -no_preconditions; clock clk; reset -expression {rst}; prove -all",
    )
    .unwrap();
    assert_eq!(vacuity(&sh, "request.a_off"), None);
    sh.eval("elaborate -top request -create_related_covers {precondition}; clock clk; reset -expression {rst}; prove -all")
        .unwrap();
    assert_eq!(vacuity(&sh, "request.a_off"), Some(Vacuity::Vacuous));
    sh.eval("informal::set_vacuity off; prove -all").unwrap();
    assert_eq!(vacuity(&sh, "request.a_off"), None);
    assert!(sh
        .eval("elaborate -top request -create_related_covers {nonsense}")
        .is_err());
}

/// The conflict narrowed: an assumption that takes no part in it is left
/// out of the set, and `check_assumptions -conflict` returns the set.
#[test]
fn conflict_set_leaves_out_bystanders() {
    let Some(sh) = shell("counter.sv", "counter") else {
        return;
    };
    sh.eval("assume -name m_low {rst |-> en}; assume -name m_on {en}; assume -name m_stop {cnt == 2 |-> !en}")
        .unwrap();
    assert_eq!(
        sh.eval("check_assumptions -depth 20 -conflict").unwrap(),
        "m_on m_stop"
    );
    // No conflict, no set.
    sh.eval("assume -disable m_stop").unwrap();
    assert_eq!(sh.eval("check_assumptions -conflict").unwrap(), "");
}

#[test]
fn immediate_precondition_covers_and_named_assumption_conflicts() {
    let engines = informal_bmc::engines();
    if engines.is_empty() {
        informal_engine::tool_missing("needs the in-house engines (CaDiCaL)");
        return;
    }
    let work = std::env::temp_dir().join(format!("informal-vacuity-imm-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let sv = work.join("imm_vac.sv");
    std::fs::write(
        &sv,
        "module imm_vac(input logic clk, rst, req, output logic gnt);\n\
           always_ff @(posedge clk) gnt <= rst ? 1'b0 : req;\n\
           always_comb begin\n\
             if (!rst && gnt) a_reachable: assert (gnt);\n\
             if (req && !req) a_dead: assert (gnt);\n\
             if (gnt && !gnt) a_err: $error(\"impossible\");\n\
           end\n\
         endmodule\n",
    )
    .unwrap();
    let sh = Shell::new(Session::new(engines.clone(), work.join("run1")), Flavor::Jg);
    sh.eval(&format!(
        "analyze -sv12 {{{}}}; elaborate -top imm_vac; clock clk; reset -expression {{rst}}; prove -all",
        sv.display()
    ))
    .unwrap();
    assert_eq!(
        a_le(&sh, "imm_vac.a_reachable"),
        ("proven", Some(Vacuity::NonVacuous))
    );
    assert_eq!(
        a_le(&sh, "imm_vac.a_dead"),
        ("proven", Some(Vacuity::Vacuous))
    );
    // `$error` reachability check (`expr == 1'b0`) must not be downgraded to vacuous when proven unreachable.
    assert_eq!(a_le(&sh, "imm_vac.a_err"), ("proven", None));

    // Unlabelled immediate `assume` statements carry hierarchical names in conflict cores.
    let sv_conf = work.join("imm_conf.sv");
    std::fs::write(
        &sv_conf,
        "module imm_conf(input logic clk, rst, a);\n\
           always @(posedge clk) begin\n\
             assume (a);\n\
             assume (!a);\n\
           end\n\
         endmodule\n",
    )
    .unwrap();
    let sh2 = Shell::new(Session::new(engines, work.join("run2")), Flavor::Jg);
    sh2.eval(&format!(
        "analyze -sv12 {{{}}}; elaborate -top imm_conf; clock clk; reset -expression {{rst}}",
        sv_conf.display()
    ))
    .unwrap();
    assert_eq!(
        sh2.eval("check_assumptions -depth 5 -conflict").unwrap(),
        "imm_conf._assume_1 imm_conf._assume_2"
    );
}

#[test]
fn witness_covers_run_when_requested_and_write_vcd() {
    let engines = informal_bmc::engines();
    if engines.is_empty() {
        informal_engine::tool_missing("needs the in-house engines (CaDiCaL)");
        return;
    }
    let work = std::env::temp_dir().join(format!("informal-wit-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let sv = work.join("wit.sv");
    std::fs::write(
        &sv,
        "module wit(input logic clk, rst, req, output logic gnt);\n\
           always_ff @(posedge clk) gnt <= rst ? 1'b0 : req;\n\
           a_pipe: assert property (@(posedge clk) disable iff (rst) req |=> gnt);\n\
         endmodule\n",
    )
    .unwrap();
    let run_dir = work.join("run");
    let sh = Shell::new(Session::new(engines, run_dir.clone()), Flavor::Jg);
    sh.eval(&format!(
        "analyze -sv12 {{{}}}; elaborate -top wit -create_related_covers {{precondition witness}}; clock clk; reset -expression {{rst}}; prove -all",
        sv.display()
    ))
    .unwrap();
    assert_eq!(
        a_le(&sh, "wit.a_pipe"),
        ("proven", Some(Vacuity::NonVacuous))
    );
    let sess: serde_json::Value = serde_json::from_str(&sh.eval("session").unwrap()).unwrap();
    let wtrace = sess["props"][0]["witness_trace"].as_str().expect(
        "witness_trace must be populated when -create_related_covers {witness} is requested",
    );
    assert!(run_dir.join(wtrace).is_file(), "missing VCD at {wtrace}");
}

#[test]
fn liveness_and_until_precondition_covers_detect_vacuity() {
    let engines = informal_bmc::engines();
    if engines.is_empty() {
        informal_engine::tool_missing("needs the in-house engines (CaDiCaL)");
        return;
    }
    let work = std::env::temp_dir().join(format!("informal-vac-live-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let sv = work.join("live_vac.sv");
    std::fs::write(
        &sv,
        "module live_vac(input logic clk, rst, req, gnt);\n\
           a_rtl_vac:  assert property (@(posedge clk) disable iff (rst) (req && !req) |-> s_eventually gnt);\n\
           a_rtl_live: assert property (@(posedge clk) disable iff (rst) req |-> s_eventually gnt);\n\
         endmodule\n",
    )
    .unwrap();
    let sh = Shell::new(Session::new(engines, work.join("run")), Flavor::Jg);
    sh.eval(&format!(
        "analyze -sv12 {{{}}}; elaborate -top live_vac; clock clk; reset -expression {{rst}};\n\
         assume -name m_fair {{s_eventually gnt}};\n\
         assert -name a_cmd_vac {{1'b0 |-> s_eventually gnt}};\n\
         assert -name a_cmd_live {{req |=> s_eventually gnt}};\n\
         prove -all",
        sv.display()
    ))
    .unwrap();
    assert_eq!(
        a_le(&sh, "live_vac.a_rtl_vac"),
        ("proven", Some(Vacuity::Vacuous))
    );
    assert_eq!(
        a_le(&sh, "live_vac.a_rtl_live"),
        ("proven", Some(Vacuity::NonVacuous))
    );
    assert_eq!(a_le(&sh, "a_cmd_vac"), ("proven", Some(Vacuity::Vacuous)));
    assert_eq!(
        a_le(&sh, "a_cmd_live"),
        ("proven", Some(Vacuity::NonVacuous))
    );
    let report = sh.eval("report").unwrap();
    assert!(report.contains("[vacuous]"), "{report}");
    assert!(report.contains("[non-vacuous]"), "{report}");
}

/// The verdict of `check_assumptions -live` through the session, after the
/// shell command has given its word.
fn fairness(sh: &Shell) -> Fairness {
    sh.session.borrow_mut().fairness_now(false).unwrap().verdict
}

/// A list result, sorted: the order of a conflicting set follows the
/// model's lines.
fn sorted(list: String) -> Vec<String> {
    let mut v: Vec<String> = list.split_whitespace().map(String::from).collect();
    v.sort();
    v
}

/// `check_assumptions -live`: a fairness assumption that no trace can meet
/// with the other assumptions makes every liveness proof vacuous, and
/// `-conflict` names the assumptions that cause it. When the constraints
/// end every trace on their own, the cause is told apart.
#[test]
fn contradictory_fairness_is_vacuous() {
    let Some(sh) = shell("fair.sv", "fair") else {
        return;
    };
    sh.eval("assume -name m_fair {s_eventually gnt}; assume -name m_never {!gnt}; assume -name m_idle {!req |=> !req}")
        .unwrap();
    assert_eq!(sh.eval("check_assumptions -live").unwrap(), "vacuous");
    assert_eq!(fairness(&sh), Fairness::Vacuous);
    // m_idle takes no part in the contradiction.
    assert_eq!(
        sorted(sh.eval("check_assumptions -live -conflict").unwrap()),
        ["m_fair", "m_never"]
    );

    // The same assumptions written in the RTL.
    let work = std::env::temp_dir().join(format!("informal-vac-fair-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let sv = work.join("fair_rtl.sv");
    std::fs::write(
        &sv,
        "module fair_rtl(input logic clk, rst, req, gnt);\n\
           m_fair: assume property (@(posedge clk) s_eventually gnt);\n\
           m_never: assume property (@(posedge clk) !gnt);\n\
           a_live: assert property (@(posedge clk) disable iff (rst) req |-> s_eventually gnt);\n\
         endmodule\n",
    )
    .unwrap();
    let rtl = Shell::new(
        Session::new(informal_bmc::engines(), work.join("run")),
        Flavor::Jg,
    );
    rtl.eval(&format!(
        "analyze -sv12 {{{}}}; elaborate -top fair_rtl; clock clk; reset -expression {{rst}}",
        sv.display()
    ))
    .unwrap();
    assert_eq!(rtl.eval("check_assumptions -live").unwrap(), "vacuous");
    assert_eq!(
        sorted(rtl.eval("check_assumptions -live -conflict").unwrap()),
        ["fair_rtl.m_fair", "fair_rtl.m_never"]
    );

    // Constraints that allow no trace past step 3, with a fairness
    // assumption on top that either of them alone lets hold: vacuous, but
    // not because of the fairness, and the set leaves it out.
    let Some(sh) = shell("counter.sv", "counter") else {
        return;
    };
    sh.eval("assume -name m_on {en}; assume -name m_stop {cnt == 2 |-> !en}; assume -name m_fair {s_eventually cnt == 2}")
        .unwrap();
    assert_eq!(sh.eval("check_assumptions -live").unwrap(), "vacuous");
    assert_eq!(fairness(&sh), Fairness::VacuousConstraints);
    assert_eq!(
        sorted(sh.eval("check_assumptions -live -conflict").unwrap()),
        ["m_on", "m_stop"]
    );
}

/// A fairness assumption the environment can meet: the engines find a fair
/// lasso, so it is consistent and nothing conflicts.
#[test]
fn satisfiable_fairness_is_consistent() {
    let Some(sh) = shell("fair.sv", "fair") else {
        return;
    };
    sh.eval("assume -name m_fair {s_eventually gnt}; assume -name m_one {req |-> !gnt}")
        .unwrap();
    assert_eq!(sh.eval("check_assumptions -live").unwrap(), "consistent");
    assert_eq!(fairness(&sh), Fairness::Satisfiable);
    assert_eq!(sh.eval("check_assumptions -live -conflict").unwrap(), "");
    // Switching the contradicting assumption off makes it consistent again.
    sh.eval("assume -name m_never {!gnt}").unwrap();
    assert_eq!(sh.eval("check_assumptions -live").unwrap(), "vacuous");
    sh.eval("assume -disable m_never").unwrap();
    assert_eq!(sh.eval("check_assumptions -live").unwrap(), "consistent");
}

/// No fairness assumption (only safety ones, or none): nothing to check,
/// whatever the safety assumptions say.
#[test]
fn no_fairness_assumption_has_nothing_to_check() {
    let Some(sh) = shell("fair.sv", "fair") else {
        return;
    };
    assert_eq!(sh.eval("check_assumptions -live").unwrap(), "none");
    sh.eval("assume -name m_never {!gnt}").unwrap();
    assert_eq!(sh.eval("check_assumptions -live").unwrap(), "none");
    assert_eq!(fairness(&sh), Fairness::NoFairness);
    assert_eq!(sh.eval("check_assumptions -live -conflict").unwrap(), "");
    // A fairness assumption that is switched off does not count.
    sh.eval("assume -name m_fair {s_eventually gnt}; assume -disable m_fair")
        .unwrap();
    assert_eq!(sh.eval("check_assumptions -live").unwrap(), "none");
}

/// `-live` takes `-conflict` but not `-depth` or `-dead_end`, which bound
/// finite traces; and a search that cannot finish (bounded model checking
/// only, which never proves that no lasso exists) is undetermined, never
/// consistent.
#[test]
fn live_option_combinations_and_undetermined() {
    let Some(sh) = shell("fair.sv", "fair") else {
        return;
    };
    sh.eval("assume -name m_fair {s_eventually gnt}; assume -name m_never {!gnt}")
        .unwrap();
    for other in ["-depth 5", "-dead_end", "-dead_end -conflict"] {
        let e = sh
            .eval(&format!("check_assumptions -live {other}"))
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("-live") && e.contains(other.split(' ').next().unwrap()),
            "{e}"
        );
    }
    // The finite checks still run without -live.
    assert_eq!(sh.eval("check_assumptions -depth 5").unwrap(), "5");

    sh.eval("set_max_trace_length 10; informal::set_engines informal-bmc")
        .unwrap();
    assert_eq!(sh.eval("check_assumptions -live").unwrap(), "undetermined");
    assert!(matches!(fairness(&sh), Fairness::Unknown(_)));
    let e = sh
        .eval("check_assumptions -live -conflict")
        .unwrap_err()
        .to_string();
    assert!(e.contains("could not decide"), "{e}");
}
