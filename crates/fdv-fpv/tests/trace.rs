//! Traces end to end on the counter's failing assertion and cover: proved
//! by fdv-bmc (its named witness feeds the VCD) and by fdv-kind (no
//! witness, so the session re-runs BMC for the trace). Skipped without
//! CaDiCaL.

use fdv_fpv::{Flavor, Session, Shell, Status};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The value of `name` (a `$var` leaf) at time `t` in a VCD written by
/// fdv: the last change at or before `t`.
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
    let mut engines = fdv_bmc::engines();
    if engines.is_empty() {
        eprintln!("skipping: fdv-bmc engines need CaDiCaL");
        return None;
    }
    engines.retain(|e| e.name() == engine);
    let work = std::env::temp_dir().join(format!("fdv-trace-{tag}-{}", std::process::id()));
    // The counter script without the safe properties, which BMC alone
    // would run on until the time limit.
    let dir = root().join("tests/fpv/counter");
    let script: String = std::fs::read_to_string(dir.join("counter.jg.tcl"))
        .unwrap()
        .replace("[file join [file dirname [info script]] counter.sv]", &dir.join("counter.sv").display().to_string())
        .lines()
        .filter(|l| !["a_max ", "a_step ", "a_hold ", "a_no_wrap ", "c_10 "].iter().any(|n| l.contains(&format!("-name {n}"))))
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
            Status::Cex { depth, .. } | Status::Covered { depth, .. } => depth.expect("depth") as u64,
            other => panic!("{name}: {other:?}"),
        };
        let path = p.trace.as_ref().unwrap_or_else(|| panic!("{name} has no trace"));
        assert!(path.ends_with(&format!("{name}.vcd")), "{path}");
        let vcd = std::fs::read_to_string(path).unwrap();
        // q reaches 9 in the last cycle; the property lane rises there only.
        assert_eq!(value_at(&vcd, "q", 10 * depth).as_deref(), Some("1001"), "{vcd}");
        assert_eq!(value_at(&vcd, name, 10 * depth).as_deref(), Some("1"));
        assert_eq!(value_at(&vcd, name, 10 * depth - 10).as_deref(), Some("0"));
        assert!(vcd.contains("$scope module counter $end"));
        assert!(vcd.contains(" clk $end") && vcd.contains(" en $end") && vcd.contains(" rst_n $end"));
        assert!(vcd.trim_end().ends_with(&format!("#{}", 10 * (depth + 1))));
        // Combinational nets come from the named outputs (slang front end
        // only): `done` = (q == 9) rises with q in the last cycle.
        if s.frontend == "slang" {
            assert_eq!(value_at(&vcd, "done", 10 * depth).as_deref(), Some("1"), "{vcd}");
            assert_eq!(value_at(&vcd, "done", 10 * depth - 10).as_deref(), Some("0"));
            assert_eq!(value_at(&vcd, "nxt", 10 * depth - 10).as_deref(), Some("1001"));
            assert_eq!(value_at(&vcd, "wrap", 0).as_deref(), Some("0"));
        }
    }
    assert_eq!(get("m_en_burst").trace, None);
    let json = serde_json::to_value(&*s).unwrap();
    assert!(json["props"][0]["trace"].as_str().unwrap().ends_with("a_never9.vcd"));
    // The proof profiler's record: the one engine ran and won.
    let runs = json["props"][0]["engines"].as_array().unwrap();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0]["engine"], *engine);
    assert_eq!(runs[0]["outcome"], "won");
    assert!(runs[0]["ms"].as_u64().is_some());
}

#[test]
fn counter_traces_from_bmc_witness() {
    let Some(sh) = prove_counter("fdv-bmc", "bmc") else { return };
    check_counter_traces(&sh, "fdv-bmc");
    let _ = std::fs::remove_dir_all(&sh.session.borrow().workdir);
}

#[test]
fn counter_traces_when_the_winner_has_no_witness() {
    // fdv-kind finds the counterexamples in its base case but exports no
    // witness, so the session re-runs BMC for the trace.
    let Some(sh) = prove_counter("fdv-kind", "kind") else { return };
    check_counter_traces(&sh, "fdv-kind");
    let _ = std::fs::remove_dir_all(&sh.session.borrow().workdir);
}
