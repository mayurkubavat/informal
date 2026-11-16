//! The environment check (docs/vacuity.md): assumptions that conflict make
//! every proof vacuous, and the shell says so. Skipped without the in-house
//! engines (CaDiCaL).

use informal_fpv::{Flavor, Session, Shell, Vacuity};
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
    let work = std::env::temp_dir().join(format!("informal-vacuity-{top}-{}", std::process::id()));
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
