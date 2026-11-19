//! End-to-end: the counter's JasperGold and VC Formal scripts run through
//! the compatibility layer, svlang and the engine portfolio. Skipped when
//! rIC3 is not installed.

use informal_fpv::{compat, Flavor, Session, Shell};
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
    if !on_path("ric3") {
        informal_engine::tool_missing("needs ric3 on PATH");
        return None;
    }
    let mut engines = informal_engines_ext::load(&root().join("config/engines.toml")).unwrap();
    engines.retain(|e| e.name().starts_with("ric3-"));
    let work = std::env::temp_dir().join(format!("informal-fpv-test-{tag}-{}", std::process::id()));
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
    // rIC3 prints its counterexample (--cex); converted to a named witness
    // it gives the traces without a BMC re-run.
    assert_eq!(
        sh.session
            .borrow()
            .trace_reruns
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    for p in &sh.session.borrow().props {
        let want = matches!(p.name.as_str(), "a_never9" | "c_done");
        assert_eq!(p.trace.is_some(), want, "{}: {:?}", p.name, p.trace);
        // The shortest trace is 10 cycles deep (ric3-bmc's); IC3's own
        // counterexample, now kept, may hold the enable low for a while.
        if let informal_fpv::Status::Cex { depth, .. }
        | informal_fpv::Status::Covered { depth, .. } = &p.status
        {
            assert!(depth.is_some_and(|d| d >= 10), "{}: {depth:?}", p.name);
        }
    }
    let proven = sh
        .eval("get_property_list -include {type assert status proven}")
        .unwrap();
    assert_eq!(proven, "a_max a_step a_hold a_no_wrap");
}

/// The front end's scope tree reaches the snapshot as `design.hierarchy`
/// and is written to the work directory.
#[test]
fn design_hierarchy_in_the_snapshot() {
    let work = std::env::temp_dir().join(format!("informal-fpv-test-hier-{}", std::process::id()));
    let sh = Shell::new(Session::new(Vec::new(), work.clone()), Flavor::Jg);
    let rtl = root().join("tests/fpv/fifo/fifo.sv");
    sh.eval(&format!(
        "analyze -sv12 {}; elaborate -top fifo",
        rtl.display()
    ))
    .unwrap();
    let json = serde_json::to_value(&*sh.session.borrow()).unwrap();
    let h = &json["design"]["hierarchy"];
    assert_eq!(h[0]["module"], "fifo");
    assert_eq!(h[0]["children"][1]["path"], "fifo.u_rp");
    assert_eq!(h[0]["children"][1]["module"], "ptr");
    let file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(work.join("design.hierarchy.json")).unwrap())
            .unwrap();
    assert_eq!(&file, h);
    // `clear -all` forgets it with the rest of the design.
    sh.eval("clear -all").unwrap();
    let json = serde_json::to_value(&*sh.session.borrow()).unwrap();
    assert_eq!(json["design"]["hierarchy"], serde_json::json!([]));
    let _ = std::fs::remove_dir_all(&work);
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
fn front_end_rejects_typos_and_reports_source_errors() {
    let work = std::env::temp_dir().join(format!("informal-fpv-test-names-{}", std::process::id()));
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

/// The FIFO: the known verdict for every property.
#[test]
fn fifo_known_answers() {
    let run = || {
        let sh = shell(Flavor::Jg, "fifo")?;
        sh.source(&root().join("tests/fpv/fifo/fifo.jg.tcl"))
            .unwrap();
        assert_eq!(sh.session.borrow().frontend, "svlang");
        Some(statuses(&sh))
    };
    let Some(v) = run() else { return };
    let want = [
        ("a_count_max", "proven"),
        ("a_full_empty", "proven"),
        ("a_ptr_diff", "proven"),
        ("a_no_overflow", "proven"),
        ("a_status", "proven"),
        ("a_data", "proven"),
        ("a_wrong", "cex"),
        ("c_full", "covered"),
        ("c_status3", "covered"),
        ("c_count5", "unreachable"),
    ];
    let mut want = want.to_vec();
    want.insert(7, ("a_sub_ptr", "proven"));
    let want: Vec<_> = want.iter().map(|(n, st)| (n.to_string(), *st)).collect();
    assert_eq!(v, want);
}

#[test]
fn param_overrides_jg_and_vcf() {
    let engines = informal_bmc::engines();
    if engines.is_empty() {
        informal_engine::tool_missing("needs CaDiCaL");
        return;
    }
    let work = std::env::temp_dir().join(format!("informal-fpv-params-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let rtl = work.join("param_cnt.sv");
    std::fs::write(
        &rtl,
        "module param_cnt #(parameter int MAX = 9) (\n\
           input  logic       clk,\n\
           input  logic       rst,\n\
           input  logic       en,\n\
           output logic [3:0] q\n\
         );\n\
           always_ff @(posedge clk)\n\
             if (rst) q <= 4'd0;\n\
             else if (en && q < MAX[3:0]) q <= q + 4'd1;\n\
         endmodule\n",
    )
    .unwrap();

    let jg = Shell::new(Session::new(engines.clone(), work.join("jg")), Flavor::Jg);
    jg.eval(&format!(
        "analyze -sv {{{}}}\n\
         elaborate -top param_cnt -param MAX 5\n\
         clock clk\n\
         reset -expression {{rst}}\n\
         assert -name a_bound {{q <= 4'd5}}\n\
         cover  -name c_five  {{q == 4'd5}}\n\
         cover  -name c_six   {{q == 4'd6}}\n\
         prove -all",
        rtl.display()
    ))
    .unwrap();
    assert_eq!(
        statuses(&jg),
        [
            ("a_bound".to_string(), "proven"),
            ("c_five".to_string(), "covered"),
            ("c_six".to_string(), "unreachable"),
        ]
    );

    let vcf = Shell::new(Session::new(engines, work.join("vcf")), Flavor::Vcf);
    vcf.eval(&format!(
        "analyze -format sverilog {{{}}}\n\
         elaborate param_cnt -parameters {{MAX=4}}\n\
         create_clock clk -period 10\n\
         create_reset rst -high\n\
         fvassert a_bound -expr {{q <= 4'd4}}\n\
         fvcover  c_four  -expr {{q == 4'd4}}\n\
         fvcover  c_five  -expr {{q == 4'd5}}\n\
         check_fv",
        rtl.display()
    ))
    .unwrap();
    assert_eq!(
        statuses(&vcf),
        [
            ("a_bound".to_string(), "proven"),
            ("c_four".to_string(), "covered"),
            ("c_five".to_string(), "unreachable"),
        ]
    );
    let _ = std::fs::remove_dir_all(&work);
}
