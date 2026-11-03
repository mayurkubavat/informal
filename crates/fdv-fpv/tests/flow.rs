//! End-to-end: the counter's JasperGold and VC Formal scripts run through
//! the compatibility layer, Yosys and the engine portfolio. Skipped when
//! Yosys or rIC3 is not installed.

use fdv_fpv::{compat, Flavor, Session, Shell};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

fn shell(flavor: Flavor, tag: &str) -> Option<Shell> {
    let yosys = on_path("yosys") || Path::new("/tools/yosys/current/bin/yosys").is_file();
    if !yosys || !on_path("ric3") {
        eprintln!("skipping: needs yosys and ric3 on PATH");
        return None;
    }
    let mut engines = fdv_engines_ext::load(&root().join("config/engines.toml")).unwrap();
    engines.retain(|e| e.name().starts_with("ric3-"));
    let work = std::env::temp_dir().join(format!("fdv-fpv-test-{tag}-{}", std::process::id()));
    Some(Shell::new(Session::new(engines, work), flavor))
}

fn statuses(sh: &Shell) -> Vec<(String, &'static str)> {
    compat::summary(&sh.session.borrow())
}

#[test]
fn jaspergold_script() {
    let Some(sh) = shell(Flavor::Jg, "jg") else {
        return;
    };
    sh.source(&root().join("tests/fpv/counter/counter.jg.tcl"))
        .unwrap();
    let got = statuses(&sh);
    let want = [
        ("a_max", "proven"),
        ("a_step", "proven"),
        ("a_hold", "proven"),
        ("a_never9", "cex"),
        ("a_no_wrap", "proven"),
        ("c_done", "covered"),
        ("c_10", "unreachable"),
    ];
    assert_eq!(got, want.map(|(n, s)| (n.to_string(), s)));
    let proven = sh
        .eval("get_property_list -include {type assert status proven}")
        .unwrap();
    assert_eq!(proven, "a_max a_step a_hold a_no_wrap");
}

#[test]
fn vc_formal_script() {
    let Some(sh) = shell(Flavor::Vcf, "vcf") else {
        return;
    };
    sh.source(&root().join("tests/fpv/counter/counter.vcf.tcl"))
        .unwrap();
    let got = statuses(&sh);
    let want = [
        ("a_max", "proven"),
        ("a_step", "proven"),
        ("a_never9", "cex"),
        ("c_done", "covered"),
        ("c_10", "unreachable"),
    ];
    assert_eq!(got, want.map(|(n, s)| (n.to_string(), s)));
}

#[test]
fn slang_rejects_typos_and_reports_source_errors() {
    if !fdv_slang::available() {
        eprintln!("skipping: built without slang");
        return;
    }
    let work = std::env::temp_dir().join(format!("fdv-fpv-test-slang-{}", std::process::id()));
    let sh = Shell::new(Session::new(Vec::new(), work.clone()), Flavor::Jg);
    let rtl = root().join("tests/fpv/counter/counter.sv");
    sh.eval(&format!("analyze -sv {}; elaborate", rtl.display()))
        .unwrap();
    assert_eq!(sh.session.borrow().design.top.as_deref(), Some("counter"));
    let e = sh.eval("assert -name t {q <= 4'd9 && qq}").unwrap_err();
    assert!(e.message.contains("qq"), "{}", e.message);
    // Parameters, `$` functions and sized literals are not names to check.
    sh.eval("assert -name ok {$past(q) != 4'hF |-> done || !done}")
        .unwrap();

    std::fs::create_dir_all(&work).unwrap();
    let bad = work.join("bad.sv");
    std::fs::write(
        &bad,
        "module t(input logic clk);\n  logic x;\n  assign x = clk + ;\nendmodule\n",
    )
    .unwrap();
    sh.eval("clear -all").unwrap();
    let e = sh
        .eval(&format!("analyze -sv {}; elaborate", bad.display()))
        .unwrap_err();
    assert!(e.message.contains("bad.sv:3:"), "{}", e.message);
}
