//! `--isolate`: in-house engines as child processes of the same binary
//! give the verdicts, depth and witness the in-process ones give.
//! Skipped without CaDiCaL (no in-house engines).

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn check(model: &str, engine: &str, isolate: bool) -> (Option<i32>, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_informal"));
    if isolate {
        c.arg("--isolate");
    }
    let o = c
        .current_dir(root())
        .args(["check", model, "--timeout", "20", "--only", engine])
        .output()
        .unwrap();
    (
        o.status.code(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
    )
}

#[test]
fn isolated_engines_agree_with_in_process_ones() {
    let (code, out) = check("tests/models/counter2_unsafe.aag", "informal-bmc", false);
    if code != Some(10) {
        informal_engine::tool_missing(&format!(
            "no in-house engines in this build ({code:?}: {out})"
        ));
        return;
    }
    let verdict = |s: &str| {
        s.lines()
            .filter(|l| !l.trim_start().starts_with("informal-"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    // Unsafe, with the same depth and named witness.
    let (code, iso) = check("tests/models/counter2_unsafe.aag", "informal-bmc", true);
    assert_eq!(code, Some(10), "{iso}");
    assert_eq!(verdict(&iso), verdict(&out));
    assert!(iso.contains("UNSAFE (by informal-bmc, depth 3)") && iso.contains("@3 counter.en=0"));
    // Safe (certified inside the child).
    let (code, iso) = check(
        "tests/models/stuck_latch_safe.aag",
        "informal-kind-ic3",
        true,
    );
    assert_eq!(code, Some(20), "{iso}");
    assert!(iso.contains("SAFE (by informal-kind-ic3)"), "{iso}");
}

/// The child side on its own: JSON lines, the last one the result.
#[test]
fn engine_subcommand_speaks_the_line_protocol() {
    let o = Command::new(env!("CARGO_BIN_EXE_informal"))
        .current_dir(root())
        .args(["engine", "informal-bmc", "tests/models/counter2_unsafe.aag"])
        .output()
        .unwrap();
    if !o.status.success() {
        informal_engine::tool_missing("no in-house engines in this build");
        return;
    }
    let out = String::from_utf8_lossy(&o.stdout);
    let last: serde_json::Value = serde_json::from_str(out.lines().last().unwrap()).unwrap();
    assert_eq!(last["result"]["verdict"], "unsafe");
    assert_eq!(last["result"]["depth"], 3);
}
