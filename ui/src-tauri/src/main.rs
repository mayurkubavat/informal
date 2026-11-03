//! Formal DV desktop app: a Tauri window over one fdv shell.
//!
//! The shell (Tcl interpreter plus FPV session) lives on its own thread
//! (`fdv_fpv::Worker`); each command below forwards one request to it and
//! returns the Tcl result together with the session, which the property
//! table redraws from. Commands are `async` so a long prove never blocks
//! the window.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use fdv_fpv::{Flavor, Reply, Request, Session, Worker};
use std::path::{Path, PathBuf};
use tauri::State;

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

async fn ask(w: &Worker, req: Request) -> Reply {
    let w = w.clone();
    tauri::async_runtime::spawn_blocking(move || w.call(req))
        .await
        .unwrap_or_else(|e| Reply { ok: false, result: e.to_string(), session: serde_json::Value::Null })
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
    Ok(ask(&w, Request::Flavor(f)).await)
}

/// Design source text for the source view.
#[tauri::command]
fn read_text(path: String) -> Result<String, String> {
    std::fs::read_to_string(Path::new(&path)).map_err(|e| format!("{path}: {e}"))
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
    let worker = Worker::spawn(flavor, move || {
        let engines = config
            .and_then(|c| fdv_engines_ext::load(&c).map_err(|e| eprintln!("fdv: {e:#}")).ok())
            .unwrap_or_default()
            .into_iter()
            .chain(fdv_bmc::engines())
            .collect();
        Session::new(engines, PathBuf::from("fdv_work"))
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
        .manage(worker)
        .invoke_handler(tauri::generate_handler![snapshot, shell_eval, shell_source, shell_flavor, read_text])
        .run(tauri::generate_context!())
        .expect("error while running the Formal DV app");
}
