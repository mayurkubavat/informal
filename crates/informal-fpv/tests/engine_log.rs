//! Engine output reaches the session's progress sink as "engine-log" lines
//! while a prove runs: a fake external engine prints a few lines (one on
//! stderr) and answers. Skipped when Yosys is not installed.

use informal_fpv::{EngineLine, Flavor, ProveProgress, ProveSink, Session, Shell};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn yosys_installed() -> bool {
    let on_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join("yosys").is_file()))
        .unwrap_or(false);
    on_path || Path::new("/tools/yosys/current/bin/yosys").is_file()
}

#[derive(Default)]
struct Rec(Mutex<Vec<EngineLine>>);
impl ProveSink for Rec {
    fn progress(&self, _: &ProveProgress) {}
    fn engine_log(&self, l: &EngineLine) {
        self.0.lock().unwrap().push(l.clone());
    }
}

#[test]
fn engine_output_is_streamed_to_the_sink() {
    if !yosys_installed() {
        eprintln!("skipping: needs yosys");
        return;
    }
    let toml = r#"
[[engine]]
name = "fake-talk"
program = "sh"
args = ["-c", "echo reading $1; echo 'note: frame 1' >&2; echo UNSAT", "fake-engine", "{model}"]
formats = ["btor2"]
proves = true
finds_bugs = true
[engine.patterns]
safe = '(?m)^UNSAT$'
"#;
    let engines = informal_engines_ext::parse_config(toml).unwrap();
    let work = std::env::temp_dir().join(format!("informal-fpv-test-log-{}", std::process::id()));
    let mut session = Session::new(engines, work);
    let rec = Arc::new(Rec::default());
    session.sink = Some(rec.clone());
    let sh = Shell::new(session, Flavor::Jg);
    let rtl = root().join("tests/fpv/counter/counter.sv");
    sh.eval(&format!(
        "analyze -sv12 {}; elaborate -top counter; clock clk; reset -expression {{!rst_n}}",
        rtl.display()
    ))
    .unwrap();
    sh.eval("assert -name a_max {q <= 4'd9}").unwrap();
    sh.eval("prove -all").unwrap();
    let lines = rec.0.lock().unwrap().clone();
    let texts: Vec<(&str, &str, bool)> = lines
        .iter()
        .map(|l| {
            (
                l.prop.as_str(),
                l.text.split(' ').next().unwrap_or(""),
                l.err,
            )
        })
        .collect();
    assert!(texts.contains(&("a_max", "reading", false)), "{lines:?}");
    assert!(texts.contains(&("a_max", "note:", true)), "{lines:?}");
    assert!(texts.contains(&("a_max", "UNSAT", false)), "{lines:?}");
    assert!(lines.iter().all(|l| l.engine == "fake-talk"));
}
