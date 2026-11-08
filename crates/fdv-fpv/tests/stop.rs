//! Stop smoke test: a prove on the counter whose only engine is a fake
//! that sleeps (`sh -c "sleep ..."`, never answering). A Stop from another
//! thread after ~1 s must end the prove promptly, leave every property
//! undetermined ("stopped"), and kill the engine's whole process group, so
//! no `sleep` is left behind. Skipped when Yosys is not installed (the
//! fallback front end; slang is used when it is built in).

use fdv_fpv::{compat, Flavor, Session, Shell};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn yosys_installed() -> bool {
    let on_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join("yosys").is_file()))
        .unwrap_or(false);
    on_path || Path::new("/tools/yosys/current/bin/yosys").is_file()
}

/// Pids of live processes whose command line contains `marker`.
fn procs_with(marker: &str) -> Vec<u32> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline"))
                .map(|c| String::from_utf8_lossy(&c).contains(marker))
                .unwrap_or(false)
        })
        // Zombies are dead already; only a running leftover counts.
        .filter(|pid| {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .map(|s| !s.rsplit(')').next().unwrap_or("").trim_start().starts_with('Z'))
                .unwrap_or(false)
        })
        .collect()
}

#[cfg(target_os = "linux")]
#[test]
fn stop_ends_a_prove_and_kills_the_engine() {
    if !yosys_installed() {
        eprintln!("skipping: needs yosys");
        return;
    }
    // A sleep length unique to this run marks its processes.
    let secs = format!("97.{}", std::process::id());
    let toml = format!(
        r#"
[[engine]]
name = "fake-sleep"
program = "sh"
args = ["-c", "sleep {secs}; echo UNSAT", "fake-engine", "{{model}}"]
formats = ["btor2"]
proves = true
finds_bugs = true
[engine.patterns]
safe = '(?m)^UNSAT$'
"#
    );
    let engines = fdv_engines_ext::parse_config(&toml).unwrap();
    let work = std::env::temp_dir().join(format!("fdv-fpv-test-stop-{}", std::process::id()));
    let session = Session::new(engines, work);
    let stop = session.stop.clone();
    let sh = Shell::new(session, Flavor::Jg);

    let rtl = root().join("tests/fpv/counter/counter.sv");
    sh.eval(&format!(
        "analyze -sv12 {}; elaborate -top counter; clock clk; reset -expression {{!rst_n}}",
        rtl.display()
    ))
    .unwrap();
    sh.eval("set_prove_time_limit 120s").unwrap();
    sh.eval("assert -name a_max {q <= 4'd9}").unwrap();
    sh.eval("assert -name a_hold {!en |=> $stable(q)}").unwrap();
    sh.eval("cover -name c_done {$rose(done)}").unwrap();

    let marker = format!("sleep {secs}");
    let t0 = Instant::now();
    let (seen_running, stopped_at) = std::thread::scope(|s| {
        let watcher = s.spawn(|| {
            // Wait until the fake engine is really running, then Stop ~1 s in.
            let mut seen = false;
            while t0.elapsed() < Duration::from_secs(60) {
                if !procs_with(&marker).is_empty() {
                    seen = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            std::thread::sleep(Duration::from_secs(1));
            stop.store(true, Ordering::SeqCst);
            (seen, t0.elapsed())
        });
        sh.eval("prove -all").unwrap();
        watcher.join().unwrap()
    });
    let took = t0.elapsed();
    assert!(seen_running, "the fake engine never started");
    assert!(
        took < stopped_at + Duration::from_secs(3),
        "prove ended {:?} after the Stop",
        took - stopped_at
    );

    let got = compat::summary(&sh.session.borrow());
    let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["a_max", "a_hold", "c_done"]);
    for (name, status) in &got {
        assert_eq!(*status, "undetermined", "{name}");
    }
    let s = sh.session.borrow();
    let reasons: Vec<String> = s
        .props
        .iter()
        .map(|p| format!("{:?}", p.status))
        .collect();
    for r in &reasons {
        assert!(r.contains("stopped"), "not stopped: {r}");
    }
    drop(s);

    // The process group was killed: no sleep from this run survives.
    std::thread::sleep(Duration::from_millis(200));
    let left = procs_with(&marker);
    assert!(left.is_empty(), "engine processes left: {left:?}");
}
