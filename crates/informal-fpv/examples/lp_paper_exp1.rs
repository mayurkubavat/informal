//! Reproducible benchmark runner for Experiment 1 (Symbolic PST SAT scaling)
//! and WS1A (`CompiledDesign` cold vs. warm multi-app caching).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use informal_fpv::{Flavor, Session, Shell};
use informal_svlang::{
    cache_stats, compile, compile_cached, design_from_compiled, lower, lower_from_compiled, Request,
};
use informal_upf::{
    load_upf, sat_backend_name, LsRequirement, NullObjectFinder, PstSat, DEFAULT_VOLTAGE_TOL_UV,
};
use serde_json::json;

fn peak_rss_kb() -> u64 {
    let mut ru = unsafe { std::mem::zeroed::<libc::rusage>() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } != 0 {
        return 0;
    }
    #[cfg(target_os = "macos")]
    {
        (ru.ru_maxrss as u64) / 1024
    }
    #[cfg(not(target_os = "macos"))]
    {
        ru.ru_maxrss as u64
    }
}

fn run_exp1(upf_path: &Path, n: usize) {
    let t_total = Instant::now();
    let t_parse = Instant::now();
    let resolved =
        load_upf(upf_path, None, None, Arc::new(NullObjectFinder)).expect("load_upf failed");
    let parse_resolve_ms = t_parse.elapsed().as_secs_f64() * 1000.0;

    let t_build = Instant::now();
    let mut sat = PstSat::build(&resolved, DEFAULT_VOLTAGE_TOL_UV);
    let consistent = sat.is_state_space_consistent();
    let sat_build_ms = t_build.elapsed().as_secs_f64() * 1000.0;
    let (cnf_vars, cnf_clauses) = sat.cnf_stats();

    let ss_ids: Vec<_> = (0..n)
        .map(|i| resolved.ss_by_name[&format!("SS_D{i}")])
        .collect();
    let (mut off_sat, mut off_unsat, mut ls_none, mut ls_h2l, mut ls_l2h, mut ls_both, mut ls_und) =
        (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);

    let t_queries = Instant::now();
    for i in 0..n {
        for j in 0..n {
            if i == j {
                continue;
            }
            if sat.can_be_off_while_on(ss_ids[i], ss_ids[j]).is_some() {
                off_sat += 1;
            } else {
                off_unsat += 1;
            }
            match sat.voltage_relation(ss_ids[i], ss_ids[j], DEFAULT_VOLTAGE_TOL_UV) {
                LsRequirement::None => ls_none += 1,
                LsRequirement::HighToLow { .. } => ls_h2l += 1,
                LsRequirement::LowToHigh { .. } => ls_l2h += 1,
                LsRequirement::Both { .. } => ls_both += 1,
                LsRequirement::Undetermined { .. } => ls_und += 1,
            }
        }
    }
    let pairwise_query_ms = t_queries.elapsed().as_secs_f64() * 1000.0;
    let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

    println!(
        "{}",
        json!({
            "mode": "exp1",
            "n": n,
            "backend": sat_backend_name(),
            "explicit_pst_rows_expr": format!("3^{n}"),
            "consistent": consistent,
            "cnf_vars": cnf_vars,
            "cnf_clauses": cnf_clauses,
            "cnf_count_method": "pst_sat_cnf_stats",
            "parse_resolve_ms": parse_resolve_ms,
            "sat_build_ms": sat_build_ms,
            "pairwise_query_ms": pairwise_query_ms,
            "total_ms": total_ms,
            "pairwise_queries": n * (n - 1),
            "off_while_on_sat": off_sat,
            "off_while_on_unsat": off_unsat,
            "ls_none": ls_none,
            "ls_high_to_low": ls_h2l,
            "ls_low_to_high": ls_l2h,
            "ls_both": ls_both,
            "ls_undetermined": ls_und,
            "peak_rss_kb": peak_rss_kb(),
        })
    );
}

fn run_ws1a(sv_path: &Path, top: &str, workdir: &Path) {
    let _ = std::fs::remove_dir_all(workdir);
    std::fs::create_dir_all(workdir).expect("create workdir");
    let req = Request {
        files: vec![sv_path.to_path_buf()],
        top: Some(top.to_string()),
        reset: Some("!rst_n".to_string()),
        clocks: vec![("clk".to_string(), 1, 0)],
        trace_outputs: true,
        cover_targets: true,
        ..Default::default()
    };

    let s0 = cache_stats();
    let t_cold_c = Instant::now();
    let compiled = compile_cached(None, &req).expect("compile_cached cold failed");
    let d_cold = design_from_compiled(&compiled, &req);
    assert!(d_cold.ok, "{:?}", d_cold.diagnostics);
    let cold_compile_elab_ms = t_cold_c.elapsed().as_secs_f64() * 1000.0;

    let t_cold_l = Instant::now();
    let l_cold = lower_from_compiled(&compiled, &req);
    assert!(l_cold.ok, "{:?}", l_cold.errors);
    let cold_lower_ms = t_cold_l.elapsed().as_secs_f64() * 1000.0;

    let t_warm_c = Instant::now();
    let compiled_hit = compile_cached(Some(&compiled), &req).expect("compile_cached warm failed");
    assert!(Arc::ptr_eq(&compiled, &compiled_hit));
    let warm_tier1_hit_ms = t_warm_c.elapsed().as_secs_f64() * 1000.0;

    let t_warm_l = Instant::now();
    let l_warm = lower_from_compiled(&compiled, &req);
    assert!(l_warm.ok);
    let warm_tier2_hit_ms = t_warm_l.elapsed().as_secs_f64() * 1000.0;
    let s1 = cache_stats();

    let t_uncached = Instant::now();
    for _ in 0..5 {
        assert!(compile(&req).ok);
        assert!(lower(&req).ok);
    }
    let frontend_5op_uncached_ms = t_uncached.elapsed().as_secs_f64() * 1000.0;

    let t_cached_fe = Instant::now();
    let fe_compiled = compile_cached(None, &req).expect("compile_cached fe failed");
    assert!(design_from_compiled(&fe_compiled, &req).ok);
    assert!(lower_from_compiled(&fe_compiled, &req).ok);
    for _ in 1..5 {
        let hit = compile_cached(Some(&fe_compiled), &req).expect("compile_cached fe hit failed");
        assert!(design_from_compiled(&hit, &req).ok);
        assert!(lower_from_compiled(&hit, &req).ok);
    }
    let frontend_5op_cached_ms = t_cached_fe.elapsed().as_secs_f64() * 1000.0;

    let engines = informal_bmc::engines();
    let has_engines = !engines.is_empty();
    let s2_before = cache_stats();
    let t_sess = Instant::now();
    let sh = Shell::new(Session::new(engines, workdir.join("work")), Flavor::Jg);
    sh.eval(&format!(
        "analyze -sv {{{}}}; elaborate -top {{{top}}}; clock clk; reset {{!rst_n}}",
        sv_path.display()
    ))
    .unwrap();
    sh.eval(&format!(
        "elaborate -top {{{top}}}; clock clk; reset {{!rst_n}}"
    ))
    .unwrap();
    if has_engines {
        sh.eval("prove -all").unwrap();
        sh.eval("check_xprop -init -prove").unwrap();
        sh.eval("check_cov -init -model {coi} -measure").unwrap();
    }
    sh.eval("get_fv_complexity").unwrap();
    let session_5app_cached_ms = t_sess.elapsed().as_secs_f64() * 1000.0;
    let s2_after = cache_stats();

    println!(
        "{}",
        json!({
            "mode": "ws1a",
            "backend": sat_backend_name(),
            "has_engines": has_engines,
            "sv_file": sv_path.display().to_string(),
            "top": top,
            "cold_compile_elab_ms": cold_compile_elab_ms,
            "cold_lower_ms": cold_lower_ms,
            "warm_tier1_hit_ms": warm_tier1_hit_ms,
            "warm_tier2_hit_ms": warm_tier2_hit_ms,
            "frontend_5op_uncached_ms": frontend_5op_uncached_ms,
            "frontend_5op_cached_ms": frontend_5op_cached_ms,
            "session_5app_cached_ms": session_5app_cached_ms,
            "micro_tier1_compiles": s1.tier1_compiles - s0.tier1_compiles,
            "micro_tier1_hits": s1.tier1_hits - s0.tier1_hits,
            "micro_tier2_lowers": s1.tier2_lowers - s0.tier2_lowers,
            "micro_tier2_hits": s1.tier2_hits - s0.tier2_hits,
            "session_tier1_compiles": s2_after.tier1_compiles - s2_before.tier1_compiles,
            "session_tier1_hits": s2_after.tier1_hits - s2_before.tier1_hits,
            "session_tier2_lowers": s2_after.tier2_lowers - s2_before.tier2_lowers,
            "session_tier2_hits": s2_after.tier2_hits - s2_before.tier2_hits,
            "peak_rss_kb": peak_rss_kb(),
        })
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (mut mode, mut upf, mut n, mut sv, mut top) = (
        String::new(),
        PathBuf::new(),
        0usize,
        PathBuf::new(),
        String::new(),
    );
    let mut workdir = PathBuf::from("target/ws1a_bench_work");
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--mode" => {
                i += 1;
                mode = args.get(i).cloned().unwrap_or_default();
            }
            "--upf" => {
                i += 1;
                upf = PathBuf::from(args.get(i).cloned().unwrap_or_default());
            }
            "--n" => {
                i += 1;
                n = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0);
            }
            "--sv" => {
                i += 1;
                sv = PathBuf::from(args.get(i).cloned().unwrap_or_default());
            }
            "--top" => {
                i += 1;
                top = args.get(i).cloned().unwrap_or_default();
            }
            "--workdir" => {
                i += 1;
                workdir = PathBuf::from(args.get(i).cloned().unwrap_or_default());
            }
            other => panic!("unknown argument: {other}"),
        }
        i += 1;
    }
    match mode.as_str() {
        "exp1" => run_exp1(&upf, n),
        "ws1a" => run_ws1a(&sv, &top, &workdir),
        other => panic!("expected --mode exp1 or --mode ws1a, got `{other}`"),
    }
}
