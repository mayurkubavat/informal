//! Formal Low-Power Verification (Stage 5, WS4) integration tests.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use informal_fpv::lp::{check_lp, load_upf, CheckLpOpts, LpStage, LpStatus};
use informal_fpv::sec::{setup_sec, SecSetup, SecStatus};
use informal_fpv::session::{Clock, Kind, Session, Status};

static SEQ: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn new_temp(tag: &str) -> TempDir {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!(
        "informal_lp_formal_{tag}_{}_{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    TempDir(p)
}

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/lp_e2e/fixtures")
        .join(rel)
}

fn make_session(dir: &TempDir, sv: PathBuf, top: &str) -> Session {
    let mut s = Session::new(informal_bmc::engines(), dir.0.join("work"));
    s.design.files.push(sv);
    s.elaborate(Some(top.to_string())).expect("elaborate");
    s.clocks.push(Clock {
        signal: "clk".into(),
        edge: "posedge",
        factor: 1,
        phase: 0,
    });
    s.reset = Some("!rst_n".into());
    s
}

#[test]
fn formal_aag_clean_and_pmu_bug_caught() {
    let dir_ok = new_temp("aag_ok");
    let mut s_ok = make_session(
        &dir_ok,
        fixture("f7_f10/f7/soc_lp_clean.sv"),
        "soc_lp_clean",
    );
    load_upf(
        &mut s_ok,
        &fixture("f7_f10/f7/soc_lp_clean.upf"),
        None,
        None,
        false,
    )
    .unwrap();
    check_lp(
        &mut s_ok,
        &CheckLpOpts {
            stage: LpStage::Formal,
            ..Default::default()
        },
    )
    .unwrap();
    let lp_asserts: Vec<_> = s_ok
        .props
        .iter()
        .filter(|p| p.task == "LowPower" && p.kind == Kind::Assert)
        .collect();
    assert!(lp_asserts.len() >= 6, "{lp_asserts:#?}");
    assert!(
        lp_asserts
            .iter()
            .all(|p| matches!(p.status, Status::Proven { .. })),
        "{lp_asserts:#?}"
    );
    assert!(lp_asserts
        .iter()
        .any(|p| p.name.contains("lp_ret_value_preserved_") && p.name.contains("cfg_q")));

    let dir_bug = new_temp("aag_bug");
    let mut s_bug = make_session(
        &dir_bug,
        fixture("f7_f10/f7/bug_iso_before_pwroff.sv"),
        "bug_iso_before_pwroff",
    );
    load_upf(
        &mut s_bug,
        &fixture("f7_f10/f7/bug_iso_before_pwroff.upf"),
        None,
        None,
        false,
    )
    .unwrap();
    check_lp(
        &mut s_bug,
        &CheckLpOpts {
            stage: LpStage::Formal,
            ..Default::default()
        },
    )
    .unwrap();
    let p_iso = s_bug
        .props
        .iter()
        .find(|p| p.name.contains("lp_iso_before_pwroff_"))
        .expect("lp_iso_before_pwroff");
    assert!(matches!(p_iso.status, Status::Cex { .. }), "{p_iso:#?}");
    let vcd = std::fs::read_to_string(p_iso.trace.as_ref().expect("trace")).unwrap();
    assert!(
        vcd.contains("$scope module lp $end"),
        "expected lp.pd.* swimlanes"
    );
}

#[test]
fn formal_auto_waiver_masked_vs_circular_and_invalidation() {
    let dir_m = new_temp("waive_masked");
    let mut s_m = make_session(
        &dir_m,
        fixture("f7_f10/f8/masked_crossing.sv"),
        "masked_crossing",
    );
    load_upf(
        &mut s_m,
        &fixture("f7_f10/f8/masked_crossing.upf"),
        None,
        None,
        false,
    )
    .unwrap();
    check_lp(
        &mut s_m,
        &CheckLpOpts {
            stage: LpStage::Rtl,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(s_m
        .lp
        .violations
        .iter()
        .any(|v| v.rule_id == "ISO_MISSING" && v.status == LpStatus::Open));
    check_lp(
        &mut s_m,
        &CheckLpOpts {
            prove_waivers: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(s_m
        .lp
        .violations
        .iter()
        .all(|v| v.status == LpStatus::FormallyWaived));
    assert!(s_m
        .lp
        .violations
        .iter()
        .all(|v| v.auto_formal_meta.is_some()));
    s_m.lp.invalidate_formal_waivers();
    assert!(s_m
        .lp
        .violations
        .iter()
        .all(|v| v.status == LpStatus::Open && v.auto_formal_meta.is_none()));

    let dir_c = new_temp("waive_circ");
    let mut s_c = make_session(
        &dir_c,
        fixture("f7_f10/f8/bug_circular_mask.sv"),
        "bug_circular_mask",
    );
    load_upf(
        &mut s_c,
        &fixture("f7_f10/f8/bug_circular_mask.upf"),
        None,
        None,
        false,
    )
    .unwrap();
    check_lp(
        &mut s_c,
        &CheckLpOpts {
            stage: LpStage::Rtl,
            ..Default::default()
        },
    )
    .unwrap();
    check_lp(
        &mut s_c,
        &CheckLpOpts {
            prove_waivers: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        !s_c.lp
            .violations
            .iter()
            .any(|v| v.status == LpStatus::FormallyWaived),
        "circular masking must never be auto-waived: {:#?}",
        s_c.lp.violations
    );
    assert!(
        s_c.lp
            .violations
            .iter()
            .filter(|v| v.status == LpStatus::FormalCex)
            .count()
            >= 2
    );
}

#[test]
fn formal_lp_sec_clean_and_missing_retention_cex() {
    let dir_ok = new_temp("sec_ok");
    let mut s_ok = make_session(&dir_ok, fixture("f7_f10/f8/pa_soc.sv"), "pa_soc");
    load_upf(
        &mut s_ok,
        &fixture("f7_f10/f8/pa_soc.upf"),
        None,
        None,
        false,
    )
    .unwrap();
    setup_sec(
        &mut s_ok,
        SecSetup {
            spec_files: vec![fixture("f7_f10/f8/golden_soc.sv")],
            spec_top: Some("golden_soc".into()),
            imp_files: vec![fixture("f7_f10/f8/pa_soc.sv")],
            imp_top: Some("pa_soc".into()),
            clock: Some("clk".into()),
            reset: Some("!rst_n".into()),
            ..Default::default()
        },
    )
    .unwrap();
    check_lp(
        &mut s_ok,
        &CheckLpOpts {
            sec: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!s_ok.sec.checks.is_empty());
    assert!(
        s_ok.sec
            .checks
            .iter()
            .all(|c| c.status == SecStatus::Proven),
        "{:#?}",
        s_ok.sec.checks
    );

    let dir_bug = new_temp("sec_bug");
    let mut s_bug = make_session(
        &dir_bug,
        fixture("f7_f10/f8/bug_pa_sec_ret.sv"),
        "bug_pa_sec_ret",
    );
    load_upf(
        &mut s_bug,
        &fixture("f7_f10/f8/bug_pa_sec_ret.upf"),
        None,
        None,
        false,
    )
    .unwrap();
    setup_sec(
        &mut s_bug,
        SecSetup {
            spec_files: vec![fixture("f7_f10/f8/golden_soc.sv")],
            spec_top: Some("golden_soc".into()),
            imp_files: vec![fixture("f7_f10/f8/bug_pa_sec_ret.sv")],
            imp_top: Some("bug_pa_sec_ret".into()),
            clock: Some("clk".into()),
            reset: Some("!rst_n".into()),
            ..Default::default()
        },
    )
    .unwrap();
    check_lp(
        &mut s_bug,
        &CheckLpOpts {
            sec: true,
            ..Default::default()
        },
    )
    .unwrap();
    let dout_chk = s_bug
        .sec
        .checks
        .iter()
        .find(|c| c.name == "sec_out_dout")
        .expect("sec_out_dout");
    assert!(matches!(dout_chk.status, SecStatus::Cex { .. }));
}
