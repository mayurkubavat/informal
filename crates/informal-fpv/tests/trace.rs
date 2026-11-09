//! Traces end to end on the counter's failing assertion and cover: proved
//! by informal-bmc (its named witness feeds the VCD) and by informal-kind (no
//! witness, so the session re-runs BMC for the trace). Skipped without
//! CaDiCaL.

use informal_fpv::{Flavor, Session, Shell, Status};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The value of `name` (a `$var` leaf) at time `t` in a VCD written by
/// informal: the last change at or before `t`.
fn value_at(vcd: &str, name: &str, t: u64) -> Option<String> {
    let id = vcd.lines().find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        (f.first() == Some(&"$var") && f.get(4) == Some(&name)).then(|| f[3].to_string())
    })?;
    let mut val = None;
    for l in vcd.lines() {
        if let Some(n) = l.strip_prefix('#') {
            let now: u64 = n.parse().ok()?;
            if now > t {
                break;
            }
        } else if let Some((v, i)) = l.strip_prefix('b').and_then(|r| r.split_once(' ')) {
            if i == id {
                val = Some(v.to_string());
            }
        } else if l.len() > 1 && l[1..] == id && "01xz".contains(&l[..1]) {
            val = Some(l[..1].to_string());
        }
    }
    val
}

fn prove_counter(engine: &str, tag: &str) -> Option<Shell> {
    let mut engines = informal_bmc::engines();
    if engines.is_empty() {
        eprintln!("skipping: informal-bmc engines need CaDiCaL");
        return None;
    }
    if engine == "fake-cex" {
        // Says SAT with a one-frame witness that cannot reach the property.
        let toml = r#"
[[engine]]
name = "fake-cex"
program = "sh"
args = ["-c", "printf 'SAT\\nsat\\nb0\\n#0\\n@0\\n.\\n'", "fake", "{model}"]
formats = ["btor2"]
finds_bugs = true
cex_witness = true
[engine.patterns]
unsafe = '(?m)^SAT$'
"#;
        engines = informal_engines_ext::parse_config(toml).unwrap();
    }
    if engine.starts_with("ric3") {
        let on_path = std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("ric3").is_file()));
        if !on_path {
            eprintln!("skipping: needs ric3 on PATH");
            return None;
        }
        engines = informal_engines_ext::load(&root().join("config/engines.toml")).unwrap();
    }
    engines.retain(|e| e.name() == engine);
    assert_eq!(engines.len(), 1, "no engine {engine}");
    let work = std::env::temp_dir().join(format!("informal-trace-{tag}-{}", std::process::id()));
    // The counter script without the safe properties, which BMC alone
    // would run on until the time limit.
    let dir = root().join("tests/fpv/counter");
    let script: String = std::fs::read_to_string(dir.join("counter.jg.tcl"))
        .unwrap()
        .replace(
            "[file join [file dirname [info script]] counter.sv]",
            &dir.join("counter.sv").display().to_string(),
        )
        .lines()
        .filter(|l| {
            !["a_max ", "a_step ", "a_hold ", "a_no_wrap ", "c_10 "]
                .iter()
                .any(|n| l.contains(&format!("-name {n}")))
        })
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::create_dir_all(&work).unwrap();
    let path = work.join("counter_traces.tcl");
    std::fs::write(&path, script).unwrap();
    let sh = Shell::new(Session::new(engines, work), Flavor::Jg);
    sh.source(&path).unwrap();
    Some(sh)
}

fn check_counter_traces(sh: &Shell, engine: &str) {
    let s = sh.session.borrow();
    let get = |n: &str| s.props.iter().find(|p| p.name == n).unwrap();
    for name in ["a_never9", "c_done"] {
        let p = get(name);
        let depth = match &p.status {
            Status::Cex { depth, .. } | Status::Covered { depth, .. } => {
                depth.expect("depth") as u64
            }
            other => panic!("{name}: {other:?}"),
        };
        let path = p
            .trace
            .as_ref()
            .unwrap_or_else(|| panic!("{name} has no trace"));
        assert!(path.ends_with(&format!("{name}.vcd")), "{path}");
        let vcd = std::fs::read_to_string(path).unwrap();
        // q reaches 9 in the last cycle; the property lane rises there only.
        assert_eq!(
            value_at(&vcd, "q", 10 * depth).as_deref(),
            Some("1001"),
            "{vcd}"
        );
        assert_eq!(value_at(&vcd, name, 10 * depth).as_deref(), Some("1"));
        assert_eq!(value_at(&vcd, name, 10 * depth - 10).as_deref(), Some("0"));
        assert!(vcd.contains("$scope module counter $end"));
        assert!(
            vcd.contains(" clk $end") && vcd.contains(" en $end") && vcd.contains(" rst_n $end")
        );
        assert!(vcd.trim_end().ends_with(&format!("#{}", 10 * (depth + 1))));
        // Combinational nets come from the named outputs (slang front end
        // only): `done` = (q == 9) rises with q in the last cycle.
        if s.frontend == "slang" {
            assert_eq!(
                value_at(&vcd, "done", 10 * depth).as_deref(),
                Some("1"),
                "{vcd}"
            );
            assert_eq!(
                value_at(&vcd, "done", 10 * depth - 10).as_deref(),
                Some("0")
            );
            assert_eq!(
                value_at(&vcd, "nxt", 10 * depth - 10).as_deref(),
                Some("1001")
            );
            assert_eq!(value_at(&vcd, "wrap", 0).as_deref(), Some("0"));
        }
    }
    assert_eq!(get("m_en_burst").trace, None);
    let json = serde_json::to_value(&*s).unwrap();
    assert!(json["props"][0]["trace"]
        .as_str()
        .unwrap()
        .ends_with("a_never9.vcd"));
    // The proof profiler's record: the one engine ran and won.
    let runs = json["props"][0]["engines"].as_array().unwrap();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0]["engine"], *engine);
    assert_eq!(runs[0]["outcome"], "won");
    assert!(runs[0]["ms"].as_u64().is_some());
    // The winner's depth: its own, or the replayed trace's.
    assert!(runs[0]["depth"].as_u64().is_some(), "{runs:?}");
}

#[test]
fn counter_traces_from_bmc_witness() {
    let Some(sh) = prove_counter("informal-bmc", "bmc") else {
        return;
    };
    check_counter_traces(&sh, "informal-bmc");
    assert_eq!(
        sh.session
            .borrow()
            .trace_reruns
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    let _ = std::fs::remove_dir_all(&sh.session.borrow().workdir);
}

#[test]
fn counter_traces_from_ric3_witness_without_a_rerun() {
    // rIC3 IC3 prints its counterexample (--cex, Btor2 witness format);
    // informal converts it to a named witness, so no BMC re-run is needed.
    let Some(sh) = prove_counter("ric3-ic3", "ric3") else {
        return;
    };
    check_counter_traces(&sh, "ric3-ic3");
    assert_eq!(
        sh.session
            .borrow()
            .trace_reruns
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    let _ = std::fs::remove_dir_all(&sh.session.borrow().workdir);
}

#[test]
fn a_witness_that_does_not_replay_falls_back_to_bmc() {
    let Some(sh) = prove_counter("fake-cex", "fake") else {
        return;
    };
    check_counter_traces(&sh, "fake-cex");
    assert_eq!(
        sh.session
            .borrow()
            .trace_reruns
            .load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    let _ = std::fs::remove_dir_all(&sh.session.borrow().workdir);
}

#[test]
fn counter_traces_when_the_winner_has_no_witness() {
    // informal-kind finds the counterexamples in its base case but exports no
    // witness, so the session re-runs BMC for the trace.
    let Some(sh) = prove_counter("informal-kind", "kind") else {
        return;
    };
    check_counter_traces(&sh, "informal-kind");
    // One re-run per trace (a_never9, c_done).
    assert_eq!(
        sh.session
            .borrow()
            .trace_reruns
            .load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    let _ = std::fs::remove_dir_all(&sh.session.borrow().workdir);
}

/// A hand-written AIGER witness for the 2-bit counter, converted by
/// informal_engine::witness, replays on the model: q reaches 3 in cycle 3. One
/// with the enable low never gets there and is refused.
#[test]
fn an_aiger_witness_replays_on_the_counter() {
    let model = root().join("tests/models/counter2_unsafe.aag");
    let g = informal_aig::load(&model).unwrap();
    let name = |l: u32| g.names.get(&informal_aig::var(l)).cloned();
    let inputs: Vec<_> = g.inputs.iter().map(|&i| name(i)).collect();
    let latches: Vec<_> = g.latches.iter().map(|l| name(l.lit)).collect();
    let named =
        informal_engine::witness::from_aiger("1\nb0\n00\n1\n1\n1\n0\n.\n", &inputs, &latches)
            .unwrap();
    let w = informal_fpv::trace::parse_witness(&named).expect("a named witness");
    assert_eq!(w.depth, 3);
    let vcd = informal_fpv::trace::vcd_from_witness(&model, &w, "q_is_3", "clk").unwrap();
    assert_eq!(value_at(&vcd, "q", 0).as_deref(), Some("00"));
    assert_eq!(value_at(&vcd, "q", 30).as_deref(), Some("11"), "{vcd}");
    let idle =
        informal_engine::witness::from_aiger("1\nb0\n00\n0\n0\n0\n0\n.\n", &inputs, &latches)
            .unwrap();
    let w = informal_fpv::trace::parse_witness(&idle).unwrap();
    assert!(informal_fpv::trace::vcd_from_witness(&model, &w, "q_is_3", "clk").is_err());
}
