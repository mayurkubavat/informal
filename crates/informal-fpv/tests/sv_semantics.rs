//! SystemVerilog known answers (tests/sv, see its README): every design
//! goes through the shell flow (slang lowering, injected harness, the
//! in-house engines) and must give the answer IEEE 1800 gives. Skipped
//! without slang or CaDiCaL.

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

/// The case's outcome, or a description of how it went wrong.
fn run(c: &Case) -> Result<(), String> {
    let work = std::env::temp_dir().join(format!("informal-sv-{}-{}", c.top, std::process::id()));
    let sh = Shell::new(Session::new(informal_bmc::engines(), work), Flavor::Jg);
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
                Some(want) if e.contains(want.as_str()) => Ok(()),
                Some(want) => Err(format!("`{cmd}` failed with `{e}`, want `{want}`")),
                None => Err(format!("`{cmd}` failed: {e}")),
            };
        }
    }
    if let Some(want) = &c.error {
        return Err(format!("no error, want `{want}`"));
    }
    // The Yosys fallback is not what this suite is about.
    let frontend = sh.session.borrow().frontend.clone();
    if frontend == "yosys" {
        return Err(format!("lowered by {frontend}, not by a SystemVerilog front end"));
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
    Ok(())
}

#[test]
fn sv_known_answers() {
    if !informal_slang::available() || informal_bmc::engines().is_empty() {
        eprintln!("skipping: needs slang and CaDiCaL");
        return;
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(root().join("tests/sv"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "sv"))
        .collect();
    files.sort();
    assert!(!files.is_empty());
    let failures: Vec<String> = files
        .iter()
        .map(|f| parse(f))
        .filter_map(|c| run(&c).err().map(|e| format!("{}: {e}", c.top)))
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} cases wrong:\n  {}",
        failures.len(),
        files.len(),
        failures.join("\n  ")
    );
}
