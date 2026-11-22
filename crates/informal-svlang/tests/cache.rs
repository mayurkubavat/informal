use informal_svlang::{
    cache_stats, compile_cached, design_from_compiled, lower_from_compiled, CompiledDesign, Request,
};
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn temp_dir(tag: &str) -> PathBuf {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "informal-svlang-cache-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn compiled_design_is_send_and_sync_and_threads_share_lower_cache() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<CompiledDesign>();
    assert_send_sync::<Arc<CompiledDesign>>();

    let dir = temp_dir("send_sync");
    let sv = dir.join("dut.sv");
    std::fs::write(
        &sv,
        "module dut(input logic clk, input logic [3:0] a, output logic [3:0] q);\n\
           always_ff @(posedge clk) q <= a + 4'd1;\n\
         endmodule\n",
    )
    .unwrap();
    let req = Request {
        files: vec![sv],
        top: Some("dut".into()),
        ..Default::default()
    };
    let compiled = compile_cached(None, &req).unwrap();
    assert!(compiled.check_ok);

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let c = Arc::clone(&compiled);
            let r = req.clone();
            std::thread::spawn(move || lower_from_compiled(&c, &r))
        })
        .collect();
    for h in handles {
        let lowered = h.join().unwrap();
        assert!(lowered.ok, "{:?}", lowered.errors);
        assert!(lowered.btor2.contains("dut.q"));
    }
    assert_eq!(compiled.tier2_lowers(), 1);
    assert_eq!(compiled.tier2_hits(), 3);
}

#[test]
fn cache_hit_reuses_tier1_and_tier2_with_top_inference_and_unused_bbox_pruning() {
    let dir = temp_dir("hit");
    let sv = dir.join("top.sv");
    std::fs::write(
        &sv,
        "module top(input logic clk, input logic [3:0] d, output logic [3:0] q);\n\
           prim_stub u_stub(.d(d));\n\
           always_ff @(posedge clk) q <= d;\n\
         endmodule\n",
    )
    .unwrap();

    let req_elab = Request {
        files: vec![sv.clone()],
        top: None,
        black_box_modules: vec!["prim_stub".into(), "unused_stub".into()],
        ..Default::default()
    };
    let s0 = cache_stats();
    let c1 = compile_cached(None, &req_elab).unwrap();
    let s1 = cache_stats();
    assert!(c1.check_ok, "{:?}", c1.diagnostics);
    assert_eq!(c1.inferred_single_top.as_deref(), Some("top"));
    assert_eq!(s1.tier1_compiles - s0.tier1_compiles, 1);
    assert_eq!(s1.tier1_hits - s0.tier1_hits, 0);

    // Same request -> Tier-1 hit.
    let c2 = compile_cached(Some(&c1), &req_elab).unwrap();
    let s2 = cache_stats();
    assert!(Arc::ptr_eq(&c1, &c2));
    assert_eq!(s2.tier1_compiles - s1.tier1_compiles, 0);
    assert_eq!(s2.tier1_hits - s1.tier1_hits, 1);

    // `top: Some("top")` and pruned unused bbox (`["prim_stub"]`) -> still Tier-1 hit!
    let mut req_lower = Request {
        files: vec![sv],
        top: Some("top".into()),
        black_box_modules: vec!["prim_stub".into()],
        trace_outputs: true,
        ..Default::default()
    };
    let c3 = compile_cached(Some(&c1), &req_lower).unwrap();
    let s3 = cache_stats();
    assert!(Arc::ptr_eq(&c1, &c3));
    assert_eq!(s3.tier1_compiles - s2.tier1_compiles, 0);
    assert_eq!(s3.tier1_hits - s2.tier1_hits, 1);

    // Tier-2 lowering runs once and hits on identical LowerKey with extra command property.
    let l1 = lower_from_compiled(&c1, &req_lower);
    assert!(l1.ok, "{:?}", l1.errors);
    assert_eq!(c1.tier2_lowers(), 1);
    assert_eq!(c1.tier2_hits(), 0);

    req_lower.properties = vec![(
        "p_id".into(),
        "assert property (@(posedge clk) q == q)".into(),
    )];
    let l2 = lower_from_compiled(&c1, &req_lower);
    assert!(l2.ok, "{:?}", l2.errors);
    assert_eq!(c1.tier2_lowers(), 1);
    assert_eq!(c1.tier2_hits(), 1);
    assert_eq!(l2.properties.len(), 1);
}

#[test]
fn invalidates_on_sv_content_change_even_with_same_len_and_preserved_mtime() {
    let dir = temp_dir("sv_mtime");
    let sv = dir.join("dut.sv");
    let src1 = "module dut(input logic [3:0] a, output logic [3:0] y);\n  assign y = a + 4'd1;\nendmodule\n";
    let src2 = "module dut(input logic [3:0] a, output logic [3:0] y);\n  assign y = a + 4'd2;\nendmodule\n";
    assert_eq!(src1.len(), src2.len());
    std::fs::write(&sv, src1).unwrap();
    let orig_meta = std::fs::metadata(&sv).unwrap();
    let orig_mtime = orig_meta.modified().unwrap();

    let req = Request {
        files: vec![sv.clone()],
        top: Some("dut".into()),
        ..Default::default()
    };
    let c1 = compile_cached(None, &req).unwrap();
    let l1 = lower_from_compiled(&c1, &req);
    assert!(l1.ok, "{:?}", l1.errors);

    // Rewrite with identical byte length and explicitly restore original mtime.
    std::fs::write(&sv, src2).unwrap();
    File::options()
        .write(true)
        .open(&sv)
        .unwrap()
        .set_modified(orig_mtime)
        .unwrap();
    let new_meta = std::fs::metadata(&sv).unwrap();
    assert_eq!(new_meta.len(), orig_meta.len());
    assert_eq!(new_meta.modified().unwrap(), orig_mtime);

    assert!(!c1.is_fresh());
    let s_before = cache_stats();
    let c2 = compile_cached(Some(&c1), &req).unwrap();
    let s_after = cache_stats();
    assert!(!Arc::ptr_eq(&c1, &c2));
    assert_eq!(s_after.tier1_compiles - s_before.tier1_compiles, 1);

    let l2 = lower_from_compiled(&c2, &req);
    assert!(l2.ok, "{:?}", l2.errors);
    assert_ne!(l1.btor2, l2.btor2);
}

#[test]
fn invalidates_on_included_svh_header_change() {
    let dir = temp_dir("svh_include");
    let svh = dir.join("defs.svh");
    let sv = dir.join("dut.sv");
    let svh1 = "`define INC_ADD 4'd1\n";
    let svh2 = "`define INC_ADD 4'd3\n";
    assert_eq!(svh1.len(), svh2.len());
    std::fs::write(&svh, svh1).unwrap();
    let svh_mtime = std::fs::metadata(&svh).unwrap().modified().unwrap();
    std::fs::write(
        &sv,
        "`include \"defs.svh\"\n\
         module dut(input logic [3:0] a, output logic [3:0] y);\n\
           assign y = a + `INC_ADD;\n\
         endmodule\n",
    )
    .unwrap();

    let req = Request {
        files: vec![sv],
        incdirs: vec![dir.clone()],
        top: Some("dut".into()),
        ..Default::default()
    };
    let c1 = compile_cached(None, &req).unwrap();
    assert!(c1.check_ok, "{:?}", c1.diagnostics);
    let canon_svh = std::fs::canonicalize(&svh).unwrap();
    assert!(
        c1.file_digests.iter().any(|d| d.path == canon_svh),
        "expected included header {} in file_digests: {:?}",
        canon_svh.display(),
        c1.file_digests
    );
    let l1 = lower_from_compiled(&c1, &req);
    assert!(l1.ok);

    // Unchanged re-compile hits.
    let c_hit = compile_cached(Some(&c1), &req).unwrap();
    assert!(Arc::ptr_eq(&c1, &c_hit));

    // Modify ONLY the included `.svh` header (same length + preserved mtime).
    std::fs::write(&svh, svh2).unwrap();
    File::options()
        .write(true)
        .open(&svh)
        .unwrap()
        .set_modified(svh_mtime)
        .unwrap();

    assert!(!c1.is_fresh());
    let s_before = cache_stats();
    let c2 = compile_cached(Some(&c1), &req).unwrap();
    let s_after = cache_stats();
    assert!(!Arc::ptr_eq(&c1, &c2));
    assert_eq!(s_after.tier1_compiles - s_before.tier1_compiles, 1);
    let l2 = lower_from_compiled(&c2, &req);
    assert!(l2.ok);
    assert_ne!(l1.btor2, l2.btor2);
}

#[test]
fn invalidates_on_defines_value_and_order_sensitivity() {
    let dir = temp_dir("defines");
    let sv = dir.join("dut.sv");
    std::fs::write(
        &sv,
        "module dut(input logic [3:0] a, output logic [3:0] y);\n\
           assign y = a + `VAL;\n\
         endmodule\n",
    )
    .unwrap();

    let req1 = Request {
        files: vec![sv.clone()],
        defines: vec![
            ("VAL".into(), Some("4'd1".into())),
            ("VAL".into(), Some("4'd2".into())),
        ],
        top: Some("dut".into()),
        ..Default::default()
    };
    let req2 = Request {
        files: vec![sv],
        defines: vec![
            ("VAL".into(), Some("4'd2".into())),
            ("VAL".into(), Some("4'd1".into())),
        ],
        top: Some("dut".into()),
        ..Default::default()
    };

    let c1 = compile_cached(None, &req1).unwrap();
    let l1 = lower_from_compiled(&c1, &req1);
    assert!(l1.ok);

    let s_before = cache_stats();
    let c2 = compile_cached(Some(&c1), &req2).unwrap();
    let s_after = cache_stats();
    assert!(!Arc::ptr_eq(&c1, &c2));
    assert_eq!(s_after.tier1_compiles - s_before.tier1_compiles, 1);
    let l2 = lower_from_compiled(&c2, &req2);
    assert!(l2.ok);
    assert_ne!(l1.btor2, l2.btor2);
}

#[test]
fn invalidates_on_param_change() {
    let dir = temp_dir("params");
    let sv = dir.join("dut.sv");
    std::fs::write(
        &sv,
        "module dut #(parameter int W = 4) (input logic [W-1:0] a, output logic [W-1:0] y);\n\
           assign y = a;\n\
         endmodule\n",
    )
    .unwrap();

    let req4 = Request {
        files: vec![sv.clone()],
        params: vec![("W".into(), "4".into())],
        top: Some("dut".into()),
        ..Default::default()
    };
    let req8 = Request {
        files: vec![sv],
        params: vec![("W".into(), "8".into())],
        top: Some("dut".into()),
        ..Default::default()
    };

    let c4 = compile_cached(None, &req4).unwrap();
    let d4 = design_from_compiled(&c4, &req4);
    assert_eq!(
        d4.signals.iter().find(|s| s.path == "dut.y").unwrap().width,
        4
    );

    let s_before = cache_stats();
    let c8 = compile_cached(Some(&c4), &req8).unwrap();
    let s_after = cache_stats();
    assert!(!Arc::ptr_eq(&c4, &c8));
    assert_eq!(s_after.tier1_compiles - s_before.tier1_compiles, 1);
    let d8 = design_from_compiled(&c8, &req8);
    assert_eq!(
        d8.signals.iter().find(|s| s.path == "dut.y").unwrap().width,
        8
    );
}

#[test]
fn bbox_module_invalidates_tier1_while_bbox_instance_and_cut_points_relower_tier2() {
    let dir = temp_dir("bbox_tier1_tier2");
    let sv = dir.join("top.sv");
    std::fs::write(
        &sv,
        "module sub(input logic [3:0] a, output logic [3:0] y);\n\
           assign y = a + 4'd1;\n\
         endmodule\n\
         module top(input logic [3:0] a, output logic [3:0] y0, output logic [3:0] y1);\n\
           logic [3:0] mid;\n\
           sub u0(.a(a), .y(mid));\n\
           sub u1(.a(a), .y(y1));\n\
           assign y0 = mid;\n\
         endmodule\n",
    )
    .unwrap();

    let base_req = Request {
        files: vec![sv.clone()],
        top: Some("top".into()),
        ..Default::default()
    };
    let c1 = compile_cached(None, &base_req).unwrap();
    let l_base = lower_from_compiled(&c1, &base_req);
    assert!(l_base.ok);
    assert_eq!(c1.tier2_lowers(), 1);

    // `black_box_modules` on an instantiated module invalidates Tier-1.
    let req_bbox_mod = Request {
        black_box_modules: vec!["sub".into()],
        ..base_req.clone()
    };
    let s_before = cache_stats();
    let c_mod = compile_cached(Some(&c1), &req_bbox_mod).unwrap();
    let s_after = cache_stats();
    assert!(!Arc::ptr_eq(&c1, &c_mod));
    assert_eq!(s_after.tier1_compiles - s_before.tier1_compiles, 1);

    // `black_box_instances` hits Tier-1 and re-lowers Tier-2.
    let req_bbox_inst = Request {
        black_box_instances: vec!["top.u0".into()],
        ..base_req.clone()
    };
    let s_before = cache_stats();
    let c_inst = compile_cached(Some(&c1), &req_bbox_inst).unwrap();
    let s_after = cache_stats();
    assert!(Arc::ptr_eq(&c1, &c_inst));
    assert_eq!(s_after.tier1_compiles - s_before.tier1_compiles, 0);
    assert_eq!(s_after.tier1_hits - s_before.tier1_hits, 1);
    let l_inst = lower_from_compiled(&c1, &req_bbox_inst);
    assert!(l_inst.ok, "{:?}", l_inst.errors);
    assert_eq!(c1.tier2_lowers(), 2);
    assert_ne!(l_base.btor2, l_inst.btor2);

    // `cut_points` hits Tier-1 and re-lowers Tier-2 without Tier-1 recompile, then hits Tier-2 on repeat.
    let req_cut = Request {
        cut_points: vec!["top.mid".into()],
        ..base_req
    };
    let s_before = cache_stats();
    let c_cut = compile_cached(Some(&c1), &req_cut).unwrap();
    let s_after = cache_stats();
    assert!(Arc::ptr_eq(&c1, &c_cut));
    assert_eq!(s_after.tier1_compiles - s_before.tier1_compiles, 0);
    let l_cut1 = lower_from_compiled(&c1, &req_cut);
    assert!(l_cut1.ok, "{:?}", l_cut1.errors);
    assert_eq!(c1.tier2_lowers(), 3);
    assert_eq!(c1.tier2_hits(), 0);
    assert!(l_cut1.btor2.contains("top.mid"));

    let l_cut2 = lower_from_compiled(&c1, &req_cut);
    assert!(l_cut2.ok);
    assert_eq!(c1.tier2_lowers(), 3);
    assert_eq!(c1.tier2_hits(), 1);
    assert_eq!(l_cut1.btor2, l_cut2.btor2);
}

#[test]
fn lower_from_compiled_fails_closed_on_compile_errors() {
    let dir = temp_dir("fail_closed");
    let sv = dir.join("bad.sv");
    std::fs::write(&sv, "module bad(input logic a); assign y = ; endmodule\n").unwrap();
    let req = Request {
        files: vec![sv],
        top: Some("bad".into()),
        ..Default::default()
    };
    let c_bad = compile_cached(None, &req).unwrap();
    assert!(!c_bad.check_ok);
    let l_bad = lower_from_compiled(&c_bad, &req);
    assert!(!l_bad.ok);
    assert!(!l_bad.errors.is_empty());
    assert_eq!(c_bad.tier2_lowers(), 0);
}
