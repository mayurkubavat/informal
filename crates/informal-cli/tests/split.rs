//! `informal split`: a property that is an OR of terms is proved term by
//! term and then on the model as given. Skipped without CaDiCaL (no
//! in-house engines).

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn split(model: &str, extra: &[&str]) -> (Option<i32>, String, String) {
    let work = std::env::temp_dir().join(format!("informal-split-test-{}", std::process::id()));
    let o = Command::new(env!("CARGO_BIN_EXE_informal"))
        .current_dir(root())
        .env_remove("INFORMAL_INV_STORE")
        .args([
            "split",
            model,
            "--timeout",
            "30",
            "--slice",
            "5",
            "--workdir",
        ])
        .arg(&work)
        .args(extra)
        .output()
        .unwrap();
    (
        o.status.code(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn have_engines() -> bool {
    let o = Command::new(env!("CARGO_BIN_EXE_informal"))
        .current_dir(root())
        .args([
            "check",
            "tests/models/counter2_unsafe.aag",
            "--timeout",
            "20",
            "--only",
            "informal-bmc",
        ])
        .output()
        .unwrap();
    o.status.code() == Some(10)
}

#[test]
fn a_safe_or_is_proved_term_by_term() {
    if !have_engines() {
        informal_engine::tool_missing("no in-house engines in this build");
        return;
    }
    let (code, out, err) = split("tests/models/split_safe.aag", &[]);
    assert_eq!(code, Some(20), "{out}{err}");
    assert!(
        out.contains("SAFE (by informal-ic3, 3 terms)"),
        "{out}{err}"
    );
    assert_eq!(err.matches(" safe ").count(), 3, "{err}");
}

#[test]
fn a_reachable_term_gives_a_counterexample_on_the_original() {
    if !have_engines() {
        informal_engine::tool_missing("no in-house engines in this build");
        return;
    }
    let (code, out, err) = split("tests/models/split_unsafe.aag", &[]);
    assert_eq!(code, Some(10), "{out}{err}");
    assert!(
        out.contains("UNSAFE (by informal-bmc") && out.contains("depth 2"),
        "{out}{err}"
    );
}

#[test]
fn write_only_lists_the_term_models_and_a_plain_property_is_refused() {
    let (code, out, err) = split("tests/models/split_safe.aag", &["--write-only"]);
    assert_eq!(code, Some(0), "{out}{err}");
    let files: Vec<&str> = out.lines().collect();
    assert_eq!(files.len(), 3, "{out}");
    for f in files {
        assert!(Path::new(f).exists() || root().join(f).exists(), "{f}");
        let _ = std::fs::remove_file(f);
    }
    let (code, _, err) = split("tests/models/stuck_latch_safe.aag", &[]);
    assert_ne!(code, Some(20));
    assert!(err.contains("nothing to split"), "{err}");
}
