//! The Model Context Protocol side: JSON-RPC 2.0 messages, one per line,
//! answered from the tools in [`crate::api`]. The transport is any reader
//! and writer; `informal mcp` uses standard input and output.

use crate::api::{Api, Reply};
use crate::tools;
use informal_fpv::{ProgressKind, ProveProgress};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

/// Protocol revisions this server speaks, newest first.
const PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
const APP_MIME: &str = "text/html;profile=mcp-app";
const WORKBENCH_HTML: &str = include_str!("../assets/workbench.html");

fn read_workbench_html() -> String {
    if let Ok(p) = std::env::var("INFORMAL_MCP_UI_HTML") {
        if let Ok(s) = std::fs::read_to_string(p) {
            return s;
        }
    }
    WORKBENCH_HTML.to_string()
}

const INSTRUCTIONS: &str = "InFormal is an IEEE 1800 SystemVerilog formal property verification tool. \
Workflow: open a design with `session_open` (run `formal_lint` early for SV-FPV-001..005 hygiene), \
run `prove` to settle properties, call `cex_diagnose` first on any `cex` (single-call causal slice + \
active trace + signal roles + RTL drivers), and call `convergence_diagnose` first on any `undetermined` \
property. Decision D17: only a `prove` verdict establishes truth; NEVER mask an RTL bug or constrain \
DUT outputs/internal state inside an `assume`. Always call `proposal_validate` (zero-side-effect scratch \
task checking environment consistency, 2QBF dead-end freedom, cover non-regression, vacuity, and target \
impact) BEFORE `proposal_add`. When an `fvp.spec.json` (`informal.spec/1`) verification plan exists, \
call `spec_audit` to check requirement-to-SVA completeness, input-only assumption boundaries, and signoff \
gates. On a desktop window, prefer `gui_present_cex` for an atomic visual walkthrough. Use MCP \
`prompts/list` for hydrated playbooks (`triage_cex`, `close_undetermined`, `spec_to_sva`, `audit_signoff`).";

type Out = Arc<dyn Fn(&Value) + Send + Sync>;

/// One client's connection to the session.
pub struct Server {
    api: Arc<Api>,
    out: Out,
    /// The request id of the long tool call in flight, for cancellation.
    long: Arc<Mutex<Option<Value>>>,
}

impl Server {
    /// `out` is called with every message for the client: replies, which
    /// for a long tool call come later and from another thread, and
    /// progress notifications.
    pub fn new(api: Arc<Api>, out: impl Fn(&Value) + Send + Sync + 'static) -> Server {
        Server {
            api,
            out: Arc::new(out),
            long: Arc::default(),
        }
    }

    /// Take one message from the client.
    pub fn handle(&self, line: &str) {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => return (self.out)(&error(Value::Null, -32700, &format!("not JSON: {e}"))),
        };
        let id = msg.get("id").cloned();
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // A reply to a request of ours (we make none), or not a message.
            if msg.get("result").is_none() && msg.get("error").is_none() {
                (self.out)(&error(id.unwrap_or(Value::Null), -32600, "not a request"));
            }
            return;
        };
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let Some(id) = id else {
            // A notification: nothing is answered.
            if method == "notifications/cancelled" {
                let running = self.long.lock().unwrap().clone();
                if running.is_some() && running.as_ref() == params.get("requestId") {
                    self.api.stop();
                }
            }
            return;
        };
        match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str);
                let version = PROTOCOLS
                    .iter()
                    .find(|p| Some(**p) == asked)
                    .unwrap_or(&PROTOCOLS[0]);
                (self.out)(&result(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {
                            "tools": {"listChanged": false},
                            "resources": {"listChanged": false},
                            "prompts": {"listChanged": false},
                        },
                        "serverInfo": {
                            "name": "informal",
                            "title": "InFormal formal verification",
                            "version": env!("CARGO_PKG_VERSION"),
                        },
                        "instructions": format!(
                            "{INSTRUCTIONS} This server runs in {} mode.",
                            self.api.mode().label()
                        ),
                    }),
                ));
            }
            "ping" => (self.out)(&result(id, json!({}))),
            "tools/list" => {
                let list: Vec<Value> = tools::for_mode(self.api.mode(), self.api.has_gui())
                    .iter()
                    .map(|t| {
                        let mut item = json!({
                            "name": t.name,
                            "title": t.title,
                            "description": t.description,
                            "inputSchema": t.input,
                            "outputSchema": t.output,
                            "annotations": {"readOnlyHint": !t.changes},
                        });
                        if let Some(meta) = tools::ui_meta(t.name) {
                            item["_meta"] = meta;
                        }
                        item
                    })
                    .collect();
                (self.out)(&result(id, json!({"tools": list})));
            }
            "resources/list" => {
                (self.out)(&result(
                    id,
                    json!({
                        "resources": [
                            {
                                "uri": tools::APP_URI,
                                "name": "InFormal Workbench",
                                "description": "Interactive formal verification waveform, causal schematic, RTL source, and Formal Apps workbench",
                                "mimeType": APP_MIME,
                                "_meta": {
                                    "ui": {"prefersBorder": false}
                                }
                            }
                        ]
                    }),
                ));
            }
            "resources/read" => {
                let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
                if uri != tools::APP_URI {
                    return (self.out)(&error(id, -32602, &format!("unknown resource: {uri}")));
                }
                (self.out)(&result(
                    id,
                    json!({
                        "contents": [
                            {
                                "uri": tools::APP_URI,
                                "mimeType": APP_MIME,
                                "text": read_workbench_html(),
                                "_meta": {
                                    "ui": {"prefersBorder": false}
                                }
                            }
                        ]
                    }),
                ));
            }
            "prompts/list" => (self.out)(&result(id, crate::prompts::list())),
            "prompts/get" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let ctx = self.api.prompt_context();
                match crate::prompts::get(name, &args, &ctx) {
                    Ok(v) => (self.out)(&result(id, v)),
                    Err(msg) => (self.out)(&error(id, -32602, &msg)),
                }
            }
            "tools/call" => self.call(id, &params),
            _ => (self.out)(&error(id, -32601, &format!("no method {method}"))),
        }
    }

    fn call(&self, id: Value, params: &Value) {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let available = tools::for_mode(self.api.mode(), self.api.has_gui());
        if !available.iter().any(|t| t.name == name) {
            let cands = crate::diagnose::closest_matches(name, available.iter().map(|t| t.name), 3);
            let msg = if cands.is_empty() {
                format!("unknown tool: {name}")
            } else {
                format!("unknown tool: {name} (did you mean: {}?)", cands.join(", "))
            };
            return (self.out)(&error(id, -32602, &msg));
        }
        if !Api::is_long(name, &args) {
            return (self.out)(&result(id, tool_result(self.api.call(name, &args))));
        }
        // A long call answers from a thread of its own, so that a stop or
        // a status request can be read meanwhile.
        let long = match self.api.begin_long() {
            Ok(l) => l,
            Err(e) => return (self.out)(&result(id, tool_result(Err(e)))),
        };
        *self.long.lock().unwrap() = Some(id.clone());
        let token = params.pointer("/_meta/progressToken").cloned();
        let (api, out, running, name) = (
            self.api.clone(),
            self.out.clone(),
            self.long.clone(),
            name.to_string(),
        );
        std::thread::spawn(move || {
            if let Some(token) = token {
                let (out, last) = (out.clone(), Mutex::new(None));
                api.set_progress_hook(Some(Arc::new(move |p: &ProveProgress| {
                    // One note per property settled, not per heartbeat.
                    let mut last = last.lock().unwrap();
                    if p.kind == ProgressKind::Update && *last == Some(p.done) {
                        return;
                    }
                    *last = Some(p.done);
                    out(&json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/progress",
                        "params": {
                            "progressToken": token,
                            "progress": p.done,
                            "total": p.total,
                            "message": format!("{} of {} properties settled", p.done, p.total),
                        },
                    }));
                })));
            }
            let reply = api.call_long(&name, &args, long);
            api.set_progress_hook(None);
            *running.lock().unwrap() = None;
            out(&result(id, tool_result(reply)));
        });
    }
}

fn result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// A tool's reply as the protocol carries it: the JSON as structured
/// content, and the same as text for clients that only read text. A tool
/// error is a result too, so that the model sees it.
fn tool_result(reply: Reply) -> Value {
    match reply {
        Ok(v) => json!({
            "content": [{"type": "text", "text": v.to_string()}],
            "structuredContent": v,
            "isError": false,
        }),
        Err(e) => {
            let v = e.to_json();
            json!({"content": [{"type": "text", "text": v.to_string()}], "isError": true})
        }
    }
}

/// Serve `api` to one client until `input` ends. A long call of this
/// client that is still running then is stopped; what another front end
/// on the same session started is left alone.
pub fn serve(
    api: Arc<Api>,
    input: impl BufRead,
    output: impl Write + Send + 'static,
) -> std::io::Result<()> {
    let output = Mutex::new(output);
    let server = Server::new(api.clone(), move |msg| {
        let mut w = output.lock().unwrap();
        let _ = writeln!(w, "{msg}");
        let _ = w.flush();
    });
    for line in input.lines() {
        let line = line?;
        if !line.trim().is_empty() {
            server.handle(&line);
        }
    }
    if server.long.lock().unwrap().is_some() {
        api.stop();
    }
    Ok(())
}
