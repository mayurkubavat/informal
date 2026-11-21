//! InFormal desktop app: a Tauri window over one informal shell.
//!
//! The shell (Tcl interpreter plus FPV session) lives on its own thread
//! (`informal_fpv::Worker`); each command below forwards one request to it and
//! returns the Tcl result together with the session, which the property
//! table redraws from. Commands are `async` so a long prove never blocks
//! the window.
//!
//! While a prove runs, the session's progress sink emits "prove-progress"
//! and "console-line" events to the window, and `shell_stop` sets the
//! shell's stop flag directly (the shell thread is busy with the prove).
//! The event contract is in docs/ui/stop-progress.md. External engines'
//! stdout/stderr lines arrive as "engine-log" events (the Log tab).
//!
//! Ask mode (docs/ui/ask-mode.md): `ask` streams an LLM's answer as
//! "ask-delta" events and ends with "ask-done", which may carry one
//! suggested Tcl command. Nothing the model says is run here; the UI puts
//! the command on the prompt and the user presses Enter (or Esc). Before it
//! answers, the model may read the session with tools (`ask.rs` TOOLS);
//! each call is shown as an "ask-step" event.
//!
//! The AI interface (decision D17, docs/ai-interface.md): the same session
//! is served to an outside agent on a local socket (`ai.rs`). What the
//! agent proposes waits for the person (`ai_status`, `ai_decide`), and an
//! "ai-activity" event tells the window to redraw after each of its calls.
//! The agent's window tools (`gui_state`, `gui_open_trace`, ...) read what
//! the views last reported with `ui_state`, and reach them as "ai-gui"
//! events that the views answer with `ai_gui_done`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod ask;

use informal_fpv::{EngineLine, Flavor, ProveProgress, ProveSink, Reply, Request, Session, Worker};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{AppHandle, Emitter, Manager, State};

/// Set once the window exists; events before that (a start-up script's
/// prove can begin first) are dropped.
static APP: OnceLock<AppHandle> = OnceLock::new();

/// Payload of "console-line".
#[derive(Clone, Serialize)]
struct ConsoleLine<'a> {
    text: &'a str,
    kind: &'static str,
}

/// Forwards prove progress from the shell to the window as Tauri events,
/// and to the AI interface, whose agents ask for progress and the log.
struct EventSink(Arc<informal_mcp::State>);

impl ProveSink for EventSink {
    fn progress(&self, p: &ProveProgress) {
        self.0.progress(p);
        if let Some(app) = APP.get() {
            let _ = app.emit("prove-progress", p);
        }
    }
    fn line(&self, text: &str, err: bool) {
        self.0.line(text, err);
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

/// `config/engines.toml` from `INFORMAL_ENGINES`, or the nearest one above the
/// working directory (the repo root when run with `npm run tauri dev`).
fn engines_config() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("INFORMAL_ENGINES") {
        return Some(p.into());
    }
    if let Ok(mut dir) = std::env::current_dir() {
        loop {
            let c = dir.join("config/engines.toml");
            if c.is_file() {
                return Some(c);
            }
            if !dir.pop() {
                break;
            }
        }
    }
    // When launched from macOS Finder / Dock / Spotlight, current_dir is `/`:
    // fall back to the checkout this binary was built from when present.
    let built_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/engines.toml");
    if built_root.is_file() {
        return Some(built_root);
    }
    None
}

/// The design file list and property names from the last reply, and the
/// active flavor, for `read_text` and `shell_complete` (which must not
/// queue behind a running prove on the shell thread).
static DESIGN_FILES: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static PROP_NAMES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static FLAVOR: Mutex<Flavor> = Mutex::new(Flavor::Jg);
/// The session JSON of the last reply, for Ask mode's prompt.
static LAST_SESSION: Mutex<serde_json::Value> = Mutex::new(serde_json::Value::Null);

async fn call(w: &Worker, req: Request) -> Reply {
    let w = w.clone();
    // While a command of the person's runs or waits, an agent's tools
    // answer "busy" at once.
    let held = matches!(req, Request::Eval(_) | Request::Source(_))
        .then(|| {
            APP.get()
                .and_then(|a| a.try_state::<ai::Ai>().map(|ai| ai.api.state().foreign()))
        })
        .flatten();
    let r = tauri::async_runtime::spawn_blocking(move || {
        let _held = held;
        w.call(req)
    })
    .await
    .unwrap_or_else(|e| Reply {
        ok: false,
        result: e.to_string(),
        session: serde_json::Value::Null,
    });
    if let Some(a) = r.session["design"]["files"].as_array() {
        let files = a
            .iter()
            .filter_map(|f| f.as_str().map(PathBuf::from))
            .collect();
        if let Ok(mut g) = DESIGN_FILES.lock() {
            *g = files;
        }
    }
    if !r.session.is_null() {
        if let Ok(mut g) = LAST_SESSION.lock() {
            *g = r.session.clone();
        }
    }
    if let Some(a) = r.session["props"].as_array() {
        let names = a
            .iter()
            .filter_map(|p| p["name"].as_str().map(String::from))
            .collect();
        if let Ok(mut g) = PROP_NAMES.lock() {
            *g = names;
        }
    }
    r
}

#[tauri::command]
async fn snapshot(w: State<'_, Worker>) -> Result<Reply, String> {
    Ok(call(&w, Request::Snapshot).await)
}

#[tauri::command]
async fn shell_eval(w: State<'_, Worker>, script: String) -> Result<Reply, String> {
    Ok(call(&w, Request::Eval(script)).await)
}

#[tauri::command]
async fn shell_source(w: State<'_, Worker>, path: String) -> Result<Reply, String> {
    if let Some(tpl) = path.strip_prefix("template:") {
        let dir = Path::new(WORKDIR).join("templates");
        let _ = std::fs::create_dir_all(&dir);
        let script = match tpl {
            "axi4lite" => {
                let sv = dir.join("axi_lite_demo.sv");
                let _ = std::fs::write(
                    &sv,
                    "module axi_lite_demo (\n\
                     \x20 input  logic        clk,\n\
                     \x20 input  logic        rst_n,\n\
                     \x20 output logic        awvalid,\n\
                     \x20 input  logic        awready,\n\
                     \x20 output logic [31:0] awaddr,\n\
                     \x20 output logic        wvalid,\n\
                     \x20 input  logic        wready,\n\
                     \x20 output logic [31:0] wdata,\n\
                     \x20 output logic [3:0]  wstrb,\n\
                     \x20 input  logic        bvalid,\n\
                     \x20 output logic        bready,\n\
                     \x20 input  logic [1:0]  bresp,\n\
                     \x20 output logic        arvalid,\n\
                     \x20 input  logic        arready,\n\
                     \x20 output logic [31:0] araddr,\n\
                     \x20 input  logic        rvalid,\n\
                     \x20 output logic        rready,\n\
                     \x20 input  logic [31:0] rdata,\n\
                     \x20 input  logic [1:0]  rresp\n\
                     );\n\
                     \x20 assign awaddr = 32'h0; assign wdata = 32'h0; assign wstrb = 4'hf; assign araddr = 32'h0;\n\
                     \x20 assign bready = 1'b1; assign rready = 1'b1; assign wvalid = 1'b0; assign arvalid = 1'b0;\n\
                     \x20 always_ff @(posedge clk or negedge rst_n)\n\
                     \x20   if (!rst_n) awvalid <= 1'b0;\n\
                     \x20   else if (!awvalid) awvalid <= 1'b1;\n\
                     \x20   else if (awready) awvalid <= 1'b0;\n\
                     endmodule\n",
                );
                format!(
                    "clear -all; analyze -sv12 {{{}}}; elaborate -top axi_lite_demo; clock clk; reset -expression {{!rst_n}}; check_protocol -create -protocol axi4lite -role master",
                    sv.display()
                )
            }
            "csr" => {
                let sv = dir.join("csr_demo.sv");
                let csv = dir.join("csr_demo.csv");
                let _ = std::fs::write(
                    &sv,
                    "module csr_demo (\n\
                     \x20 input  logic        clk,\n\
                     \x20 input  logic        rst_n,\n\
                     \x20 input  logic        wr_en,\n\
                     \x20 input  logic [7:0]  addr,\n\
                     \x20 input  logic [31:0] wr_data,\n\
                     \x20 output logic [31:0] ctrl_q,\n\
                     \x20 output logic [31:0] status_q\n\
                     );\n\
                     \x20 assign status_q = 32'h0000_00a5;\n\
                     \x20 always_ff @(posedge clk or negedge rst_n)\n\
                     \x20   if (!rst_n) ctrl_q <= 32'h0;\n\
                     \x20   else if (wr_en && addr == 8'h00) ctrl_q <= wr_data;\n\
                     endmodule\n",
                );
                let _ = std::fs::write(
                    &csv,
                    "name,offset,access,reset,field_signal,bus_write,bus_addr,bus_wdata\n\
                     ctrl,0x00,RW,0x0,ctrl_q,wr_en,addr,wr_data\n\
                     status,0x04,RO,0xa5,status_q,wr_en,addr,wr_data\n",
                );
                format!(
                    "clear -all; analyze -sv12 {{{}}}; elaborate -top csr_demo; clock clk; reset -expression {{!rst_n}}; check_csr -load {{{}}}",
                    sv.display(),
                    csv.display()
                )
            }
            _ => format!("source {{{tpl}}}"),
        };
        return Ok(call(&w, Request::Eval(script)).await);
    }
    if path.ends_with(".sv") || path.ends_with(".svh") {
        let script = format!("clear -all; analyze -sv12 {{{path}}}; elaborate");
        return Ok(call(&w, Request::Eval(script)).await);
    }
    Ok(call(&w, Request::Source(path.into())).await)
}

#[tauri::command]
async fn shell_flavor(w: State<'_, Worker>, flavor: String) -> Result<Reply, String> {
    let f = Flavor::parse(&flavor).ok_or_else(|| format!("unknown flavor {flavor}"))?;
    if let Ok(mut g) = FLAVOR.lock() {
        *g = f;
    }
    Ok(call(&w, Request::Flavor(f)).await)
}

/// The shell's flavor (`jg` or `vcf`): a script given at start sets it
/// before the window exists, so the UI asks.
#[tauri::command]
fn current_flavor() -> &'static str {
    match FLAVOR.lock().map(|g| *g).unwrap_or(Flavor::Jg) {
        Flavor::Jg => "jg",
        Flavor::Vcf => "vcf",
    }
}

/// Console Tab completion: the active flavor's command names, then
/// property names, starting with `prefix` (informal_fpv::compat::complete).
/// Answered here from the last reply, so it works while a prove runs.
#[tauri::command]
fn shell_complete(prefix: String) -> Vec<String> {
    let flavor = FLAVOR.lock().map(|g| *g).unwrap_or(Flavor::Jg);
    let props = PROP_NAMES.lock().map(|g| g.clone()).unwrap_or_default();
    informal_fpv::compat::complete(flavor, &props, &prefix)
}

/// Stop the running prove. Not sent through the shell thread, which is
/// busy proving: this sets the shared stop flag and returns at once. The
/// prove's own `shell_eval` then returns, after a "stopped" event.
#[tauri::command]
async fn shell_stop(w: State<'_, Worker>) -> Result<Reply, String> {
    w.stop();
    Ok(Reply {
        ok: true,
        result: String::new(),
        session: serde_json::Value::Null,
    })
}

/// The session's work directory, relative to the repo root the app runs
/// from: generated models and trace VCDs land here.
const WORKDIR: &str = "informal_work";

/// A trace VCD for the trace tab (`Property.trace`). Only `.vcd` files
/// inside the work directory are served, whatever the path says.
#[tauri::command]
fn read_trace(path: String) -> Result<String, String> {
    trace_in(Path::new(WORKDIR), Path::new(&path))
}

/// Binary `IFTR` v1 waveform stream over raw Tauri v2 IPC (`Uint8Array` on JS side),
/// avoiding VCD text allocation and JSON serialization for large traces.
#[tauri::command]
fn read_trace_bin(path: String) -> Result<tauri::ipc::Response, String> {
    let text = trace_in(Path::new(WORKDIR), Path::new(&path))?;
    let bytes = informal_mcp::vcd::encode_iftr(&text)?;
    Ok(tauri::ipc::Response::new(bytes))
}

fn trace_in(workdir: &Path, path: &Path) -> Result<String, String> {
    let p = vcd_in(workdir, path)?;
    std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", path.display()))
}

/// The canonical path of `path` when it is an existing `.vcd` file inside
/// `workdir` (symlinks and `..` resolved first), else an error.
fn vcd_in(workdir: &Path, path: &Path) -> Result<PathBuf, String> {
    let shown = path.display();
    let root = std::fs::canonicalize(workdir).map_err(|e| format!("{}: {e}", workdir.display()))?;
    let p = std::fs::canonicalize(path).map_err(|e| format!("{shown}: {e}"))?;
    if !p.starts_with(&root) || p.extension().is_none_or(|e| e != "vcd") || !p.is_file() {
        return Err(format!("{shown}: not a trace in {}", root.display()));
    }
    Ok(p)
}

/// An executable called `name` in one of the `path` directories (a PATH
/// value). Plain names only: anything with a path separator is refused.
fn find_on_path(name: &str, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    std::env::split_paths(path?)
        .map(|d| d.join(name))
        .find(|f| {
            use std::os::unix::fs::PermissionsExt;
            f.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// Whether an external tool (e.g. `surfer`) is on PATH; the UI enables its
/// hand-off buttons from this at start-up.
#[tauri::command]
fn tool_available(name: String) -> bool {
    find_on_path(&name, std::env::var_os("PATH").as_deref()).is_some()
}

/// Open a trace in the Surfer waveform viewer (surfer-project.org):
/// spawns `surfer <vcd>` detached (its own process group, so it is not tied
/// to the app's) and returns at once.
/// Only `.vcd` files inside the work directory are opened.
#[tauri::command]
fn open_in_surfer(path: String) -> Result<(), String> {
    let vcd = vcd_in(Path::new(WORKDIR), Path::new(&path))?;
    let surfer = find_on_path("surfer", std::env::var_os("PATH").as_deref())
        .ok_or("surfer is not on PATH (install it from surfer-project.org)")?;
    spawn_detached(&surfer, &vcd)
}

fn spawn_detached(program: &Path, arg: &Path) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut child = Command::new(program)
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("{}: {e}", program.display()))?;
    // Reap it when it exits, so no zombie is left behind.
    std::thread::spawn(move || child.wait());
    Ok(())
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
        return Err(format!(
            "{shown}: not under {} or in the design files",
            root.display()
        ));
    }
    std::fs::read_to_string(&p).map_err(|e| format!("{shown}: {e}"))
}

/// Ask mode's providers (config/llm.toml) and the one in use. Switching
/// with `llm_select` lasts until the app closes; the file is not written.
struct Llm {
    config: Result<informal_llm::LlmConfig, String>,
    current: Mutex<Option<(String, Arc<dyn informal_llm::LlmProvider>)>>,
    /// Why the current provider could not be built, if it could not.
    build_error: Mutex<Option<String>>,
    /// Running asks by id, so `ask_cancel` can abort one.
    running: Mutex<std::collections::HashMap<u64, tauri::async_runtime::JoinHandle<()>>>,
}

impl Llm {
    fn load(root: &Path) -> Llm {
        let config = match informal_llm::find_config(root) {
            Some(p) => informal_llm::LlmConfig::load(&p).map_err(|e| format!("{e:#}")),
            None => Err("no config/llm.toml".into()),
        };
        let llm = Llm {
            config,
            current: Mutex::new(None),
            build_error: Mutex::new(None),
            running: Mutex::new(Default::default()),
        };
        let env = std::env::var(informal_llm::DEFAULT_PROVIDER_ENV).ok();
        match llm
            .config
            .as_ref()
            .map_err(String::clone)
            .and_then(|c| c.selected(env.as_deref()).map_err(|e| e.to_string()))
        {
            Ok(name) => {
                let _ = llm.select(&name);
            }
            Err(e) => *llm.build_error.lock().unwrap() = Some(e),
        }
        llm
    }

    fn select(&self, name: &str) -> Result<(), String> {
        let c = self.config.as_ref().map_err(String::clone)?;
        let p = c
            .providers
            .get(name)
            .ok_or_else(|| format!("no LLM provider {name}"))?;
        match informal_llm::build(name, p) {
            Ok(b) => {
                *self.current.lock().unwrap() = Some((name.to_string(), Arc::from(b)));
                *self.build_error.lock().unwrap() = None;
                Ok(())
            }
            Err(e) => {
                let e = format!("{e:#}");
                *self.build_error.lock().unwrap() = Some(e.clone());
                Err(e)
            }
        }
    }

    /// How the provider in use is given tools (its `tools` setting).
    fn tool_mode(&self) -> informal_llm::agent::ToolMode {
        let current = self.current.lock().unwrap();
        let cfg = current
            .as_ref()
            .and_then(|(name, _)| self.config.as_ref().ok()?.providers.get(name));
        cfg.map_or(
            informal_llm::agent::ToolMode::Off,
            informal_llm::agent::ToolMode::of,
        )
    }

    fn provider(&self) -> Result<Arc<dyn informal_llm::LlmProvider>, String> {
        if let Some((_, p)) = self.current.lock().unwrap().as_ref() {
            return Ok(p.clone());
        }
        Err(self
            .build_error
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "no LLM provider".into()))
    }
}

#[derive(Clone, Serialize)]
struct LlmStatus {
    provider: String,
    model: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// The provider in use and whether it answers (its health check).
#[tauri::command]
async fn llm_status(llm: State<'_, Llm>) -> Result<LlmStatus, String> {
    let p = match llm.provider() {
        Ok(p) => p,
        Err(e) => {
            return Ok(LlmStatus {
                provider: String::new(),
                model: String::new(),
                ok: false,
                error: Some(e),
            })
        }
    };
    let (provider, model) = (p.name().to_string(), p.model().to_string());
    let h = tokio::time::timeout(std::time::Duration::from_secs(8), p.health()).await;
    let error = match h {
        Ok(Ok(_)) => None,
        Ok(Err(e)) => Some(format!("{e:#}")),
        Err(_) => Some("health check timed out".into()),
    };
    Ok(LlmStatus {
        provider,
        model,
        ok: error.is_none(),
        error,
    })
}

#[derive(Clone, Serialize)]
struct ProviderEntry {
    name: String,
    kind: String,
    model: String,
    base_url: String,
    selected: bool,
}

/// Configured providers, for the status-bar menu.
#[tauri::command]
fn llm_providers(llm: State<'_, Llm>) -> Result<Vec<ProviderEntry>, String> {
    let c = llm.config.as_ref().map_err(String::clone)?;
    let cur = llm.current.lock().unwrap().as_ref().map(|(n, _)| n.clone());
    Ok(c.providers
        .iter()
        .map(|(n, p)| ProviderEntry {
            name: n.clone(),
            kind: p.kind.clone(),
            model: p.model.clone(),
            base_url: p.base_url.clone(),
            selected: cur.as_deref() == Some(n),
        })
        .collect())
}

/// Switch provider for the rest of this run, then report its status.
#[tauri::command]
async fn llm_select(llm: State<'_, Llm>, name: String) -> Result<LlmStatus, String> {
    llm.select(&name)?;
    llm_status(llm).await
}

#[derive(Clone, Serialize)]
struct AskDelta<'a> {
    id: u64,
    text: &'a str,
}

#[derive(Clone, Serialize)]
struct AskDone {
    id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    explanation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    model: String,
    first_token_ms: Option<u64>,
    total_ms: u64,
}

/// Ask the model. Returns at once; the answer streams as "ask-delta"
/// {id, text} events and ends with one "ask-done" {id, command?,
/// explanation, error?}. `id` is chosen by the UI so it can match events
/// that arrive before this call returns. `context` is extra text from the
/// UI (e.g. the selected property). The command is only ever shown.
#[tauri::command]
fn ask(
    llm: State<'_, Llm>,
    id: u64,
    prompt: String,
    context: Option<String>,
) -> Result<u64, String> {
    let p = llm.provider()?;
    let flavor = FLAVOR.lock().map(|g| *g).unwrap_or(Flavor::Jg);
    let session = LAST_SESSION.lock().map(|g| g.clone()).unwrap_or_default();
    let flavor_word = if flavor == Flavor::Vcf { "vcf" } else { "jg" };
    let system = ask::system_prompt(
        flavor_word,
        &informal_fpv::compat::command_names(flavor),
        &session,
    );
    let user = match context.filter(|c| !c.trim().is_empty()) {
        Some(c) => format!("{prompt}\n\n(Context from the UI: {c})"),
        None => prompt,
    };
    // With tools, the model may read the session before it answers
    // (ask.rs TOOLS: nothing that proves or changes anything).
    let mode = llm.tool_mode();
    let tools = match (mode, APP.get().and_then(|a| a.try_state::<ai::Ai>())) {
        (informal_llm::agent::ToolMode::Off, _) | (_, None) => None,
        (_, Some(ai)) => Some(ask::Host(ai.api.clone())),
    };
    let system = match tools {
        Some(_) => format!("{system}\n\n{}", ask::TOOLS_NOTE),
        None => system,
    };
    let messages = vec![
        informal_llm::Message::system(system),
        informal_llm::Message::user(user),
    ];
    // Held across the spawn, so a very quick answer cannot remove its
    // entry before it is inserted.
    let mut running = llm.running.lock().unwrap();
    let task = tauri::async_runtime::spawn(async move {
        let mut on_event = |e: informal_llm::agent::Event| {
            let Some(app) = APP.get() else { return };
            match e {
                informal_llm::agent::Event::Text(text) => {
                    let _ = app.emit("ask-delta", AskDelta { id, text });
                }
                informal_llm::agent::Event::Call { name, arguments } => {
                    let text = &ask::step_text(name, arguments);
                    let _ = app.emit("ask-step", AskDelta { id, text });
                }
                informal_llm::agent::Event::Result { .. } => {}
            }
        };
        let (specs, host): (_, &dyn informal_llm::agent::ToolHost) = match &tools {
            Some(h) => (ask::tool_specs(), h),
            None => (Vec::new(), &ask::NoTools),
        };
        let limits = match mode {
            informal_llm::agent::ToolMode::Native => informal_llm::agent::Limits {
                max_steps: 12,
                max_result_chars: 20_000,
            },
            _ => informal_llm::agent::Limits::default(),
        };
        let run = informal_llm::agent::run(
            p.as_ref(),
            mode,
            &specs,
            host,
            messages,
            limits,
            &mut on_event,
        );
        let done = match run.await {
            Ok(r) => {
                let (explanation, command) = ask::split_answer(&r.text);
                AskDone {
                    id,
                    command,
                    explanation,
                    error: None,
                    model: r.model,
                    first_token_ms: r.first_token_ms,
                    total_ms: r.total_ms,
                }
            }
            Err(e) => AskDone {
                id,
                command: None,
                explanation: String::new(),
                error: Some(format!("{e:#}")),
                model: p.model().to_string(),
                first_token_ms: None,
                total_ms: 0,
            },
        };
        if let Some(app) = APP.get() {
            let _ = app.emit("ask-done", done);
            if let Some(llm) = app.try_state::<Llm>() {
                llm.running.lock().unwrap().remove(&id);
            }
        }
    });
    running.insert(id, task);
    Ok(id)
}

/// Abort a running ask. Its stream stops; "ask-done" is sent with error
/// "cancelled" so the UI has one ending either way.
#[tauri::command]
fn ask_cancel(llm: State<'_, Llm>, id: u64) -> bool {
    let Some(t) = llm.running.lock().unwrap().remove(&id) else {
        return false;
    };
    t.abort();
    if let Some(app) = APP.get() {
        let _ = app.emit(
            "ask-done",
            AskDone {
                id,
                command: None,
                explanation: String::new(),
                error: Some("cancelled".into()),
                model: String::new(),
                first_token_ms: None,
                total_ms: 0,
            },
        );
    }
    true
}

/// The AI interface as the window shows it: the mode, where agents
/// connect, and every proposal with its state.
#[tauri::command]
fn ai_status(ai: State<'_, ai::Ai>) -> ai::AiStatus {
    ai.status()
}

/// The person's answer to a pending proposal. Accepting applies it to the
/// session, tagged AI-added; the reply is the proposal as it now stands.
#[tauri::command]
async fn ai_decide(
    ai: State<'_, ai::Ai>,
    id: u32,
    accept: bool,
) -> Result<serde_json::Value, String> {
    let api = ai.api.clone();
    tauri::async_runtime::spawn_blocking(move || api.decide(id, accept))
        .await
        .map_err(|e| e.to_string())?
        .map(|r| r["proposal"].clone())
        .map_err(|e| e.message)
}

/// The views say what the window shows (after every change), for an
/// agent's `gui_state`.
#[tauri::command]
fn ui_state(ai: State<'_, ai::Ai>, state: serde_json::Value) {
    ai.window.set_state(state);
}

/// The views say how an "ai-gui" action ended: the window's state after
/// it, or why it could not be done.
#[tauri::command]
fn ai_gui_done(
    ai: State<'_, ai::Ai>,
    id: u64,
    state: Option<serde_json::Value>,
    error: Option<String>,
) {
    let result = match (state, error) {
        (_, Some(e)) => Err(e),
        (Some(s), None) => Ok(s),
        (None, None) => Err("the window gave no state".into()),
    };
    ai.window.done(id, result);
}

/// Structured tool queries used by the rich UI views (CodeMirror `dv-lint` + quick-fixes,
/// Go-to-Driver/Load `signal_nav`, `schematic_get`, `fv_complexity`).
#[tauri::command]
async fn workbench_tool(
    ai: State<'_, ai::Ai>,
    tool: String,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    const ALLOWED: &[&str] = &[
        "schematic_get",
        "formal_lint",
        "formal_fix_apply",
        "signal_nav",
        "fv_complexity",
        "why_cex",
        "open_workbench",
    ];
    if !ALLOWED.contains(&tool.as_str()) {
        return Err(format!("disallowed workbench tool: {tool}"));
    }
    let api = ai.api.clone();
    tauri::async_runtime::spawn_blocking(move || api.call(&tool, &args))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.message)
}

/// Open or focus a secondary monitor pop-out window (`trace` or `schematic`).
#[tauri::command]
fn pop_out_window(app: AppHandle, kind: String, title: String) -> Result<(), String> {
    if !matches!(kind.as_str(), "trace" | "schematic") {
        return Err(format!("unsupported pop-out kind: {kind}"));
    }
    let label = format!("informal-popout-{kind}");
    if let Some(existing) = app.get_webview_window(&label) {
        let _ = existing.set_title(&title);
        let _ = existing.set_focus();
        return Ok(());
    }
    tauri::WebviewWindowBuilder::new(
        &app,
        label,
        tauri::WebviewUrl::App(format!("index.html?popout={kind}").into()),
    )
    .title(title)
    .inner_size(1120.0, 680.0)
    .min_inner_size(640.0, 400.0)
    .build()
    .map(|_| ())
    .map_err(|e| e.to_string())
}

fn main() {
    // Augment minimal macOS LaunchServices PATH so external engines (`ric3`,
    // `abc`) and `surfer` in ~/.local/bin or Homebrew are found from Dock/Spotlight.
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let cur = std::env::var_os("PATH").unwrap_or_default();
        let mut paths: Vec<PathBuf> = std::env::split_paths(&cur).collect();
        for extra in [
            home.join(".local/bin"),
            home.join(".cargo/bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ] {
            if extra.is_dir() && !paths.contains(&extra) {
                paths.push(extra);
            }
        }
        if let Ok(joined) = std::env::join_paths(paths) {
            std::env::set_var("PATH", joined);
        }
    }
    // Run from the repo root (the directory holding config/engines.toml), so
    // script paths read as they do for `informal shell` and generated models land
    // in its informal_work/ rather than under src-tauri, which the dev watcher
    // would take as a source change.
    let config = engines_config();
    if let Some(root) = config.as_ref().and_then(|c| c.parent()?.parent()) {
        let _ = std::env::set_current_dir(root);
    }
    let config = config.and_then(|c| std::fs::canonicalize(c).ok());
    // `informal-app script.tcl` sources a script at start, flavor detected from it.
    let script = std::env::args().nth(1).map(PathBuf::from);
    let flavor = script
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map_or(Flavor::Jg, |t| informal_fpv::detect_flavor(&t));
    if let Ok(mut g) = FLAVOR.lock() {
        *g = flavor;
    }
    let ai_state = Arc::new(informal_mcp::State::default());
    let sink = Arc::new(EventSink(ai_state.clone()));
    let worker = Worker::spawn(flavor, move || {
        let engines = config
            .and_then(|c| {
                informal_engines_ext::load(&c)
                    .map_err(|e| eprintln!("informal: {e:#}"))
                    .ok()
            })
            .unwrap_or_default()
            .into_iter()
            .chain(informal_bmc::engines())
            .collect();
        let mut session = Session::new(engines, PathBuf::from(WORKDIR));
        session.sink = Some(sink);
        session
    });
    if let Some(p) = script {
        let w = worker.clone();
        std::thread::spawn(move || {
            let r = w.call(Request::Source(p));
            if !r.ok {
                eprintln!("informal: {}", r.result);
            }
        });
    }
    // Agents connect to a socket only this user can reach; INFORMAL_AI_SOCKET
    // moves it, or turns it off with `off`.
    let socket = match std::env::var_os("INFORMAL_AI_SOCKET") {
        Some(p) if p == "off" => None,
        Some(p) => Some(PathBuf::from(p)),
        None => Some(informal_mcp::socket::for_app(std::process::id())),
    };
    // An agent's window tools reach the views as "ai-gui" events.
    let window = ai::Window::new(|id, action| {
        if let Some(app) = APP.get() {
            let _ = app.emit("ai-gui", serde_json::json!({"id": id, "action": action}));
        }
    });
    let ai = ai::start(worker.clone(), ai_state, Path::new(WORKDIR), socket, window);
    if let Some(e) = &ai.error {
        eprintln!("informal: the AI interface socket could not be opened: {e}");
    }
    ai.api.set_change_hook(Some(Arc::new(|tool: &str| {
        if let Some(app) = APP.get() {
            let _ = app.emit("ai-activity", tool);
        }
    })));
    let llm = Llm::load(&std::env::current_dir().unwrap_or_default());
    tauri::Builder::default()
        .setup(|app| {
            let _ = APP.set(app.handle().clone());
            Ok(())
        })
        .manage(worker)
        .manage(llm)
        .manage(ai)
        .invoke_handler(tauri::generate_handler![
            snapshot,
            shell_eval,
            shell_source,
            shell_flavor,
            current_flavor,
            shell_stop,
            shell_complete,
            read_text,
            read_trace,
            read_trace_bin,
            tool_available,
            open_in_surfer,
            llm_status,
            llm_providers,
            llm_select,
            ask,
            ask_cancel,
            ai_status,
            ai_decide,
            ui_state,
            ai_gui_done,
            workbench_tool,
            pop_out_window
        ])
        .run(tauri::generate_context!())
        .expect("error while running the InFormal app");
}

#[cfg(test)]
mod tests {
    use super::{find_on_path, spawn_detached, text_in, trace_in, vcd_in};

    #[test]
    fn text_reads_only_the_repo_and_design_files() {
        let base = std::env::temp_dir().join(format!("informal-app-text-{}", std::process::id()));
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join("tests")).unwrap();
        std::fs::write(repo.join("tests/run.tcl"), "assert -name a {x}\n").unwrap();
        std::fs::write(base.join("ip.sv"), "module ip; endmodule\n").unwrap();
        std::fs::write(base.join("secret.txt"), "no").unwrap();
        let files = vec![base.join("ip.sv")];
        assert!(text_in(&repo, &files, &repo.join("tests/run.tcl"))
            .unwrap()
            .contains("assert"));
        assert!(text_in(&repo, &files, &base.join("ip.sv"))
            .unwrap()
            .contains("module ip"));
        assert!(text_in(&repo, &files, &base.join("secret.txt")).is_err());
        assert!(text_in(&repo, &files, &repo.join("../secret.txt")).is_err());
        assert!(text_in(&repo, &[], &base.join("ip.sv")).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn trace_reads_only_vcd_inside_the_workdir() {
        let base = std::env::temp_dir().join(format!("informal-app-trace-{}", std::process::id()));
        let work = base.join("informal_work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("a.vcd"), "$enddefinitions $end\n").unwrap();
        std::fs::write(work.join("prop_0.btor2"), "1 sort bitvec 1\n").unwrap();
        std::fs::write(base.join("outside.vcd"), "x").unwrap();
        assert!(trace_in(&work, &work.join("a.vcd"))
            .unwrap()
            .contains("$enddefinitions"));
        assert!(trace_in(&work, &work.join("prop_0.btor2")).is_err());
        assert!(trace_in(&work, &work.join("../outside.vcd")).is_err());
        assert!(trace_in(&work, &base.join("outside.vcd")).is_err());
        assert!(trace_in(&work, &work.join("missing.vcd")).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn surfer_opens_only_vcd_inside_the_workdir() {
        let base = std::env::temp_dir().join(format!("informal-app-surfer-{}", std::process::id()));
        let work = base.join("informal_work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        std::fs::write(work.join("a.vcd"), "$enddefinitions $end\n").unwrap();
        std::fs::write(work.join("prop_0.btor2"), "1 sort bitvec 1\n").unwrap();
        std::fs::write(base.join("outside.vcd"), "x").unwrap();
        std::os::unix::fs::symlink(base.join("outside.vcd"), work.join("link.vcd")).unwrap();
        let ok = vcd_in(&work, &work.join("a.vcd")).unwrap();
        assert!(ok.is_absolute() && ok.ends_with("informal_work/a.vcd"));
        assert!(vcd_in(&work, &work.join("sub/../a.vcd")).is_ok());
        for bad in [
            "prop_0.btor2",
            "../outside.vcd",
            "link.vcd",
            "missing.vcd",
            "sub",
        ] {
            assert!(
                vcd_in(&work, &work.join(bad)).is_err(),
                "{bad} must be refused"
            );
        }
        assert!(vcd_in(&work, &base.join("outside.vcd")).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn tools_are_found_on_path_by_plain_name_only() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("informal-app-path-{}", std::process::id()));
        let (bin, other) = (base.join("bin"), base.join("other"));
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let exe = bin.join("surfer");
        std::fs::write(
            &exe,
            "#!/bin/sh\necho \"$1\" > \"$(dirname \"$0\")/opened\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(other.join("surfer"), "not executable").unwrap();
        let path = std::env::join_paths([&other, &bin]).unwrap();
        assert_eq!(find_on_path("surfer", Some(&path)), Some(exe.clone()));
        assert_eq!(
            find_on_path("surfer", Some(&std::env::join_paths([&other]).unwrap())),
            None
        );
        assert_eq!(find_on_path("../bin/surfer", Some(&path)), None);
        assert_eq!(find_on_path("", Some(&path)), None);
        assert_eq!(find_on_path("surfer", None), None);
        // The detached spawn runs the tool with the trace path and returns.
        spawn_detached(&exe, &base.join("t.vcd")).unwrap();
        let opened = bin.join("opened");
        let t = std::time::Instant::now();
        while !opened.exists() && t.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            std::fs::read_to_string(&opened).unwrap().trim(),
            base.join("t.vcd").display().to_string()
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
