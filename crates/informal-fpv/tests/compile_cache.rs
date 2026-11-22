//! Integration tests for WS1A two-tier `CompiledDesign` caching across
//! `Session`, `compat`, `Worker`, Formal Apps, and `sec.rs`.

use informal_fpv::sec::SecStatus;
use informal_fpv::{Flavor, Session, Shell, Worker};
use informal_svlang::cache_stats;
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

static TEST_SEQ: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "informal-compile-cache-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn engines() -> Option<Vec<Arc<dyn informal_engine::Engine>>> {
    let e = informal_bmc::engines();
    if e.is_empty() {
        informal_engine::tool_missing("needs the in-house engines (CaDiCaL)");
        return None;
    }
    Some(e)
}

#[test]
fn session_cross_app_reuse_and_mutation_invalidations() {
    let Some(eng) = engines() else { return };
    let dir = temp_dir("session");
    let sv = dir.join("dut.sv");
    let src_w4 = "\
module dut #(parameter int W = 4) (
    input logic clk,
    input logic rst_n,
    input logic [W-1:0] a,
    output logic [W-1:0] q
);
    logic [W-1:0] mid;
    assign mid = a;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n)
            q <= '0;
        else
            q <= mid;
    end
endmodule
";
    std::fs::write(&sv, src_w4).unwrap();

    let sh = Shell::new(Session::new(eng.clone(), dir.join("work")), Flavor::Jg);

    // 1. `analyze` + `elaborate` (without `-top`, exercising inferred top + unused `-bbox_m` pruning)
    //    populates `Session::compiled` with exactly 1 Tier-1 compile.
    let s0 = cache_stats();
    sh.eval(&format!(
        "analyze -sv {{{}}} -bbox_m {{unused_stub}}; elaborate; clock clk; reset {{!rst_n}}",
        sv.display()
    ))
    .unwrap();
    let s1 = cache_stats();
    assert_eq!(s1.tier1_compiles - s0.tier1_compiles, 1);

    let c_elab = {
        let s = sh.session.borrow();
        assert_eq!(s.design.top.as_deref(), Some("dut"));
        Arc::clone(s.compiled.as_ref().expect("compiled should be populated"))
    };

    // 2. Repeated `prove`, `check_xprop`, `check_cov`, `get_fv_complexity`, and `stopat` / `task`
    //    all reuse `Session::compiled` with ZERO Tier-1 recompiles.
    sh.eval("assert -name a_id {q == q}").unwrap();
    sh.eval("prove -all").unwrap();
    sh.eval("check_xprop -init -prove").unwrap();
    sh.eval("check_cov -init -model {coi} -measure").unwrap();
    sh.eval("get_fv_complexity").unwrap();
    sh.eval("task -create t_cut; task -set t_cut; stopat mid; assert -name a_cut {q == q}; prove -task t_cut")
        .unwrap();
    assert_eq!(sh.eval("get_status t_cut::a_cut").unwrap(), "proven");
    sh.eval("task -set <embedded>").unwrap();
    let s2 = cache_stats();
    assert_eq!(
        s2.tier1_compiles - s1.tier1_compiles,
        0,
        "cross-app calls and Tier-2 stopat/task changes must not trigger Tier-1 recompiles"
    );
    {
        let s = sh.session.borrow();
        assert!(Arc::ptr_eq(s.compiled.as_ref().unwrap(), &c_elab));
    }

    // 3. Re-running `elaborate -top dut` with identical inputs hits Tier-1 (`Arc::ptr_eq`).
    sh.eval("elaborate -top dut").unwrap();
    let s3 = cache_stats();
    assert_eq!(s3.tier1_compiles - s2.tier1_compiles, 0);
    assert!(Arc::ptr_eq(
        sh.session.borrow().compiled.as_ref().unwrap(),
        &c_elab
    ));

    // 4. `elaborate -param W 8` misses Tier-1 and replaces `compiled`.
    sh.eval("elaborate -top dut -param W 8").unwrap();
    let s4 = cache_stats();
    assert_eq!(s4.tier1_compiles - s3.tier1_compiles, 1);
    let c_w8 = Arc::clone(sh.session.borrow().compiled.as_ref().unwrap());
    assert!(!Arc::ptr_eq(&c_elab, &c_w8));

    // 5. `set_lowering` invalidates `compiled` and `elaborated`.
    sh.eval("set_lowering isunknown_is_false on").unwrap();
    {
        let s = sh.session.borrow();
        assert!(s.compiled.is_none());
        assert!(!s.elaborated);
    }

    // 6. Re-elaborate, then `analyze -clear` invalidates `compiled` and `elaborated`.
    sh.eval("elaborate -top dut").unwrap();
    assert!(sh.session.borrow().compiled.is_some());
    sh.eval(&format!("analyze -clear -sv {{{}}}", sv.display()))
        .unwrap();
    {
        let s = sh.session.borrow();
        assert!(s.compiled.is_none());
        assert!(!s.elaborated);
    }

    // 7. Disk `.sv` edit (same length + preserved mtime) is caught by `Session::lower_opts`
    //    `is_fresh()` check rather than using stale RTL!
    let sv_same_len = dir.join("live.sv");
    let l1 = "module live(input logic clk, input logic d, output logic q); always_ff @(posedge clk) q <=  d; endmodule\n";
    let l2 = "module live(input logic clk, input logic d, output logic q); always_ff @(posedge clk) q <= ~d; endmodule\n";
    assert_eq!(l1.len(), l2.len());
    std::fs::write(&sv_same_len, l1).unwrap();
    let orig_mtime = std::fs::metadata(&sv_same_len).unwrap().modified().unwrap();

    sh.eval(&format!(
        "clear -all; analyze -sv {{{}}}; elaborate -top live; clock clk",
        sv_same_len.display()
    ))
    .unwrap();
    sh.eval("assert -name a_eq {q == $past(d)}").unwrap();
    sh.eval("prove -all").unwrap();
    assert_eq!(sh.eval("get_status a_eq").unwrap(), "proven");

    std::fs::write(&sv_same_len, l2).unwrap();
    File::options()
        .write(true)
        .open(&sv_same_len)
        .unwrap()
        .set_modified(orig_mtime)
        .unwrap();
    sh.eval("prove -all").unwrap();
    assert_eq!(
        sh.eval("get_status a_eq").unwrap(),
        "cex",
        "Session::lower_opts must detect stale FileDigest and re-lower updated RTL"
    );

    // 8. `clear` clears `compiled`.
    sh.eval("clear -all").unwrap();
    assert!(sh.session.borrow().compiled.is_none());

    // 9. VCF `read_file` populates `compiled` and `set_blackbox` invalidates it.
    let sh_vcf = Shell::new(Session::new(eng, dir.join("work_vcf")), Flavor::Vcf);
    sh_vcf
        .eval(&format!(
            "read_file -format sverilog {{{}}}",
            sv_same_len.display()
        ))
        .unwrap();
    assert!(sh_vcf.session.borrow().compiled.is_some());
    sh_vcf.eval("set_blackbox -designs dummy_sub").unwrap();
    assert!(sh_vcf.session.borrow().compiled.is_none());
}

#[test]
fn sec_reuses_compiled_designs_and_invalidates_per_side() {
    let Some(eng) = engines() else { return };
    let dir = temp_dir("sec");
    let spec_sv = dir.join("spec.sv");
    let imp_sv = dir.join("imp.sv");

    let spec_src = "\
module spec_top(input logic clk, input logic rst_n, input logic [3:0] a, output logic [3:0] y);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) y <= 4'd0;
        else        y <= a + 4'd1;
    end
endmodule
";
    let imp_good = "\
module imp_top(input logic clk, input logic rst_n, input logic [3:0] a, output logic [3:0] y);
    logic [3:0] inc;
    assign inc = a + 4'd1;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) y <= 4'd0;
        else        y <= inc;
    end
endmodule
";
    let imp_bad = "\
module imp_top(input logic clk, input logic rst_n, input logic [3:0] a, output logic [3:0] y);
    logic [3:0] inc;
    assign inc = a + 4'd2;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) y <= 4'd0;
        else        y <= inc;
    end
endmodule
";
    assert_eq!(imp_good.len(), imp_bad.len());
    std::fs::write(&spec_sv, spec_src).unwrap();
    std::fs::write(&imp_sv, imp_good).unwrap();
    let imp_mtime = std::fs::metadata(&imp_sv).unwrap().modified().unwrap();

    let sh = Shell::new(Session::new(eng, dir.join("work")), Flavor::Jg);

    // 1. `check_sec -setup` (auto-detecting tops) compiles each side once.
    let s0 = cache_stats();
    sh.eval(&format!(
        "check_sec -setup -spec {{{}}} -imp {{{}}} -clock clk -reset {{!rst_n}}",
        spec_sv.display(),
        imp_sv.display()
    ))
    .unwrap();
    let s1 = cache_stats();
    assert_eq!(s1.tier1_compiles - s0.tier1_compiles, 2);

    let (c_spec_0, c_imp_0) = {
        let s = sh.session.borrow();
        (
            Arc::clone(s.sec.spec_compiled.as_ref().expect("spec_compiled")),
            Arc::clone(s.sec.imp_compiled.as_ref().expect("imp_compiled")),
        )
    };

    // 2. `check_sec -prove` reuses both `CompiledDesign`s with 0 Tier-1 recompiles.
    sh.eval("check_sec -prove").unwrap();
    let s2 = cache_stats();
    assert_eq!(s2.tier1_compiles - s1.tier1_compiles, 0);
    {
        let s = sh.session.borrow();
        assert!(Arc::ptr_eq(
            s.sec.spec_compiled.as_ref().unwrap(),
            &c_spec_0
        ));
        assert!(Arc::ptr_eq(s.sec.imp_compiled.as_ref().unwrap(), &c_imp_0));
        assert!(s.sec.checks.iter().any(|c| c.status == SecStatus::Proven));
    }

    // 3. Re-running `check_sec -setup -lec ... -prove` still hits Tier-1 on both sides.
    sh.eval(&format!(
        "check_sec -setup -lec -spec {{{}}} -imp {{{}}} -clock clk -prove",
        spec_sv.display(),
        imp_sv.display()
    ))
    .unwrap();
    let s3 = cache_stats();
    assert_eq!(s3.tier1_compiles - s2.tier1_compiles, 0);
    {
        let s = sh.session.borrow();
        assert!(Arc::ptr_eq(
            s.sec.spec_compiled.as_ref().unwrap(),
            &c_spec_0
        ));
        assert!(Arc::ptr_eq(s.sec.imp_compiled.as_ref().unwrap(), &c_imp_0));
        assert!(s.sec.checks.iter().any(|c| c.status == SecStatus::Proven));
    }

    // 4. Editing ONLY `imp.sv` (same length + preserved mtime) recompiles ONLY `imp`.
    std::fs::write(&imp_sv, imp_bad).unwrap();
    File::options()
        .write(true)
        .open(&imp_sv)
        .unwrap()
        .set_modified(imp_mtime)
        .unwrap();
    sh.eval("check_sec -prove").unwrap();
    let s4 = cache_stats();
    assert_eq!(s4.tier1_compiles - s3.tier1_compiles, 1);
    {
        let s = sh.session.borrow();
        assert!(
            Arc::ptr_eq(s.sec.spec_compiled.as_ref().unwrap(), &c_spec_0),
            "unchanged spec side must keep Tier-1 cache hit"
        );
        assert!(
            !Arc::ptr_eq(s.sec.imp_compiled.as_ref().unwrap(), &c_imp_0),
            "edited imp side must recompile"
        );
        assert!(s
            .sec
            .checks
            .iter()
            .any(|c| matches!(c.status, SecStatus::Cex { .. })));
    }

    // 5. `check_sec -clear` drops both cached compilations and keeps `is_empty()` true.
    sh.eval("check_sec -clear").unwrap();
    {
        let s = sh.session.borrow();
        assert!(s.sec.spec_compiled.is_none());
        assert!(s.sec.imp_compiled.is_none());
        assert!(s.sec.is_empty());
    }
}

#[test]
fn worker_session_remains_send_and_uses_compiled_cache() {
    fn assert_send<T: Send + 'static>() {}
    assert_send::<Session>();

    let Some(eng) = engines() else { return };
    let dir = temp_dir("worker");
    let sv = dir.join("w.sv");
    std::fs::write(
        &sv,
        "module w(input logic clk, input logic d, output logic q); always_ff @(posedge clk) q <= d; endmodule\n",
    )
    .unwrap();

    let w = Worker::spawn(Flavor::Jg, move || Session::new(eng, dir.join("work")));
    let r1 = w.call(informal_fpv::Request::Eval(format!(
        "analyze -sv {{{}}}; elaborate; clock clk; assert -name p {{q == q}}; prove -all",
        sv.display()
    )));
    assert!(r1.ok, "{}", r1.result);
    let has_compiled = w.with(|sh| sh.session.borrow().compiled.is_some());
    assert_eq!(has_compiled, Some(true));
}
