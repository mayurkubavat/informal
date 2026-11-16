//! `informal check --certificate`: the files it writes are what the outside
//! checkers read, and they hold up under this repository's own checks. The
//! checkers themselves (aigsim, Certifaiger) run in
//! `scripts/certificate_check.py`, not here. Skipped without CaDiCaL (no
//! in-house engines).

use informal_aig::{aiger, Aig};
use informal_bmc::certificate::{summaries, Summary};
use informal_bmc::certify;
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A new, empty directory for one test.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("informal-cert-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// Run `informal check` on a model of tests/models with one engine;
/// returns the exit status, stdout and the summary it left (if any).
fn check(
    model: &str,
    engine: &str,
    dir: &Path,
    extra: &[&str],
    env: &[(&str, &str)],
) -> (Option<i32>, String, Option<Summary>) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_informal"));
    c.current_dir(root()).args(extra).args([
        "check",
        &format!("tests/models/{model}"),
        "--timeout",
        "30",
        "--only",
        engine,
        "--certificate",
    ]);
    c.arg(dir);
    for (k, v) in env {
        c.env(k, v);
    }
    let o = c.output().unwrap();
    let summary = summaries(dir).ok().and_then(|all| {
        all.into_iter()
            .map(|(_, s)| s)
            .find(|s| s.model.ends_with(model))
    });
    (
        o.status.code(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        summary,
    )
}

fn have_engines() -> bool {
    let d = scratch("probe");
    let (code, out, _) = check("toggle_unsafe.aag", "informal-bmc", &d, &[], &[]);
    let _ = std::fs::remove_dir_all(&d);
    if code != Some(10) {
        informal_engine::tool_missing(&format!(
            "no in-house engines in this build ({code:?}: {out})"
        ));
        return false;
    }
    true
}

fn model(name: &str) -> Aig {
    informal_aig::load(&root().join("tests/models").join(name)).unwrap()
}

fn sat() -> Box<dyn informal_sat::Sat> {
    informal_sat::best()
}

/// The witness circuit in `file`: its one bad output is unreachable by
/// plain 1-induction (Certifaiger's base and inductive conditions), and
/// its symbol table maps inputs and latches to literals of the model.
fn circuit_holds(file: &Path, g: &Aig) {
    let bytes = std::fs::read(file).unwrap();
    let w = aiger::read(&bytes).unwrap();
    assert_eq!(w.bads.len(), 1, "{}", file.display());
    assert_eq!(
        certify::check_kind_paths(&w, &[], false, 1, &sat, &|| false),
        Ok(0),
        "{}",
        file.display()
    );
    let text = String::from_utf8_lossy(&bytes);
    let mut mapped = 0;
    for line in text.lines() {
        let Some((tag, lit)) = line.split_once(" = ") else {
            continue;
        };
        let lit: u32 = lit.parse().unwrap();
        // The first symbol follows the binary gates on its line.
        let tag = &tag[tag.rfind(['i', 'l']).unwrap()..];
        match tag.as_bytes()[0] {
            b'i' => assert!(g.inputs.contains(&lit), "{line}"),
            b'l' => assert!(g.is_latch(lit), "{line}"),
            _ => panic!("{line}"),
        }
        mapped += 1;
    }
    assert!(mapped > 0 && text.contains("\nc\nWITNESS b0 "));
}

/// The AIGER witness in `file` reaches a bad output of `g` when replayed.
fn trace_replays(file: &Path, g: &Aig) -> usize {
    let text = std::fs::read_to_string(file).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "1");
    assert!(lines[1].starts_with('b'));
    assert_eq!(*lines.last().unwrap(), ".");
    let row = |s: &str| s.bytes().map(|c| c == b'1').collect::<Vec<bool>>();
    let t = certify::Trace {
        init: row(lines[2]),
        inputs: lines[3..lines.len() - 1].iter().map(|l| row(l)).collect(),
    };
    let depth = t.inputs.len() - 1;
    assert_eq!(
        certify::replay_unsafe_bad(g, depth, &t).map(|b| format!("b{b}")),
        Ok(lines[1].to_string())
    );
    depth
}

#[test]
fn every_kind_of_certificate_is_written_and_holds() {
    if !have_engines() {
        return;
    }
    let d = scratch("kinds");
    // The k-induction engines fold a free-running clock too, and a
    // folded k-induction proof has no outside certificate kind yet (it is
    // certified in the tool only): clocked_safe is proved unfolded here,
    // and folded below.
    let unfolded: &[(&str, &str)] = &[("INFORMAL_FOLD", "0")];
    for (name, engine, kind, env) in [
        ("stuck_latch_safe.aag", "informal-ic3", "invariant", &[][..]),
        ("delay_safe.aag", "informal-ic3-mt", "invariant", &[]),
        ("delay_safe.aag", "informal-kind", "k-induction", &[]),
        ("clocked_safe.aag", "informal-kind", "k-induction", unfolded),
        ("clocked_safe.aag", "informal-ic3", "two-step", &[]),
        ("split_safe.aag", "informal-kind-ic3", "invariant", &[]),
    ] {
        let dir = d.join(format!("{engine}-{}", env.len()));
        let (code, out, s) = check(name, engine, &dir, &[], env);
        assert_eq!(code, Some(20), "{name} {engine}: {out}");
        let s = s.unwrap();
        assert_eq!(
            (s.verdict.as_str(), s.evidence.as_str(), s.engine.as_str()),
            ("safe", kind, engine),
            "{name}"
        );
        assert_eq!(s.checker.as_deref(), Some("certifaiger"));
        // Only a k-induction proof has a depth; the delay line needs 3.
        assert_eq!(s.k.is_some(), kind == "k-induction", "{name}");
        if (name, engine) == ("delay_safe.aag", "informal-kind") {
            assert_eq!(s.k, Some(3));
        }
        let file = s.file.unwrap();
        assert_eq!(file, format!("{name}.cert.aig"));
        assert!(
            out.contains(&format!("certificate: {file} ({kind}, for certifaiger)")),
            "{out}"
        );
        circuit_holds(&dir.join(file), &model(name));
    }
    let dir = d.join("informal-kind-folded");
    let (code, out, s) = check("clocked_safe.aag", "informal-kind", &dir, &[], &[]);
    assert_eq!(code, Some(20), "{out}");
    let s = s.unwrap();
    assert_eq!((s.verdict.as_str(), s.evidence.as_str()), ("safe", "none"));
    for (name, engine, depth) in [
        ("counter2_unsafe.aag", "informal-bmc", 3),
        ("counter2_unsafe.aag", "informal-ic3", 3),
        ("split_unsafe.aag", "informal-kind", 2),
        ("toggle_unsafe.aag", "informal-ic3-mt", 1),
    ] {
        let dir = d.join(engine);
        let (code, out, s) = check(name, engine, &dir, &[], &[]);
        assert_eq!(code, Some(10), "{name} {engine}: {out}");
        let s = s.unwrap();
        assert_eq!(
            (s.verdict.as_str(), s.evidence.as_str()),
            ("unsafe", "trace")
        );
        assert_eq!(s.checker.as_deref(), Some("aigsim"));
        let file = dir.join(s.file.unwrap());
        assert_eq!(trace_replays(&file, &model(name)), depth, "{name} {engine}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// The clocked model has a free-running clock, so IC3 proves it two steps
/// at a time; without folding the same engine gives a one-step invariant.
#[test]
fn folding_decides_the_kind_of_invariant() {
    if !have_engines() {
        return;
    }
    let d = scratch("fold");
    let (code, out, s) = check(
        "clocked_safe.aag",
        "informal-ic3",
        &d,
        &[],
        &[("INFORMAL_FOLD", "0")],
    );
    assert_eq!(code, Some(20), "{out}");
    let s = s.unwrap();
    assert_eq!(s.evidence, "invariant");
    circuit_holds(&d.join(s.file.unwrap()), &model("clocked_safe.aag"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn isolated_engines_hand_their_certificates_over() {
    if !have_engines() {
        return;
    }
    let d = scratch("isolate");
    for (name, engine, ext) in [
        ("clocked_safe.aag", "informal-ic3", "cert.aig"),
        ("delay_safe.aag", "informal-kind", "cert.aig"),
        ("counter2_unsafe.aag", "informal-bmc", "trace"),
    ] {
        let (a, b) = (d.join("in"), d.join("out"));
        let (code, out, _) = check(name, engine, &a, &[], &[]);
        let (code_iso, out_iso, _) = check(name, engine, &b, &["--isolate"], &[]);
        assert_eq!(code, code_iso, "{out}\n{out_iso}");
        for f in [format!("{name}.{ext}"), format!("{name}.cert.json")] {
            assert_eq!(
                std::fs::read(a.join(&f)).unwrap(),
                std::fs::read(b.join(&f)).unwrap(),
                "{f}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_btor2_model_is_certified_at_bit_level_and_no_certificate_has_a_reason() {
    if !have_engines() {
        return;
    }
    let d = scratch("btor2");
    // The counter reaches 7 after seven steps: the trace is for the
    // bit-blasted model, which is written next to it.
    let (code, out, s) = check("counter.btor2", "informal-bmc", &d, &[], &[]);
    assert_eq!(code, Some(10), "{out}");
    let s = s.unwrap();
    assert_eq!(
        s.bit_level_model.as_deref(),
        Some("counter.btor2.model.aig")
    );
    assert!(s
        .note
        .as_deref()
        .unwrap()
        .contains("bit-blaster is trusted"));
    let g = aiger::read_file(&d.join("counter.btor2.model.aig")).unwrap();
    assert_eq!(trace_replays(&d.join(s.file.unwrap()), &g), 7);
    // Without the independent check there is nothing to write, and the
    // summary says why.
    let e = d.join("none");
    let (code, out, s) = check(
        "counter2_unsafe.aag",
        "informal-bmc",
        &e,
        &[],
        &[("INFORMAL_NO_CERT", "1")],
    );
    assert_eq!(code, Some(10), "{out}");
    let s = s.unwrap();
    assert_eq!((s.file, s.checker), (None, None));
    assert_eq!(s.evidence, "none");
    assert!(out.contains("certificate: none (informal-bmc gives no certificate)"));
    let _ = std::fs::remove_dir_all(&d);
}

/// Two models with the same file name from different directories keep
/// their own certificates in one directory; checking one of them again
/// reuses its own names.
#[test]
fn same_file_name_from_two_directories() {
    if !have_engines() {
        return;
    }
    let d = scratch("same-name");
    let src = scratch("same-name-models");
    for (sub, from) in [("a", "toggle_unsafe.aag"), ("b", "stuck_latch_safe.aag")] {
        std::fs::create_dir_all(src.join(sub)).unwrap();
        std::fs::copy(
            root().join("tests/models").join(from),
            src.join(sub).join("same.aag"),
        )
        .unwrap();
    }
    let run = |sub: &str| {
        Command::new(env!("CARGO_BIN_EXE_informal"))
            .current_dir(root())
            .args(["check"])
            .arg(src.join(sub).join("same.aag"))
            .args(["--timeout", "30", "--certificate"])
            .arg(&d)
            .output()
            .unwrap()
            .status
            .code()
    };
    assert_eq!(run("a"), Some(10));
    assert_eq!(run("b"), Some(20));
    assert_eq!(run("a"), Some(10));
    let all: Vec<Summary> = summaries(&d).unwrap().into_iter().map(|(_, s)| s).collect();
    assert_eq!(all.len(), 2, "{all:?}");
    let a = all
        .iter()
        .find(|s| s.model.ends_with("a/same.aag"))
        .expect("a's summary");
    let b = all
        .iter()
        .find(|s| s.model.ends_with("b/same.aag"))
        .expect("b's summary");
    assert_eq!((a.verdict.as_str(), b.verdict.as_str()), ("unsafe", "safe"));
    assert_ne!(a.file, b.file);
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&src);
}
