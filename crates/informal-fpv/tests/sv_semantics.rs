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
        // INFORMAL_KEEP_MODELS=<dir> keeps a copy of every model, to compare
        // two lowerings with scripts/btor2_equiv.py.
        if let Some(dir) = std::env::var_os("INFORMAL_KEEP_MODELS") {
            let dir = PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{}.{}.btor2", c.top, p.name)), &model).unwrap();
        }
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

/// The session's working directory for a design, one per test process.
fn work_dir(top: &str) -> PathBuf {
    std::env::temp_dir().join(format!("informal-sv-{top}-{}", std::process::id()))
}

/// The case's lowering.lock lines, or a description of how it went wrong.
fn run(c: &Case) -> Result<Vec<String>, String> {
    let work = work_dir(&c.top);
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

/// The designs run one at a time inside one test process (each starts the
/// engines); under nextest every test is its own process and the `engines`
/// test group does the same job (.config/nextest.toml).
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

const BLESS: &str = "If the change is meant, rewrite the file with\n  \
                     INFORMAL_BLESS=1 cargo test -p informal-fpv --test sv_semantics\n\
                     and say in the commit why the models changed.";

fn lock_path() -> PathBuf {
    root().join("tests/sv/lowering.lock")
}

/// tests/sv/lowering.lock with `edit` applied to its lines grouped by
/// design, written back in design order. Several test processes may do
/// this at once, so the file is locked for the read and the write.
fn rewrite_lock(edit: impl FnOnce(&mut std::collections::BTreeMap<String, Vec<String>>)) {
    use std::io::{Read, Seek, Write};
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path())
        .unwrap();
    f.lock().unwrap();
    let mut text = String::new();
    f.read_to_string(&mut text).unwrap();
    let mut by_design = std::collections::BTreeMap::<String, Vec<String>>::new();
    for l in text.lines() {
        let design = l.split(' ').next().unwrap().to_string();
        by_design.entry(design).or_default().push(l.to_string());
    }
    edit(&mut by_design);
    let out: String = by_design
        .values()
        .flatten()
        .map(|l| format!("{l}\n"))
        .collect();
    f.rewind().unwrap();
    f.set_len(0).unwrap();
    f.write_all(out.as_bytes()).unwrap();
}

/// One design: its known answers, then its lines of the lowering record.
/// INFORMAL_BLESS rewrites those lines (scripts/check.sh clears that
/// variable, so the gate only ever compares).
fn case(name: &str) {
    if informal_bmc::engines().is_empty() {
        informal_engine::tool_missing("needs CaDiCaL");
        return;
    }
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let c = parse(&root().join("tests/sv").join(format!("{name}.sv")));
    let got = run(&c).unwrap_or_else(|e| panic!("{name}: {e}"));
    // A passing case leaves nothing behind (the directory is in /tmp, often
    // a RAM disk); a failing one keeps its models for a look.
    let _ = std::fs::remove_dir_all(work_dir(name));
    if std::env::var_os("INFORMAL_BLESS").is_some() {
        rewrite_lock(|by_design| {
            by_design.remove(name);
            if !got.is_empty() {
                by_design.insert(name.to_string(), got.clone());
            }
        });
        return;
    }
    let text = std::fs::read_to_string(lock_path()).unwrap_or_default();
    let want: Vec<&str> = text
        .lines()
        .filter(|l| l.split(' ').next() == Some(name))
        .collect();
    if got != want {
        let changed: Vec<String> = got
            .iter()
            .filter(|l| !want.contains(&l.as_str()))
            .map(|l| format!("+ {l}"))
            .chain(
                want.iter()
                    .filter(|l| !got.iter().any(|n| n == *l))
                    .map(|l| format!("- {l}")),
            )
            .collect();
        panic!(
            "{name}: the models differ from tests/sv/lowering.lock:\n  {}\n{BLESS}",
            changed.join("\n  ")
        );
    }
}

// `fn sv_<design>() { case("<design>") }` for every tests/sv/*.sv (build.rs).
include!(concat!(env!("OUT_DIR"), "/sv_cases.rs"));

/// The record names no design that is gone (INFORMAL_BLESS drops them).
#[test]
fn lowering_lock_has_no_stale_design() {
    let exists = |design: &str| {
        root()
            .join("tests/sv")
            .join(format!("{design}.sv"))
            .is_file()
    };
    if std::env::var_os("INFORMAL_BLESS").is_some() {
        rewrite_lock(|by_design| by_design.retain(|design, _| exists(design)));
        return;
    }
    let text = std::fs::read_to_string(lock_path()).unwrap_or_default();
    let stale: std::collections::BTreeSet<&str> = text
        .lines()
        .map(|l| l.split(' ').next().unwrap())
        .filter(|d| !exists(d))
        .collect();
    assert!(
        stale.is_empty(),
        "tests/sv/lowering.lock has lines for designs that are gone: {stale:?}\n{BLESS}"
    );
}

/// The design with its command properties written in the RTL instead: the
/// same text as `rf_<name>: assert property (...)` inside the top module,
/// under a default clocking and a default disable for the script's clock
/// and reset. `None` when the script has more than a plain clock, a reset
/// and assert/cover commands, or expects an error.
fn rtl_form(c: &Case) -> Option<(String, Vec<(String, String)>)> {
    let text = std::fs::read_to_string(&c.path).unwrap();
    let mut clock = None;
    let mut reset = None;
    let mut props = Vec::new();
    for cmd in &c.commands {
        let (word, rest) = cmd.split_once(' ').unwrap_or((cmd.as_str(), ""));
        match word {
            "clock" if clock.is_none() && !rest.contains(' ') => clock = Some(rest.to_string()),
            "reset" if reset.is_none() => {
                let e = rest.strip_prefix("-expression {")?.strip_suffix('}')?;
                reset = Some(e.to_string());
            }
            "assert" | "cover" => {
                let rest = rest.strip_prefix("-name ")?;
                let (name, expr) = rest.split_once(' ')?;
                let expr = expr.strip_prefix('{')?.strip_suffix('}')?;
                props.push((word, name.to_string(), expr.to_string()));
            }
            _ => return None,
        }
    }
    let clock = clock?;
    if c.error.is_some() || props.is_empty() {
        return None;
    }
    // Every command property must have a plain expected status.
    let mut expect = Vec::new();
    for (_, name, _) in &props {
        let (_, want) = c.expect.iter().find(|(n, _)| n == name)?;
        if want == "error" {
            return None;
        }
        expect.push((format!("{}.rf_{name}", c.top), want.clone()));
    }
    // The top module's own header, not a module whose name starts the same.
    let header = format!("module {}", c.top);
    let start = text.match_indices(&header).map(|(i, _)| i).find(|i| {
        let after = text[i + header.len()..].chars().next();
        !after.is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
    })?;
    let end = start + text[start..].find("endmodule")?;
    let mut add = String::new();
    if !text.contains("default clocking") {
        add += &format!("  default clocking @(posedge {clock}); endclocking\n");
    }
    if let (Some(r), false) = (&reset, text.contains("default disable")) {
        add += &format!("  default disable iff ({r});\n");
    }
    for (kind, name, expr) in &props {
        add += &format!("  rf_{name}: {kind} property ({expr});\n");
    }
    Some((format!("{}{add}{}", &text[..end], &text[end..]), expect))
}

/// A property means the same written in the RTL as given by command: the
/// two forms take different paths into the model (the first is lowered with
/// the design, the second is bound to it afterwards), so each design that
/// can be rewritten is run in its RTL form and must give the same answers.
#[test]
fn rtl_form_gives_the_same_answers() {
    if informal_bmc::engines().is_empty() {
        informal_engine::tool_missing("needs CaDiCaL");
        return;
    }
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut files: Vec<PathBuf> = std::fs::read_dir(root().join("tests/sv"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "sv"))
        .collect();
    files.sort();
    let mut ran = 0;
    let mut wrong = Vec::new();
    for f in &files {
        let c = parse(f);
        let Some((text, expect)) = rtl_form(&c) else {
            continue;
        };
        let dir = work_dir(&format!("rtlform-{}", c.top));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.sv", c.top));
        std::fs::write(&path, text).unwrap();
        // The commands stay (the script's own properties run again beside
        // the RTL ones); the expectations are those of the RTL names.
        let variant = Case {
            path,
            top: c.top.clone(),
            commands: c.commands.clone(),
            expect,
            error: None,
        };
        ran += 1;
        match run(&variant) {
            Ok(_) => {
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::remove_dir_all(work_dir(&c.top));
            }
            Err(e) => wrong.push(format!("{}: {e}", c.top)),
        }
    }
    // The rewrite must keep covering the designs it covers today: a design
    // dropping out silently would be a test that no longer runs.
    assert!(
        ran >= 26,
        "only {ran} designs could be put in RTL form, 26 before"
    );
    assert!(
        wrong.is_empty(),
        "{} of {ran} designs answer differently in RTL form:\n  {}",
        wrong.len(),
        wrong.join("\n  ")
    );
}
