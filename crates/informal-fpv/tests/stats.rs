//! Design information and proof statistics (stats.rs): `get_design_info`,
//! `report -design`, `get_property_info`, `report -detail`, `get_stats`.
//! The design counts need no engine; the proof statistics use the in-house
//! engines and are skipped without them (CaDiCaL).

use informal_fpv::{Flavor, Session, Shell};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn shell(engines: Vec<std::sync::Arc<dyn informal_engine::Engine>>, tag: &str) -> Shell {
    let work = std::env::temp_dir().join(format!("informal-stats-{tag}-{}", std::process::id()));
    Shell::new(Session::new(engines, work), Flavor::Jg)
}

fn counter(engines: Vec<std::sync::Arc<dyn informal_engine::Engine>>, tag: &str) -> Shell {
    let sh = shell(engines, tag);
    let path = root().join("tests/fpv/counter/counter.sv");
    sh.eval(&format!("analyze -sv12 {{{}}}", path.display()))
        .unwrap();
    sh.eval(
        "elaborate -top counter; clock clk; reset -expression {!rst_n}; set_prove_time_limit 60s",
    )
    .unwrap();
    sh
}

fn json(sh: &Shell, cmd: &str) -> Value {
    serde_json::from_str(&sh.eval(cmd).unwrap()).unwrap()
}

#[test]
fn counter_design_counts() {
    let sh = counter(Vec::new(), "counter");
    sh.eval("assert -name a_max {q <= 4'd9}; assume -name m_en {en}; stopat q")
        .unwrap();
    let d = json(&sh, "get_design_info -json");
    let design = &d["design"];
    assert_eq!(design["instances"], 1);
    assert_eq!(design["modules"], 1);
    // clk, rst_n, en; q[3:0], done.
    assert_eq!(design["inputs"], 3);
    assert_eq!(design["input_bits"], 3);
    assert_eq!(design["outputs"], 2);
    assert_eq!(design["output_bits"], 5);
    assert_eq!(design["flops"], 1);
    assert_eq!(design["flop_bits"], 4);
    assert_eq!(design["latches"], 0);
    assert_eq!(design["memories"].as_array().unwrap().len(), 0);
    assert_eq!(design["clocks"][0]["signal"], "clk");
    assert_eq!(design["clocks"][0]["edges"][0], "posedge");
    assert_eq!(design["resets"][0]["signal"], "rst_n");
    assert_eq!(design["resets"][0]["active_high"], false);
    assert_eq!(d["declared_clocks"][0], "clk");
    assert_eq!(d["declared_reset"], "!rst_n");
    assert_eq!(d["cut_points"][0], "q");
    assert_eq!(d["properties"]["asserts"], 1);
    assert_eq!(d["properties"]["assumptions"], 1);
    // The current task's model: built, with the target and the
    // assumption, and bit-blasted.
    let m = &d["model"];
    assert!(m["nodes"].as_u64().unwrap() > 0, "{d:#}");
    assert!(m["bad"].as_u64().unwrap() >= 1, "{d:#}");
    assert!(m["constraints"].as_u64().unwrap() >= 1, "{d:#}");
    assert!(m["aig"]["ands"].as_u64().unwrap() > 0, "{d:#}");
    // The readable table says the same.
    let t = sh.eval("report -design").unwrap();
    assert!(t.contains("flops                1 (4 bits)"), "{t}");
    assert!(t.contains("rst_n (active low, 1 reg)"), "{t}");
    assert!(t.contains("cut points           q"), "{t}");
    assert_eq!(
        sh.eval("get_design_info -list instance").unwrap(),
        "counter"
    );
    assert_eq!(
        sh.eval("get_design_info -list clock -filter c*").unwrap(),
        "clk"
    );
    assert!(sh.eval("get_design_info -list local_parameter").is_err());
}

/// A latch, a memory and two clocks, counted as written.
#[test]
fn latches_memories_and_clocks() {
    let dir = std::env::temp_dir().join(format!("informal-stats-sv-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sv = dir.join("mix.sv");
    std::fs::write(
        &sv,
        r#"
module mix(input logic clk, input logic clk2, input logic en,
           input logic [2:0] wa, input logic [7:0] wd, output logic [7:0] q);
  logic [7:0] mem [0:4];
  logic [7:0] l;
  logic [1:0] slow;
  always_ff @(posedge clk) if (en) mem[wa] <= wd;
  always_latch if (en) l = wd;
  always_ff @(negedge clk2) slow <= slow + 2'd1;
  assign q = mem[0] ^ l ^ {6'd0, slow};
endmodule
"#,
    )
    .unwrap();
    let sh = shell(Vec::new(), "mix");
    sh.eval(&format!(
        "analyze -sv12 {{{}}}; elaborate -top mix",
        sv.display()
    ))
    .unwrap();
    let d = json(&sh, "get_design_info -json")["design"].clone();
    assert_eq!(d["latches"], 1, "{d:#}");
    assert_eq!(d["latch_bits"], 8, "{d:#}");
    let mems = d["memories"].as_array().unwrap();
    assert_eq!(mems.len(), 1, "{d:#}");
    assert_eq!(mems[0]["words"], 5);
    assert_eq!(mems[0]["width"], 8);
    let clocks: Vec<(String, String)> = d["clocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["signal"].as_str().unwrap().to_string(),
                c["edges"][0].as_str().unwrap().to_string(),
            )
        })
        .collect();
    // The memory's write port is on clk, the counter on clk2's falling edge.
    assert!(
        clocks.contains(&("clk".into(), "posedge".into())),
        "{clocks:?}"
    );
    assert!(
        clocks.contains(&("clk2".into(), "negedge".into())),
        "{clocks:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every number a prove reports is one an engine measured.
#[test]
fn proof_statistics() {
    let engines = informal_bmc::engines();
    if engines.is_empty() {
        informal_engine::tool_missing("needs the in-house engines (CaDiCaL)");
        return;
    }
    let sh = counter(engines, "prove");
    sh.eval("assert -name a_max {q <= 4'd9}; assert -name a_never9 {q != 4'd9}; cover -name c_done {done}")
        .unwrap();
    // Before a prove there is nothing to say.
    let before = json(&sh, "get_property_info a_max -json");
    assert!(before.get("stats").is_none(), "{before:#}");
    sh.eval("prove -all").unwrap();
    let cex = json(&sh, "get_property_info a_never9 -json -cone");
    let st = &cex["stats"];
    assert_eq!(cex["status"], "cex");
    assert!(
        st["engine"].as_str().is_some_and(|e| !e.is_empty()),
        "{cex:#}"
    );
    assert_eq!(st["depth_kind"], "trace", "{cex:#}");
    assert!(st["depth"].as_u64().unwrap() >= 9, "{cex:#}");
    assert!(st.get("wall_ms").is_some());
    #[cfg(unix)]
    assert!(st.get("cpu_ms").is_some(), "{cex:#}");
    // The counter is the one register in the cone.
    assert!(cex["cone"]["states"].as_u64().unwrap() >= 1, "{cex:#}");
    assert!(cex["cone"]["state_bits"].as_u64().unwrap() >= 4, "{cex:#}");
    let proof = json(&sh, "get_property_info a_max -json");
    let st = &proof["stats"];
    assert_eq!(proof["status"], "proven");
    // The in-house engines give their evidence; a k-induction proof says
    // k, an invariant its clauses.
    let evidence = st["evidence"].as_str().unwrap_or_default();
    assert!(
        [
            "invariant",
            "k-induction",
            "two-step",
            "two-step-k-induction"
        ]
        .contains(&evidence),
        "{proof:#}"
    );
    if evidence.contains("k-induction") {
        assert_eq!(st["depth_kind"], "k", "{proof:#}");
    }
    if st["depth_kind"] != "k" {
        assert!(st.get("invariant_clauses").is_some(), "{proof:#}");
    }
    // No certificate was asked for.
    assert!(st.get("certificate").is_none(), "{proof:#}");
    assert!(
        [
            "non-vacuous",
            "vacuous",
            "not checked",
            "vacuity not checked",
            "non-vacuous (cut points)"
        ]
        .contains(&proof["vacuity"].as_str().unwrap()),
        "{proof:#}"
    );
    // The readable forms.
    let text = sh.eval("report -detail -property a_never9").unwrap();
    assert!(text.contains("wall time"), "{text}");
    assert!(text.contains("(trace)"), "{text}");
    let all = sh.eval("report -detail").unwrap();
    assert!(all.contains("c_done") && all.contains("a_max"), "{all}");
    let stats: Value = serde_json::from_str(&sh.eval("get_stats").unwrap()).unwrap();
    assert_eq!(stats.as_array().unwrap().len(), 3);
    // The session JSON carries the same numbers.
    let session = json(&sh, "session");
    let p = session["props"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "a_never9")
        .unwrap()
        .clone();
    assert_eq!(p["stats"]["depth_kind"], "trace");
}
