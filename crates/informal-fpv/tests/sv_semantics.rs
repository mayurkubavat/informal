//! SystemVerilog known answers (tests/sv, see its README): every design
//! goes through the shell flow (svlang lowering, injected harness, the
//! in-house engines) and must give the answer IEEE 1800 gives. Skipped
//! without CaDiCaL.
//!
//! The same run records what each property's model looked like
//! (tests/sv/lowering.lock): a change that is meant to keep the lowering
//! as it is must leave that file alone, and one that changes it shows
//! which properties moved and by how much.

use informal_fpv::{compat, Flavor, Session, Shell};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct Case {
    path: PathBuf,
    top: String,
    commands: Vec<String>,
    expect: Vec<(String, String)>,
    error: Option<String>,
}

fn parse(path: &Path) -> Case {
    let text = std::fs::read_to_string(path).unwrap();
    let mut c = Case {
        path: path.to_path_buf(),
        top: path.file_stem().unwrap().to_string_lossy().into_owned(),
        commands: Vec::new(),
        expect: Vec::new(),
        error: None,
    };
    for l in text.lines() {
        if let Some(cmd) = l.strip_prefix("//%") {
            c.commands.push(cmd.trim().to_string());
        } else if let Some(e) = l.strip_prefix("//=") {
            let mut w = e.split_whitespace();
            c.expect
                .push((w.next().unwrap().to_string(), w.next().unwrap().to_string()));
        } else if let Some(e) = l.strip_prefix("//!") {
            c.error = Some(e.trim().to_string());
        }
    }
    assert!(
        c.error.is_some() || !c.expect.is_empty(),
        "{}: no //= or //! line",
        path.display()
    );
    c
}

/// FNV-1a, 64 bits: enough to see that a model changed.
fn digest(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    })
}

/// One lowering.lock line per property that got a model: its digest and
/// the counts that say how large it is.
fn lock_lines(c: &Case, sh: &Shell, work: &Path) -> Vec<String> {
    let session = sh.session.borrow();
    let mut lines = Vec::new();
    for (i, p) in session.props.iter().enumerate() {
        let Ok(model) = std::fs::read_to_string(work.join(format!("prop_{i}.btor2"))) else {
            continue;
        };
        let count = |op: &str| {
            model
                .lines()
                .filter(|l| l.split_whitespace().nth(1) == Some(op))
                .count()
        };
        lines.push(format!(
            "{} {} {:016x} lines={} state={} input={} bad={} constraint={} justice={}",
            c.top,
            p.name,
            digest(&model),
            model.lines().count(),
            count("state"),
            count("input"),
            count("bad"),
            count("constraint"),
            count("justice"),
        ));
    }
    lines
}

/// The case's lowering.lock lines, or a description of how it went wrong.
fn run(c: &Case) -> Result<Vec<String>, String> {
    let work = std::env::temp_dir().join(format!("informal-sv-{}-{}", c.top, std::process::id()));
    let sh = Shell::new(
        Session::new(informal_bmc::engines(), work.clone()),
        Flavor::Jg,
    );
    let mut script = vec![
        format!("analyze -sv {{{}}}", c.path.display()),
        format!("elaborate -top {}", c.top),
    ];
    script.extend(c.commands.iter().cloned());
    if c.error.is_none() {
        script.push("set_prove_time_limit 20s".into());
        script.push("prove -all".into());
    }
    for cmd in &script {
        if let Err(e) = sh.eval(cmd) {
            let e = e.to_string();
            return match &c.error {
                Some(want) if e.contains(want.as_str()) => Ok(Vec::new()),
                Some(want) => Err(format!("`{cmd}` failed with `{e}`, want `{want}`")),
                None => Err(format!("`{cmd}` failed: {e}")),
            };
        }
    }
    if let Some(want) = &c.error {
        return Err(format!("no error, want `{want}`"));
    }
    let got = compat::summary(&sh.session.borrow());
    for (name, want) in &c.expect {
        let status = got.iter().find(|(n, _)| n == name).map(|(_, s)| *s);
        if status != Some(want.as_str()) {
            let detail = sh
                .session
                .borrow()
                .props
                .iter()
                .find(|p| &p.name == name)
                .map(|p| format!("{:?}", p.status))
                .unwrap_or_default();
            return Err(format!("{name}: got {status:?} ({detail}), want {want}"));
        }
    }
    Ok(lock_lines(c, &sh, &work))
}

#[test]
fn sv_known_answers() {
    if informal_bmc::engines().is_empty() {
        informal_engine::tool_missing("needs CaDiCaL");
        return;
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(root().join("tests/sv"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "sv"))
        .collect();
    files.sort();
    assert!(!files.is_empty());
    let mut failures = Vec::new();
    let mut lock = Vec::new();
    for c in files.iter().map(|f| parse(f)) {
        match run(&c) {
            Ok(lines) => lock.extend(lines),
            Err(e) => failures.push(format!("{}: {e}", c.top)),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases wrong:\n  {}",
        failures.len(),
        files.len(),
        failures.join("\n  ")
    );

    // The lowering record. INFORMAL_BLESS rewrites it (scripts/check.sh
    // clears that variable, so the gate only ever compares).
    let path = root().join("tests/sv/lowering.lock");
    let got = lock.join("\n") + "\n";
    if std::env::var_os("INFORMAL_BLESS").is_some() {
        std::fs::write(&path, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    if got != want {
        let old: Vec<&str> = want.lines().collect();
        let changed: Vec<String> = lock
            .iter()
            .filter(|l| !old.contains(&l.as_str()))
            .map(|l| format!("+ {l}"))
            .chain(
                old.iter()
                    .filter(|l| !lock.iter().any(|n| n == *l))
                    .map(|l| format!("- {l}")),
            )
            .collect();
        panic!(
            "the lowering of {} properties differs from tests/sv/lowering.lock:\n  {}\n\
             If the change is meant, rewrite the file with\n  \
             INFORMAL_BLESS=1 cargo test -p informal-fpv --test sv_semantics\n\
             and say in the commit why the models changed.",
            changed.iter().filter(|l| l.starts_with('+')).count().max(1),
            changed.join("\n  ")
        );
    }
}
