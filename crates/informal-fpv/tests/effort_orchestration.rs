//! Integration tests for engine effort (`Effort`), whole-batch `total_time_limit`,
//! two-pass sweep/converge orchestration (`Orchestration`), persistent
//! `set_max_trace_length` (`BoundedProven` promotion), and non-vacuous AXI4-Lite VIP.

use informal_engine::{
    Capabilities, CheckRequest, Effort, Engine, EngineResult, ModelFormat, RunContext, Verdict,
};
use informal_fpv::{
    Flavor, Orchestration, ProgressKind, ProveProgress, ProveSink, Session, Shell, Status, Vacuity,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type SpyLog = Arc<Mutex<Vec<(Effort, Option<u32>, u128)>>>;

struct SpyEngine {
    seen: SpyLog,
    solve_on_high: bool,
}

impl Engine for SpyEngine {
    fn name(&self) -> &str {
        "spy-engine"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            formats: vec![ModelFormat::Btor2],
            proves: true,
            finds_bugs: true,
            ..Default::default()
        }
    }

    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let budget_ms = ctx
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis();
        self.seen
            .lock()
            .unwrap()
            .push((req.effort, req.max_depth, budget_ms));
        if self.solve_on_high && req.effort >= Effort::High {
            return Ok(EngineResult {
                engine: self.name().into(),
                verdict: Verdict::Safe,
                wall_ms: 5,
                witness: None,
                certificate: None,
            });
        }
        while !ctx.should_stop() {
            std::thread::sleep(Duration::from_millis(15));
        }
        Ok(EngineResult {
            engine: self.name().into(),
            verdict: Verdict::Unknown {
                reason: ctx.stop_reason().into(),
            },
            wall_ms: 15,
            witness: None,
            certificate: None,
        })
    }
}

#[derive(Default)]
struct SinkLog {
    events: Mutex<Vec<ProveProgress>>,
    lines: Mutex<Vec<String>>,
}

impl ProveSink for SinkLog {
    fn progress(&self, p: &ProveProgress) {
        self.events.lock().unwrap().push(p.clone());
    }
    fn line(&self, text: &str, _err: bool) {
        self.lines.lock().unwrap().push(text.to_string());
    }
}

#[test]
fn effort_commands_aliases_vcf_vars_and_max_signoff() {
    let engines = informal_bmc::engines();
    if engines.is_empty() {
        informal_engine::tool_missing("needs CaDiCaL");
        return;
    }
    let work = std::env::temp_dir().join(format!("informal-effort-max-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let sv = work.join("pipe.sv");
    std::fs::write(
        &sv,
        "module pipe(input logic clk, rst, req, en, output logic gnt);\n\
           always_ff @(posedge clk) gnt <= rst ? 1'b0 : (req && en);\n\
         endmodule\n",
    )
    .unwrap();

    let run_dir = work.join("run");
    let sh = Shell::new(Session::new(engines, run_dir.clone()), Flavor::Jg);
    assert_eq!(sh.eval("set_prove_effort").unwrap(), "standard");
    sh.eval("set_prove_effort quick").unwrap();
    assert_eq!(sh.eval("set_prove_effort").unwrap(), "low");
    sh.eval("set_prove_effort deep").unwrap();
    assert_eq!(sh.eval("set_prove_effort").unwrap(), "high");
    sh.eval("set_prove_effort signoff").unwrap();
    assert_eq!(sh.eval("set_prove_effort").unwrap(), "max");

    assert_eq!(sh.eval("set_prove_orchestration").unwrap(), "auto");
    sh.eval("set_prove_orchestration off").unwrap();
    assert_eq!(sh.session.borrow().orchestration, Orchestration::Off);
    sh.eval("set_prove_orchestration auto").unwrap();

    // Signoff (`max`) auto-generates witness traces and minimal proof cores!
    sh.eval(&format!(
        "analyze -sv12 {{{}}}; elaborate -top pipe; clock clk; reset -expression {{rst}};\n\
         assume -name m_en {{en}};\n\
         assume -name m_unused {{1'b1}};\n\
         assert -name a_gnt {{req |=> gnt}};\n\
         prove -all",
        sv.display()
    ))
    .unwrap();

    let s = sh.session.borrow();
    let p = s.props.iter().find(|p| p.name == "a_gnt").unwrap();
    assert!(matches!(p.status, Status::Proven { .. }), "{:?}", p.status);
    assert_eq!(p.vacuity, Some(Vacuity::NonVacuous));
    let wtrace = p
        .witness_trace
        .as_deref()
        .expect("Effort::Max must auto-populate witness_trace");
    assert!(
        run_dir.join(wtrace).is_file(),
        "missing witness VCD {wtrace}"
    );
    let core = p
        .proof_core
        .as_ref()
        .expect("Effort::Max must auto-compute minimal proof_core");
    assert!(core.needed_assumptions.iter().any(|a| a.contains("m_en")));
    drop(s);

    // VCF flavor parity (`set_fml_var fml_effort` / `fml_max_depth` / `check_fv -effort`)
    sh.activate(Flavor::Vcf);
    sh.eval("set_fml_var fml_effort low; set_fml_var fml_max_depth 12")
        .unwrap();
    assert_eq!(sh.session.borrow().effort, Effort::Low);
    assert_eq!(sh.session.borrow().max_trace_length, Some(12));
    sh.eval("check_fv -effort high -time_limit 10s").unwrap();
    // Per-run `-effort` override restores session effort afterwards:
    assert_eq!(sh.session.borrow().effort, Effort::Low);
    sh.eval("set_fml_var fml_max_depth off").unwrap();
    assert_eq!(sh.session.borrow().max_trace_length, None);
}

#[test]
fn two_pass_orchestration_sweeps_low_then_converges_high_without_duplicate_logs() {
    if informal_bmc::engines().is_empty() {
        informal_engine::tool_missing("needs CaDiCaL");
        return;
    }
    let work = std::env::temp_dir().join(format!("informal-orch-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let sv = work.join("two.sv");
    std::fs::write(
        &sv,
        "module two(input logic clk, rst, a, output logic q);\n\
           always_ff @(posedge clk) q <= rst ? 1'b0 : a;\n\
         endmodule\n",
    )
    .unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let spy: Arc<dyn Engine> = Arc::new(SpyEngine {
        seen: seen.clone(),
        solve_on_high: true,
    });
    // Include real 1-step BMC alongside SpyEngine:
    // - `c_easy` (`q == 1'b1`) is covered by 1-step BMC in <20 ms during Pass 1 (`Effort::Low`).
    // - `a_hard` (`!q || q` folded? use `q == $past(a)` — BMC alone cannot prove unboundedly,
    //   and SpyEngine only returns `Safe` when `req.effort >= Effort::High` in Pass 2!).
    let bmc_only: Arc<dyn Engine> = Arc::new(informal_bmc::Bmc {
        max_depth: None,
        step: 1,
    });
    let mut sess = Session::new(vec![bmc_only, spy], work.join("run"));
    let sink = Arc::new(SinkLog::default());
    sess.sink = Some(sink.clone());
    let sh = Shell::new(sess, Flavor::Jg);

    sh.eval(&format!(
        "analyze -sv12 {{{}}}; elaborate -top two; clock clk; reset -expression {{rst}};\n\
         informal::set_vacuity off;\n\
         set_prove_time_limit 800ms;\n\
         set_prove_effort high;\n\
         cover -name c_easy {{q}};\n\
         assert -name a_hard {{1'b1 |=> (q == $past(a))}};\n\
         prove -all",
        sv.display()
    ))
    .unwrap();

    assert_eq!(sh.eval("get_status c_easy").unwrap(), "covered");
    assert_eq!(sh.eval("get_status a_hard").unwrap(), "proven");

    let calls = seen.lock().unwrap().clone();
    // Pass 1 (`Effort::Low`) sheds heavy engines (`spy-engine`) while `informal-bmc`
    // covers `c_easy` in <30 ms; Pass 2 (`Effort::High`) runs ONLY undecided `a_hard`
    // with the remaining ~550 ms budget (`800ms - 250ms` Pass-1 sweep)!
    assert_eq!(
        calls.len(),
        1,
        "expected spy-engine to be shed in Pass-1 Low and invoked only on undecided a_hard in Pass-2 High, got {calls:?}"
    );
    assert_eq!(calls[0].0, Effort::High);
    assert!(
        (400..=600).contains(&calls[0].2),
        "expected Pass-2 remaining budget near 550ms, got {}ms",
        calls[0].2
    );

    // Console log must report each property's verdict exactly once (no premature undetermined line).
    let lines = sink.lines.lock().unwrap().clone();
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("c_easy:")).count(),
        1,
        "{lines:?}"
    );
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("a_hard:")).count(),
        1,
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("undetermined")),
        "{lines:?}"
    );

    // With explicit engine selection (`informal::set_engines`), Pass 1 keeps `spy-engine`
    // and dispatches `Effort::Low` first, then `Effort::High` in Pass 2 on `a_hard`.
    seen.lock().unwrap().clear();
    sh.eval("informal::set_engines {spy-engine}; prove -all")
        .unwrap();
    let explicit_calls = seen.lock().unwrap().clone();
    let low_calls = explicit_calls
        .iter()
        .filter(|(e, _, _)| *e == Effort::Low)
        .count();
    let high_calls = explicit_calls
        .iter()
        .filter(|(e, _, _)| *e == Effort::High)
        .count();
    assert_eq!(
        low_calls, 2,
        "expected 2 Pass-1 Low sweeps under explicit engine list, got {explicit_calls:?}"
    );
    assert_eq!(
        high_calls, 2,
        "expected 2 Pass-2 High runs under spy-only engine list, got {explicit_calls:?}"
    );
}

#[test]
fn total_time_limit_caps_batch_and_marks_remaining_total_time_limit() {
    let work = std::env::temp_dir().join(format!("informal-tot-tl-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let sv = work.join("slow.sv");
    std::fs::write(
        &sv,
        "module slow(input logic clk, rst, a, output logic [3:0] q);\n\
           always_ff @(posedge clk) q <= rst ? 4'd0 : (q + {3'd0, a});\n\
         endmodule\n",
    )
    .unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let spy: Arc<dyn Engine> = Arc::new(SpyEngine {
        seen,
        solve_on_high: false,
    });
    let mut sess = Session::new(vec![spy], work.join("run"));
    let sink = Arc::new(SinkLog::default());
    sess.sink = Some(sink.clone());
    let sh = Shell::new(sess, Flavor::Jg);

    let t0 = Instant::now();
    sh.eval(&format!(
        "analyze -sv12 {{{}}}; elaborate -top slow; clock clk; reset -expression {{rst}};\n\
         informal::set_vacuity off;\n\
         set_prove_orchestration off;\n\
         set_prove_time_limit 10s;\n\
         set_prove_total_time_limit 250ms;\n\
         assert -name a1 {{q != 4'd15}};\n\
         assert -name a2 {{q != 4'd14}};\n\
         assert -name a3 {{q != 4'd13}};\n\
         prove -all",
        sv.display()
    ))
    .unwrap();
    let elapsed = t0.elapsed();
    assert!(
        elapsed < Duration::from_secs(3),
        "batch total_time_limit should cap whole run near 250ms, took {elapsed:?}"
    );
    assert_eq!(sh.eval("set_prove_total_time_limit").unwrap(), "250ms");

    let s = sh.session.borrow();
    assert!(
        s.props.iter().any(
            |p| matches!(&p.status, Status::Undetermined { reason } if reason == "total time limit")
        ),
        "expected unreached property to carry reason `total time limit`, got {:?}",
        s.props.iter().map(|p| &p.status).collect::<Vec<_>>()
    );
    // Final progress event must be `Done`, NOT `Stopped`!
    let last_ev = sink.events.lock().unwrap().last().unwrap().kind;
    assert_eq!(last_ev, ProgressKind::Done);
    drop(s);

    sh.eval("set_prove_total_time_limit off").unwrap();
    assert_eq!(sh.eval("set_prove_total_time_limit").unwrap(), "");
}

#[test]
fn persistent_max_trace_length_proves_unbounded_or_promotes_bounded_proven() {
    let engines = informal_bmc::engines();
    if engines.is_empty() {
        informal_engine::tool_missing("needs CaDiCaL");
        return;
    }
    let work = std::env::temp_dir().join(format!("informal-max-tl-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let sv = work.join("cnt.sv");
    std::fs::write(
        &sv,
        "module cnt(input logic clk, rst, output logic [3:0] q);\n\
           always_ff @(posedge clk)\n\
             if (rst) q <= 4'd0;\n\
             else if (q < 4'd12) q <= q + 4'd1;\n\
         endmodule\n",
    )
    .unwrap();

    let sh = Shell::new(Session::new(engines, work.join("run")), Flavor::Jg);
    sh.eval(&format!(
        "analyze -sv12 {{{}}}; elaborate -top cnt; clock clk; reset -expression {{rst}};\n\
         informal::set_vacuity off;\n\
         assert -name a_safe {{q <= 4'd12}};\n\
         assert -name a_late_bug {{q != 4'd10}};\n\
         set_max_trace_length 5;\n\
         prove -property a_safe",
        sv.display()
    ))
    .unwrap();
    assert_eq!(sh.eval("set_max_trace_length").unwrap(), "5");
    // Inductive engines still prove `a_safe` unboundedly (`proven`, not `bounded_proven`)!
    assert_eq!(sh.eval("get_status a_safe").unwrap(), "proven");

    // Under BMC-only (`set_engine_mode B`), `a_late_bug` (violation at depth > 5) clears
    // bound 5 cleanly and promotes to `bounded_proven`!
    sh.eval("set_engine_mode B; prove -property a_late_bug")
        .unwrap();
    assert_eq!(sh.eval("get_status a_late_bug").unwrap(), "bounded_proven");
    {
        let s = sh.session.borrow();
        let p = s.props.iter().find(|p| p.name == "a_late_bug").unwrap();
        assert!(
            matches!(p.status, Status::BoundedProven { bound: 5, .. }),
            "expected BoundedProven(5), got {:?}",
            p.status
        );
    }

    // Clearing `set_max_trace_length 0` finds the real CEX beyond depth 5!
    sh.eval("set_max_trace_length 0; prove -property a_late_bug")
        .unwrap();
    assert_eq!(sh.eval("set_max_trace_length").unwrap(), "");
    assert_eq!(sh.eval("get_status a_late_bug").unwrap(), "cex");
}

#[test]
fn axi4lite_demo_proves_all_6_assertions_non_vacuous_and_covers_both_txns() {
    let mut engines = informal_bmc::engines();
    engines.extend(informal_bitwuzla::engines());
    if engines.is_empty() {
        informal_engine::tool_missing("needs CaDiCaL");
        return;
    }
    let work = std::env::temp_dir().join(format!("informal-axil-demo-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let sv = work.join("axi_lite_demo.sv");
    std::fs::write(
        &sv,
        "module axi_lite_demo (\n\
           input  logic        clk,\n\
           input  logic        rst_n,\n\
           output logic        awvalid,\n\
           input  logic        awready,\n\
           output logic [31:0] awaddr,\n\
           output logic        wvalid,\n\
           input  logic        wready,\n\
           output logic [31:0] wdata,\n\
           output logic [3:0]  wstrb,\n\
           input  logic        bvalid,\n\
           output logic        bready,\n\
           input  logic [1:0]  bresp,\n\
           output logic        arvalid,\n\
           input  logic        arready,\n\
           output logic [31:0] araddr,\n\
           input  logic        rvalid,\n\
           output logic        rready,\n\
           input  logic [31:0] rdata,\n\
           input  logic [1:0]  rresp\n\
         );\n\
           assign bready = 1'b1;\n\
           assign rready = 1'b1;\n\
           always_ff @(posedge clk or negedge rst_n) begin\n\
             if (!rst_n) begin\n\
               awvalid <= 1'b0;\n\
               awaddr  <= 32'h0;\n\
               wvalid  <= 1'b0;\n\
               wdata   <= 32'h0;\n\
               wstrb   <= 4'hf;\n\
               arvalid <= 1'b0;\n\
               araddr  <= 32'h0;\n\
             end else begin\n\
               if (!awvalid) begin\n\
                 awvalid <= 1'b1;\n\
                 awaddr  <= awaddr + 32'h4;\n\
               end else if (awready) begin\n\
                 awvalid <= 1'b0;\n\
               end\n\
               if (!wvalid) begin\n\
                 wvalid <= 1'b1;\n\
                 wdata  <= wdata + 32'h1;\n\
                 wstrb  <= 4'hf;\n\
               end else if (wready) begin\n\
                 wvalid <= 1'b0;\n\
               end\n\
               if (!arvalid) begin\n\
                 arvalid <= 1'b1;\n\
                 araddr  <= araddr + 32'h4;\n\
               end else if (arready) begin\n\
                 arvalid <= 1'b0;\n\
               end\n\
             end\n\
           end\n\
         endmodule\n",
    )
    .unwrap();

    let mut sess = Session::new(engines, work.join("run"));
    let sink = Arc::new(SinkLog::default());
    sess.sink = Some(sink.clone());
    let sh = Shell::new(sess, Flavor::Jg);
    sh.eval(&format!(
        "analyze -sv12 {{{}}}; elaborate -top axi_lite_demo; clock clk; reset -expression {{!rst_n}};\n\
         check_protocol -create -protocol axi4lite -role master;\n\
         task -set Protocol;\n\
         prove -all",
        sv.display()
    ))
    .unwrap();

    let s = sh.session.borrow();
    let proto_asserts: Vec<_> = s
        .props
        .iter()
        .filter(|p| p.task == "Protocol" && p.kind == informal_fpv::Kind::Assert)
        .collect();
    assert_eq!(proto_asserts.len(), 6);
    for p in &proto_asserts {
        assert!(
            matches!(p.status, Status::Proven { .. }),
            "{} not proven: {:?}",
            p.name,
            p.status
        );
        assert_eq!(
            p.vacuity,
            Some(Vacuity::NonVacuous),
            "{} should be NonVacuous, got {:?}",
            p.name,
            p.vacuity
        );
    }
    let proto_covers: Vec<_> = s
        .props
        .iter()
        .filter(|p| p.task == "Protocol" && p.kind == informal_fpv::Kind::Cover)
        .collect();
    assert_eq!(proto_covers.len(), 2);
    for c in &proto_covers {
        assert!(
            matches!(c.status, Status::Covered { .. }),
            "{} not covered: {:?}",
            c.name,
            c.status
        );
    }
    let lines = sink.lines.lock().unwrap().join("\n");
    assert!(
        !lines.contains("VACUOUS"),
        "expected 0 VACUOUS warnings, got:\n{lines}"
    );
}
