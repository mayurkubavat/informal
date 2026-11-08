//! Formal DV desktop app: a Tauri window over one fdv shell.
//!
//! The shell (Tcl interpreter plus FPV session) lives on its own thread
//! (`fdv_fpv::Worker`); each command below forwards one request to it and
//! returns the Tcl result together with the session, which the property
//! table redraws from. Commands are `async` so a long prove never blocks
//! the window.
//!
//! While a prove runs, the session's progress sink emits "prove-progress"
//! and "console-line" events to the window, and `shell_stop` sets the
//! shell's stop flag directly (the shell thread is busy with the prove).
//! The event contract is in docs/ui/stop-progress.md. External engines'
//! stdout/stderr lines arrive as "engine-log" events (the Log tab).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use fdv_fpv::{EngineLine, Flavor, ProveProgress, ProveSink, Reply, Request, Session, Worker};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{AppHandle, Emitter, State};

/// Set once the window exists; events before that (a start-up script's
/// prove can begin first) are dropped.
static APP: OnceLock<AppHandle> = OnceLock::new();

/// Payload of "console-line".
#[derive(Clone, Serialize)]
struct ConsoleLine<'a> {
    text: &'a str,
    kind: &'static str,
}

/// Forwards prove progress from the shell to the window as Tauri events.
struct EventSink;

impl ProveSink for EventSink {
    fn progress(&self, p: &ProveProgress) {
        if let Some(app) = APP.get() {
            let _ = app.emit("prove-progress", p);
        }
    }
    fn line(&self, text: &str, err: bool) {
        if let Some(app) = APP.get() {
            let kind = if err { "err" } else { "out" };
            let _ = app.emit("console-line", ConsoleLine { text, kind });
        }
    }
    fn engine_log(&self, l: &EngineLine) {
        if let Some(app) = APP.get() {
            let _ = app.emit("engine-log", l);
        }
    }
}

/// `config/engines.toml` from `FDV_ENGINES`, or the nearest one above the
/// working directory (the repo root when run with `npm run tauri dev`).
fn engines_config() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FDV_ENGINES") {
        return Some(p.into());
    }
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let c = dir.join("config/engines.toml");
        if c.is_file() {
            return Some(c);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// The design file list and property names from the last reply, and the
/// active flavor, for `read_text` and `shell_complete` (which must not
/// queue behind a running prove on the shell thread).
static DESIGN_FILES: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static PROP_NAMES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static FLAVOR: Mutex<Flavor> = Mutex::new(Flavor::Jg);

async fn ask(w: &Worker, req: Request) -> Reply {
    let w = w.clone();
    let r = tauri::async_runtime::spawn_blocking(move || w.call(req))
        .await
        .unwrap_or_else(|e| Reply { ok: false, result: e.to_string(), session: serde_json::Value::Null });
    if let Some(a) = r.session["design"]["files"].as_array() {
        let files = a.iter().filter_map(|f| f.as_str().map(PathBuf::from)).collect();
        if let Ok(mut g) = DESIGN_FILES.lock() {
            *g = files;
        }
    }
    if let Some(a) = r.session["props"].as_array() {
        let names = a.iter().filter_map(|p| p["name"].as_str().map(String::from)).collect();
        if let Ok(mut g) = PROP_NAMES.lock() {
            *g = names;
        }
    }
    r
}

#[tauri::command]
async fn snapshot(w: State<'_, Worker>) -> Result<Reply, String> {
    Ok(ask(&w, Request::Snapshot).await)
}

#[tauri::command]
async fn shell_eval(w: State<'_, Worker>, script: String) -> Result<Reply, String> {
    Ok(ask(&w, Request::Eval(script)).await)
}

#[tauri::command]
async fn shell_source(w: State<'_, Worker>, path: String) -> Result<Reply, String> {
    Ok(ask(&w, Request::Source(path.into())).await)
}

#[tauri::command]
async fn shell_flavor(w: State<'_, Worker>, flavor: String) -> Result<Reply, String> {
    let f = Flavor::parse(&flavor).ok_or_else(|| format!("unknown flavor {flavor}"))?;
    if let Ok(mut g) = FLAVOR.lock() {
        *g = f;
    }
    Ok(ask(&w, Request::Flavor(f)).await)
}

/// Console Tab completion: the active flavor's command names, then
/// property names, starting with `prefix` (fdv_fpv::compat::complete).
/// Answered here from the last reply, so it works while a prove runs.
#[tauri::command]
fn shell_complete(prefix: String) -> Vec<String> {
    let flavor = FLAVOR.lock().map(|g| *g).unwrap_or(Flavor::Jg);
    let props = PROP_NAMES.lock().map(|g| g.clone()).unwrap_or_default();
    fdv_fpv::compat::complete(flavor, &props, &prefix)
}

/// Stop the running prove. Not sent through the shell thread, which is
/// busy proving: this sets the shared stop flag and returns at once. The
/// prove's own `shell_eval` then returns, after a "stopped" event.
#[tauri::command]
async fn shell_stop(w: State<'_, Worker>) -> Result<Reply, String> {
    w.stop();
    Ok(Reply { ok: true, result: String::new(), session: serde_json::Value::Null })
}

/// The session's work directory, relative to the repo root the app runs
/// from: generated models and trace VCDs land here.
const WORKDIR: &str = "fdv_work";

/// A trace VCD for the trace tab (`Property.trace`). Only `.vcd` files
/// inside the work directory are served, whatever the path says.
#[tauri::command]
fn read_trace(path: String) -> Result<String, String> {
    trace_in(Path::new(WORKDIR), Path::new(&path))
}

fn trace_in(workdir: &Path, path: &Path) -> Result<String, String> {
    let shown = path.display();
    let root = std::fs::canonicalize(workdir).map_err(|e| format!("{}: {e}", workdir.display()))?;
    let p = std::fs::canonicalize(path).map_err(|e| format!("{shown}: {e}"))?;
    if !p.starts_with(&root) || p.extension().is_none_or(|e| e != "vcd") {
        return Err(format!("{shown}: not a trace in {}", root.display()));
    }
    std::fs::read_to_string(&p).map_err(|e| format!("{shown}: {e}"))
}

/// Design source or script text for the source view (a property's
/// `source` file, the top design file). Only files under the app's working
/// directory (the repo root, which holds the work directory) or in the
/// session's design file list are served.
#[tauri::command]
fn read_text(path: String) -> Result<String, String> {
    let files = DESIGN_FILES.lock().map(|g| g.clone()).unwrap_or_default();
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    text_in(&root, &files, Path::new(&path))
}

fn text_in(root: &Path, files: &[PathBuf], path: &Path) -> Result<String, String> {
    let shown = path.display();
    let p = std::fs::canonicalize(path).map_err(|e| format!("{shown}: {e}"))?;
    let root = std::fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let listed = files
        .iter()
        .any(|f| std::fs::canonicalize(f).is_ok_and(|f| f == p));
    if !p.starts_with(&root) && !listed {
        return Err(format!("{shown}: not under {} or in the design files", root.display()));
    }
    std::fs::read_to_string(&p).map_err(|e| format!("{shown}: {e}"))
}

fn main() {
    // Run from the repo root (the directory holding config/engines.toml), so
    // script paths read as they do for `fdv shell` and generated models land
    // in its fdv_work/ rather than under src-tauri, which the dev watcher
    // would take as a source change.
    let config = engines_config();
    if let Some(root) = config.as_ref().and_then(|c| c.parent()?.parent()) {
        let _ = std::env::set_current_dir(root);
    }
    let config = config.and_then(|c| std::fs::canonicalize(c).ok());
    // `fdv-app script.tcl` sources a script at start, flavor detected from it.
    let script = std::env::args().nth(1).map(PathBuf::from);
    let flavor = script
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map_or(Flavor::Jg, |t| fdv_fpv::detect_flavor(&t));
    if let Ok(mut g) = FLAVOR.lock() {
        *g = flavor;
    }
    let worker = Worker::spawn(flavor, move || {
        let engines = config
            .and_then(|c| fdv_engines_ext::load(&c).map_err(|e| eprintln!("fdv: {e:#}")).ok())
            .unwrap_or_default()
            .into_iter()
            .chain(fdv_bmc::engines())
            .collect();
        let mut session = Session::new(engines, PathBuf::from(WORKDIR));
        session.sink = Some(Arc::new(EventSink));
        session
    });
    if let Some(p) = script {
        let w = worker.clone();
        std::thread::spawn(move || {
            let r = w.call(Request::Source(p));
            if !r.ok {
                eprintln!("fdv: {}", r.result);
            }
        });
    }
    tauri::Builder::default()
        .setup(|app| {
            let _ = APP.set(app.handle().clone());
            Ok(())
        })
        .manage(worker)
        .invoke_handler(tauri::generate_handler![
            snapshot,
            shell_eval,
            shell_source,
            shell_flavor,
            shell_stop,
            shell_complete,
            read_text,
            read_trace
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Formal DV app");
}

#[cfg(test)]
mod tests {
    use super::{text_in, trace_in};

    #[test]
    fn text_reads_only_the_repo_and_design_files() {
        let base = std::env::temp_dir().join(format!("fdv-app-text-{}", std::process::id()));
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join("tests")).unwrap();
        std::fs::write(repo.join("tests/run.tcl"), "assert -name a {x}\n").unwrap();
        std::fs::write(base.join("ip.sv"), "module ip; endmodule\n").unwrap();
        std::fs::write(base.join("secret.txt"), "no").unwrap();
        let files = vec![base.join("ip.sv")];
        assert!(text_in(&repo, &files, &repo.join("tests/run.tcl")).unwrap().contains("assert"));
        assert!(text_in(&repo, &files, &base.join("ip.sv")).unwrap().contains("module ip"));
        assert!(text_in(&repo, &files, &base.join("secret.txt")).is_err());
        assert!(text_in(&repo, &files, &repo.join("../secret.txt")).is_err());
        assert!(text_in(&repo, &[], &base.join("ip.sv")).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn trace_reads_only_vcd_inside_the_workdir() {
        let base = std::env::temp_dir().join(format!("fdv-app-trace-{}", std::process::id()));
        let work = base.join("fdv_work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("a.vcd"), "$enddefinitions $end\n").unwrap();
        std::fs::write(work.join("prop_0.btor2"), "1 sort bitvec 1\n").unwrap();
        std::fs::write(base.join("outside.vcd"), "x").unwrap();
        assert!(trace_in(&work, &work.join("a.vcd")).unwrap().contains("$enddefinitions"));
        assert!(trace_in(&work, &work.join("prop_0.btor2")).is_err());
        assert!(trace_in(&work, &work.join("../outside.vcd")).is_err());
        assert!(trace_in(&work, &base.join("outside.vcd")).is_err());
        assert!(trace_in(&work, &work.join("missing.vcd")).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
