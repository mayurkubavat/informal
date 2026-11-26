//! The AI interface end to end on the tests/fpv designs, with the in-house
//! engines: golden replies, every reply checked against its tool's schema,
//! the three modes, and a scripted client that speaks the protocol and
//! does what decision D17 asks of an agent: find the failing assertion,
//! read its trace as data and propose a fix, without console text.
//! Skipped without CaDiCaL (no in-house engines).

use informal_mcp::{tools, Api, Mode, Options, Server};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::Duration;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn api(mode: Mode, tag: &str) -> Option<Arc<Api>> {
    let engines = informal_bmc::engines();
    if engines.is_empty() {
        informal_engine::tool_missing("the in-house engines need CaDiCaL");
        return None;
    }
    let work = std::env::temp_dir().join(format!("informal-mcp-{tag}-{}", std::process::id()));
    let opts = Options {
        mode,
        root: root(),
        workdir: work.clone(),
        max_time: Duration::from_secs(60),
    };
    Some(Arc::new(Api::spawn(engines, work, opts)))
}

/// What `schema` (the subset of JSON Schema the tools use) rejects in `v`.
fn violations(schema: &Value, v: &Value, at: &str, out: &mut Vec<String>) {
    let is = |t: &str| match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "integer" => v.is_u64() || v.is_i64(),
        "number" => v.is_number(),
        "null" => v.is_null(),
        _ => false,
    };
    let types: Vec<&str> = match &schema["type"] {
        Value::String(t) => vec![t],
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if !types.is_empty() && !types.iter().any(|t| is(t)) {
        return out.push(format!("{at}: {v} is not {types:?}"));
    }
    if let Some(c) = schema.get("const") {
        if c != v {
            out.push(format!("{at}: {v} is not {c}"));
        }
    }
    if let Some(e) = schema.get("enum").and_then(Value::as_array) {
        if !e.contains(v) {
            out.push(format!("{at}: {v} is not one of {e:?}"));
        }
    }
    if let (Some(o), Some(props)) = (v.as_object(), schema.get("properties")) {
        for r in schema["required"].as_array().into_iter().flatten() {
            if !o.contains_key(r.as_str().unwrap()) {
                out.push(format!("{at}: {r} is missing"));
            }
        }
        for (k, child) in o {
            match props.get(k) {
                Some(s) => violations(s, child, &format!("{at}.{k}"), out),
                // A reply says nothing its schema does not describe.
                None => out.push(format!("{at}: {k} is not in the schema")),
            }
        }
    }
    if let (Some(a), Some(items)) = (v.as_array(), schema.get("items")) {
        for (i, child) in a.iter().enumerate() {
            violations(items, child, &format!("{at}[{i}]"), out);
        }
    }
}

/// Call a tool, and hold its reply to the tool's output schema.
fn call(api: &Arc<Api>, tool: &str, args: Value) -> Value {
    let reply = api
        .call(tool, &args)
        .unwrap_or_else(|e| panic!("{tool} {args}: {e:?}"));
    let all: Vec<tools::Tool> = tools::all().into_iter().chain(tools::gui()).collect();
    let t = all.iter().find(|t| t.name == tool).unwrap();
    let mut bad = Vec::new();
    violations(&t.output, &reply, tool, &mut bad);
    assert!(
        bad.is_empty(),
        "{tool} breaks its schema: {bad:#?}\n{reply:#}"
    );
    reply
}

/// A reply without what differs from run to run or machine to machine:
/// which engine won and how fast, the depth it happened to find, the
/// engines of this build, and where the repository is.
fn stable(v: &Value) -> Value {
    match v {
        Value::Object(o) => o
            .iter()
            .map(|(k, v)| {
                let v = match k.as_str() {
                    "engine" => json!("<engine>"),
                    "time_ms" | "depth" => json!(0),
                    "engines" => json!("<engines>"),
                    // Measured times and engine-dependent sizes; their
                    // content is tested in informal-fpv's tests/stats.rs.
                    "stats" => json!("<stats>"),
                    _ => stable(v),
                };
                (k.clone(), v)
            })
            .collect::<serde_json::Map<_, _>>()
            .into(),
        Value::Array(a) => a.iter().map(stable).collect(),
        Value::String(s) => json!(s.replace(&root().display().to_string(), "<root>")),
        _ => v.clone(),
    }
}

/// Compare with `tests/golden/<name>.json`. `INFORMAL_BLESS=1` rewrites the
/// file instead (the gate clears that variable).
fn golden(name: &str, v: &Value) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/golden/{name}.json"));
    let got = format!("{:#}\n", stable(v));
    if std::env::var_os("INFORMAL_BLESS").is_some() {
        std::fs::write(&path, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        got == want,
        "{name} differs from {} (INFORMAL_BLESS=1 rewrites it):\n{got}",
        path.display()
    );
}

#[test]
fn the_tool_list_is_the_golden_one() {
    let list: Vec<Value> = tools::all()
        .iter()
        .map(|t| json!({"name": t.name, "description": t.description, "changes": t.changes, "input": t.input, "output": t.output}))
        .collect();
    golden("tools", &json!(list));
    // The document lists every tool.
    let doc = std::fs::read_to_string(root().join("docs/ai-interface.md")).unwrap();
    for t in tools::all().into_iter().chain(tools::gui()) {
        assert!(
            doc.contains(&format!("`{}`", t.name)),
            "docs/ai-interface.md lacks {}",
            t.name
        );
    }
    // A read-only server offers no tool that changes the session.
    let read_only = tools::for_mode(Mode::ReadOnly, false);
    assert!(read_only.iter().all(|t| !t.changes));
    assert!(!read_only
        .iter()
        .any(|t| t.name == "proposal_add" || t.name == "tcl_eval"));
}

#[test]
fn fifo_replies_match_the_goldens() {
    let Some(api) = api(Mode::ReadOnly, "fifo") else {
        return;
    };
    let open = call(
        &api,
        "session_open",
        json!({"script": "tests/fpv/fifo/fifo.jg.tcl"}),
    );
    golden("fifo_open", &open);
    golden("fifo_summary", &call(&api, "session_summary", json!({})));
    golden("fifo_properties", &call(&api, "property_list", json!({})));
    golden(
        "fifo_property",
        &call(&api, "property_get", json!({"name": "a_data"})),
    );
    golden("fifo_hierarchy", &call(&api, "design_hierarchy", json!({})));
    golden(
        "fifo_cone",
        &call(
            &api,
            "property_cone",
            json!({"name": "a_data", "helpers": true}),
        ),
    );
    golden(
        "fifo_source",
        &call(
            &api,
            "design_source",
            json!({"file": "tests/fpv/fifo/fifo.sv", "first_line": 38, "last_line": 48}),
        ),
    );
    golden(
        "fifo_check",
        &call(
            &api,
            "script_check",
            json!({"script": "tests/fpv/fifo/fifo.jg.tcl"}),
        ),
    );
    // Filters.
    let covers = call(
        &api,
        "property_list",
        json!({"kind": "cover", "status": "covered"}),
    );
    let names: Vec<&str> = covers["properties"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["c_full", "c_status3"]);
    // The trace's values depend on the engine that found it; what must
    // hold is that it shows the failure: count is 3 where a_wrong fails.
    let t = call(&api, "trace_get", json!({"name": "a_wrong"}));
    let hit = t["hit_at"][0].as_u64().unwrap() as usize;
    assert_eq!(hit as u64 + 1, t["frames"].as_u64().unwrap());
    assert_eq!(
        t["signals"][0]["name"], "fifo.count",
        "the property's own signal comes first"
    );
    assert_eq!(t["signals"][0]["values"][hit], "3");
    assert_eq!(t["signals"][0]["values"].as_array().unwrap().len(), hit + 1);
    // A window and a choice of signals.
    let w = call(
        &api,
        "trace_get",
        json!({"name": "a_wrong", "signals": ["push", "u_wp.*"], "first": hit - 1}),
    );
    let names: Vec<&str> = w["signals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"fifo.push") && names.contains(&"fifo.u_wp.p"),
        "{names:?}"
    );
    assert!(
        names
            .iter()
            .all(|n| *n == "fifo.push" || n.starts_with("fifo.u_wp.")),
        "{names:?}"
    );
    assert_eq!(w["signals"][0]["values"].as_array().unwrap().len(), 2);
    // Errors are data with a code.
    let e = api
        .call("trace_get", &json!({"name": "a_data"}))
        .unwrap_err();
    assert_eq!(e.code, "no_trace");
    let e = api
        .call("property_get", &json!({"name": "nope"}))
        .unwrap_err();
    assert_eq!(e.code, "not_found");
    // Nothing outside the root is opened or read.
    let e = api
        .call("session_open", &json!({"script": "/etc/hostname"}))
        .unwrap_err();
    assert_eq!(e.code, "path");
    let e = api
        .call("design_source", &json!({"file": "../../../etc/hostname"}))
        .unwrap_err();
    assert_eq!(e.code, "path");
    // A read-only interface changes nothing.
    let e = api
        .call(
            "proposal_add",
            &json!({"kind": "assume", "expr": "!push", "rationale": "x"}),
        )
        .unwrap_err();
    assert_eq!(e.code, "mode");
    let e = api
        .call("tcl_eval", &json!({"command": "clear -all"}))
        .unwrap_err();
    assert_eq!(e.code, "mode");
}

#[test]
fn counter_replies_match_the_goldens() {
    let Some(api) = api(Mode::ReadOnly, "counter") else {
        return;
    };
    let open = call(
        &api,
        "session_open",
        json!({"script": "tests/fpv/counter/counter.vcf.tcl"}),
    );
    golden("counter_open", &open);
    golden(
        "counter_properties",
        &call(&api, "property_list", json!({})),
    );
    // From files: no script, no properties yet.
    let open = call(
        &api,
        "session_open",
        json!({"files": ["tests/fpv/counter/counter.sv"], "top": "counter", "clock": "clk", "reset": "!rst_n"}),
    );
    golden("counter_open_files", &open);
    // A script given as a design file is pointed to the right argument.
    let e = api
        .call(
            "session_open",
            &json!({"files": ["tests/fpv/counter/counter.jg.tcl"]}),
        )
        .unwrap_err();
    assert!(
        e.code == "arguments" && e.message.contains("`script`"),
        "{e:?}"
    );
    // A setup that fails says why, as data: this file is not SystemVerilog.
    let open = call(&api, "session_open", json!({"files": ["README.md"]}));
    assert_eq!(open["ok"], false);
    assert!(
        open["error"].as_str().is_some_and(|e| !e.is_empty()),
        "{open}"
    );
}

/// In propose mode a change waits for a person; in automatic mode it is
/// applied, tagged, and its effect on results is said and undone.
#[test]
fn proposals_wait_for_a_person_and_ai_changes_are_tagged_and_undone() {
    let Some(api) = api(Mode::Propose, "propose") else {
        return;
    };
    call(
        &api,
        "session_open",
        json!({"script": "tests/fpv/fifo/fifo.jg.tcl"}),
    );
    let p = call(
        &api,
        "proposal_add",
        json!({"kind": "assume", "name": "m_no_push", "expr": "!push", "rationale": "try an empty FIFO"}),
    );
    assert_eq!(p["proposal"]["state"], "pending");
    let id = p["proposal"]["id"].as_u64().unwrap() as u32;
    // Not in the session yet.
    assert_eq!(
        call(&api, "property_list", json!({"origin": "ai"}))["properties"],
        json!([])
    );
    assert_eq!(
        call(&api, "session_summary", json!({}))["ai"]["pending_proposals"],
        1
    );
    // What the session would reject is rejected when proposed.
    let e = api
        .call(
            "proposal_add",
            &json!({"kind": "assert", "expr": "no_such_signal == 1", "rationale": "x"}),
        )
        .unwrap_err();
    assert_eq!(e.code, "rejected");
    assert!(e.message.contains("no_such_signal"), "{}", e.message);
    let e = api
        .call(
            "proposal_add",
            &json!({"kind": "cut_point", "signal": "nope", "rationale": "x"}),
        )
        .unwrap_err();
    assert_eq!(e.code, "rejected");
    // A Tcl command waits too.
    let t = call(
        &api,
        "tcl_eval",
        json!({"command": "set_prove_time_limit 5s", "rationale": "faster"}),
    );
    assert_eq!(t["proposal"]["state"], "pending");
    assert_eq!(t["proposal"]["kind"], "command");
    let command = t["proposal"]["id"].as_u64().unwrap() as u32;
    assert_eq!(
        api.decide(command, false).unwrap()["proposal"]["state"],
        "rejected"
    );
    // The person accepts the assumption: it is in the session, tagged.
    assert_eq!(
        api.decide(id, true).unwrap()["proposal"]["state"],
        "applied"
    );
    let ai = call(&api, "property_list", json!({"origin": "ai"}));
    assert_eq!(ai["properties"][0]["name"], "m_no_push");
    assert_eq!(api.decide(id, true).unwrap_err().code, "rejected");
    // A result found with it in the model says so.
    let r = call(
        &api,
        "prove",
        json!({"properties": ["c_full", "a_count_max"], "time_limit_s": 30}),
    );
    assert_eq!(r["counts"], json!({"proven": 1, "unreachable": 1}));
    for p in r["results"].as_array().unwrap() {
        assert_eq!(p["under_ai"], json!(["m_no_push"]), "{p}");
    }
    // Undo removes it and resets exactly those results.
    let u = call(&api, "proposal_undo", json!({"id": id}));
    assert_eq!(u["undone"], json!([id]));
    assert_eq!(u["reset"], json!(["a_count_max", "c_full"]));
    let after = call(&api, "property_list", json!({"name": "c_full"}));
    assert_eq!(after["properties"][0]["status"], "unprocessed");
    assert_eq!(
        call(&api, "property_list", json!({"origin": "ai"}))["properties"],
        json!([])
    );
    let list = call(&api, "proposal_list", json!({}));
    let states: Vec<&str> = list["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["undone", "rejected"]);
}

#[test]
fn a_cut_point_is_tagged_and_a_prove_can_run_without_waiting() {
    let Some(api) = api(Mode::Auto, "auto") else {
        return;
    };
    call(
        &api,
        "session_open",
        json!({"script": "tests/fpv/fifo/fifo.jg.tcl"}),
    );
    let p = call(
        &api,
        "proposal_add",
        json!({"kind": "cut_point", "signal": "fifo.count", "rationale": "free the counter"}),
    );
    assert_eq!(p["proposal"]["state"], "applied");
    assert_eq!(p["proposal"]["text"], "count");
    let s = call(&api, "session_summary", json!({}));
    assert_eq!(
        (&s["cut_points"], &s["ai"]["cut_points"]),
        (&json!(["count"]), &json!(1))
    );
    // With count free, the bound on it no longer holds; started without
    // waiting, the result comes with prove_status.
    let started = call(
        &api,
        "prove",
        json!({"properties": ["a_count_max"], "wait": false}),
    );
    assert_eq!(started["started"], true);
    let result = loop {
        let st = call(&api, "prove_status", json!({}));
        if st["running"] == false {
            break st["result"].clone();
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(result["results"][0]["status"], "cex", "{result}");
    assert_eq!(result["results"][0]["under_ai"], json!(["stopat count"]));
    // Undo everything: the cut is gone and the result is reset.
    let u = call(&api, "proposal_undo", json!({}));
    assert_eq!(u["reset"], json!(["a_count_max"]));
    assert_eq!(
        call(&api, "session_summary", json!({}))["cut_points"],
        json!([])
    );
    // The escape hatch runs in automatic mode; a new session forgets the
    // proposals of the last one.
    let t = call(
        &api,
        "tcl_eval",
        json!({"command": "get_property_list -include {type cover}"}),
    );
    assert_eq!(
        (&t["ok"], &t["result"]),
        (&json!(true), &json!("c_full c_status3 c_count5"))
    );
    let t = call(&api, "tcl_eval", json!({"command": "no_such_command"}));
    assert_eq!(t["ok"], false);
    call(
        &api,
        "session_open",
        json!({"script": "tests/fpv/counter/counter.jg.tcl"}),
    );
    assert_eq!(
        call(&api, "proposal_list", json!({}))["proposals"],
        json!([])
    );
    let log = call(&api, "session_log", json!({"last": 500}));
    let lines: Vec<&str> = log["lines"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        lines.contains(&"AI-added cut point (stopat) count"),
        "{lines:#?}"
    );
}

/// A window that records what it is asked to do.
struct FakeWindow(std::sync::Mutex<Vec<Value>>);

impl informal_mcp::Gui for FakeWindow {
    fn state(&self) -> Result<Value, String> {
        Ok(
            json!({"selected_property": "a_wrong", "view": "table", "filter": "", "trace": null, "source": null}),
        )
    }
    fn act(&self, action: &Value) -> Result<Value, String> {
        if action["name"] == "nope" {
            return Err("no property named nope".into());
        }
        self.0.lock().unwrap().push(action.clone());
        Ok(json!({"selected_property": action["name"], "view": "trace",
                  "trace": {"property": action["name"], "cursor": action["frame"], "signals": []}}))
    }
}

/// The window tools exist only with a window; they pass the action on and
/// return what the window then shows.
#[test]
fn window_tools_need_a_window_and_drive_it() {
    let work = std::env::temp_dir().join(format!("informal-mcp-gui-{}", std::process::id()));
    let opts = Options {
        mode: Mode::ReadOnly,
        root: root(),
        workdir: work.clone(),
        max_time: Duration::from_secs(5),
    };
    let api = Arc::new(Api::spawn(Vec::new(), work, opts));
    let names = |api: &Api| -> Vec<&'static str> {
        tools::for_mode(api.mode(), api.has_gui())
            .iter()
            .map(|t| t.name)
            .collect()
    };
    assert!(!names(&api).iter().any(|n| n.starts_with("gui_")));
    let e = api.call("gui_state", &json!({})).unwrap_err();
    assert_eq!(e.code, "no_gui");
    let window = Arc::new(FakeWindow(Default::default()));
    api.set_gui(window.clone());
    assert_eq!(
        names(&api).iter().filter(|n| n.starts_with("gui_")).count(),
        12
    );
    golden(
        "tools_gui",
        &tools::gui()
            .iter()
            .map(|t| json!({"name": t.name, "description": t.description, "input": t.input, "output": t.output}))
            .collect::<Vec<_>>()
            .into(),
    );
    let seen = call(&api, "gui_state", json!({}));
    assert_eq!(seen["window"]["selected_property"], "a_wrong");
    let shown = call(
        &api,
        "gui_open_trace",
        json!({"name": "a_wrong", "frame": 4}),
    );
    assert_eq!(shown["done"], "open_trace");
    assert_eq!(shown["window"]["trace"]["cursor"], 4);
    call(
        &api,
        "gui_highlight_source",
        json!({"file": "tests/fpv/fifo/fifo.sv", "first_line": 40}),
    );
    call(&api, "gui_add_signals", json!({"signals": ["fifo.count"]}));
    call(&api, "gui_select_property", json!({"name": "a_data"}));
    call(
        &api,
        "gui_set_view",
        json!({"view": "schematic", "layout_preset": "cex-debug"}),
    );
    call(
        &api,
        "gui_toggle_why_cex",
        json!({"open": true, "slice_only": true}),
    );
    call(
        &api,
        "gui_annotate_trace",
        json!({"annotations": [{"frame": 4, "signal": "fifo.count", "label": "hits 3", "tone": "danger"}]}),
    );
    call(
        &api,
        "gui_present_walkthrough",
        json!({"title": "FIFO CEX", "steps": [{"label": "Cycle 4", "note": "count reaches 3", "frame": 4}]}),
    );
    call(
        &api,
        "gui_open_schematic",
        json!({"name": "a_wrong", "max_depth": 2}),
    );
    call(&api, "gui_capture_view", json!({"target": "trace"}));
    let actions = window.0.lock().unwrap().clone();
    assert_eq!(actions.len(), 10);
    assert_eq!(
        &actions[..4],
        [
            json!({"action": "open_trace", "name": "a_wrong", "frame": 4}),
            json!({"action": "highlight_source", "file": "tests/fpv/fifo/fifo.sv", "first_line": 40, "last_line": 40}),
            json!({"action": "add_signals", "signals": ["fifo.count"]}),
            json!({"action": "select_property", "name": "a_data"}),
        ]
    );
    // What the window refuses comes back as the tool's error.
    let e = api
        .call("gui_select_property", &json!({"name": "nope"}))
        .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        ("rejected", "no property named nope")
    );
    let e = api.call("gui_add_signals", &json!({})).unwrap_err();
    assert_eq!(e.code, "arguments");
    let e = api
        .call("gui_highlight_source", &json!({"file": "x.sv"}))
        .unwrap_err();
    assert_eq!(e.code, "arguments");
}

/// A client on the protocol itself: JSON-RPC lines in, lines out.
struct Client {
    server: Server,
    rx: mpsc::Receiver<Value>,
    next: u64,
}

impl Client {
    fn new(api: Arc<Api>) -> Client {
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let server = Server::new(api, move |m| {
            let _ = tx.lock().unwrap().send(m.clone());
        });
        Client {
            server,
            rx,
            next: 0,
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let msg = json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params});
        self.server.handle(&msg.to_string());
        loop {
            let m = self
                .rx
                .recv_timeout(Duration::from_secs(120))
                .expect("a reply");
            if m["id"] == self.next {
                assert!(m.get("error").is_none(), "{method}: {m}");
                return m["result"].clone();
            }
        }
    }

    /// The structured content of a tool call that did not fail.
    fn tool(&mut self, name: &str, args: Value) -> Value {
        let r = self.request("tools/call", json!({"name": name, "arguments": args}));
        assert_eq!(r["isError"], false, "{name}: {r}");
        // The text block carries the same JSON for clients that read text.
        let text: Value = serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, r["structuredContent"]);
        r["structuredContent"].clone()
    }
}

#[test]
fn an_agent_finds_the_failing_assertion_and_proposes_a_fix() {
    let Some(api) = api(Mode::Auto, "agent") else {
        return;
    };
    let mut c = Client::new(api);
    let init = c.request("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
    assert_eq!(init["protocolVersion"], "2025-06-18");
    assert_eq!(init["serverInfo"]["name"], "informal");
    c.server
        .handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    let listed = c.request("tools/list", json!({}));
    assert_eq!(
        listed["tools"].as_array().unwrap().len(),
        tools::all().len()
    );

    // 1. Open the design and find what fails.
    let open = c.tool(
        "session_open",
        json!({"script": "tests/fpv/fifo/fifo.jg.tcl"}),
    );
    assert_eq!(open["summary"]["properties"]["by_status"]["cex"], 1);
    let failing = c.tool("property_list", json!({"kind": "assert", "status": "cex"}));
    let prop = &failing["properties"][0];
    assert_eq!(prop["name"], "a_wrong");
    // 2. Where it was written, and what it says.
    let line = prop["source"]["line"].as_u64().unwrap();
    let src = c.tool(
        "design_source",
        json!({"file": prop["source"]["file"], "first_line": line, "last_line": line}),
    );
    assert!(src["lines"][0].as_str().unwrap().contains("count != 3'd3"));
    // 3. The trace as data: count walks up to 3, which a 4-entry FIFO may.
    let trace = c.tool(
        "trace_get",
        json!({"name": "a_wrong", "signals": ["count", "push", "pop"]}),
    );
    let hit = trace["hit_at"][0].as_u64().unwrap() as usize;
    let count = trace["signals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "fifo.count")
        .unwrap();
    assert_eq!(count["values"][hit], "3");
    // 4. The fix: the assertion is wrong, not the design. Propose the
    // bound the design keeps, and let the engines say whether it holds.
    let fix = c.tool(
        "proposal_add",
        json!({"kind": "assert", "name": "a_count_legal", "expr": "count <= 3'd4",
               "rationale": "a_wrong forbids count == 3, a legal fill level of a 4-entry FIFO; the bound is 4"}),
    );
    assert_eq!(fix["proposal"]["state"], "applied");
    let proved = c.tool(
        "prove",
        json!({"properties": ["a_count_legal"], "time_limit_s": 30}),
    );
    assert_eq!(proved["results"][0]["status"], "proven");
    assert_eq!(proved["results"][0]["origin"], "ai");

    // MCP App SEP-1865 resource discovery and read (`ui://informal/workbench.html`).
    let resources = c.request("resources/list", json!({}));
    assert_eq!(
        resources["resources"][0]["uri"],
        "ui://informal/workbench.html"
    );
    assert_eq!(
        resources["resources"][0]["mimeType"],
        "text/html;profile=mcp-app"
    );
    let read_res = c.request(
        "resources/read",
        json!({"uri": "ui://informal/workbench.html"}),
    );
    assert_eq!(
        read_res["contents"][0]["mimeType"],
        "text/html;profile=mcp-app"
    );
    assert!(read_res["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("<html"));

    // Protocol errors: an unknown tool and an unknown method.
    c.server
        .handle(r#"{"jsonrpc":"2.0","id":"x","method":"tools/call","params":{"name":"nope"}}"#);
    let e = c.rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(e["error"]["code"], -32602);
    c.server
        .handle(r#"{"jsonrpc":"2.0","id":"y","method":"unknown/method"}"#);
    assert_eq!(
        c.rx.recv_timeout(Duration::from_secs(5)).unwrap()["error"]["code"],
        -32601
    );
    c.server.handle("not json");
    assert_eq!(
        c.rx.recv_timeout(Duration::from_secs(5)).unwrap()["error"]["code"],
        -32700
    );
    // A tool's own error is a result the model can read.
    let r = c.request(
        "tools/call",
        json!({"name": "trace_get", "arguments": {"name": "a_count_legal"}}),
    );
    assert_eq!(r["isError"], true);
    assert!(r["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("no_trace"));
}

#[test]
fn mcp_why_cex_and_formal_app_tools_conform_to_schemas() {
    let Some(api) = api(Mode::Auto, "apps") else {
        return;
    };
    call(
        &api,
        "session_open",
        json!({"script": "tests/fpv/counter/counter.jg.tcl"}),
    );
    call(&api, "prove", json!({}));

    let slice = call(&api, "why_cex", json!({"name": "a_never9"}));
    assert_eq!(slice["schema"], "informal.why_cex/1");
    assert_eq!(slice["status"], "cex");
    assert!(!slice["events"].as_array().unwrap().is_empty());

    let wb = call(
        &api,
        "open_workbench",
        json!({"focus_property": "a_never9"}),
    );
    assert_eq!(wb["schema"], "informal.workbench/1");
    assert_eq!(wb["focus_property"], "a_never9");

    let schem = call(
        &api,
        "schematic_get",
        json!({"name": "a_never9", "max_depth": 2}),
    );
    assert_eq!(schem["schema"], "informal.schematic/1");
    assert!(!schem["nodes"].as_array().unwrap().is_empty());

    let lint = call(&api, "formal_lint", json!({}));
    assert_eq!(lint["schema"], "informal.lint/1");

    let nav = call(&api, "signal_nav", json!({"signal": "counter.q"}));
    assert_eq!(nav["schema"], "informal.signal_nav/1");
    assert!(!nav["declarations"].as_array().unwrap().is_empty());

    let comp = call(&api, "fv_complexity", json!({}));
    assert_eq!(comp["schema"], "informal.complexity/1");

    let pc = call(&api, "prove_cache", json!({"enabled": true, "mode": "rw"}));
    assert_eq!(pc["enabled"], true);

    let cov_init = call(
        &api,
        "check_cov",
        json!({"action": "init", "models": ["coi"]}),
    );
    assert_eq!(cov_init["initialized"], true);
    let cov_meas = call(
        &api,
        "check_cov",
        json!({"action": "measure", "models": ["coi"]}),
    );
    assert_eq!(cov_meas["measured"], true);

    let xp = call(
        &api,
        "check_xprop",
        json!({"action": "init", "targets": ["outputs"]}),
    );
    assert_eq!(xp["initialized"], true);
    let xp_done = call(&api, "check_xprop", json!({"action": "prove"}));
    assert_eq!(xp_done["schema"], "informal.xprop/1");

    let spv = call(&api, "check_spv", json!({"action": "list"}));
    assert_eq!(spv["schema"], "informal.spv/1");
    let conn = call(&api, "check_conn", json!({"action": "list"}));
    assert_eq!(conn["schema"], "informal.conn/1");
    let csr = call(&api, "check_csr", json!({"action": "list"}));
    assert_eq!(csr["schema"], "informal.csr/1");
    let proto = call(&api, "check_protocol", json!({"action": "list"}));
    assert_eq!(proto["schema"], "informal.protocol/1");

    let sec_setup = call(
        &api,
        "check_sec",
        json!({
            "action": "setup",
            "spec_files": ["tests/fpv/counter/counter.sv"],
            "imp_files": ["tests/fpv/counter/counter.sv"],
            "clock": "clk",
            "reset": "!rst_n"
        }),
    );
    assert_eq!(sec_setup["schema"], "informal.sec/1");
    assert_eq!(sec_setup["configured"], true);
    let sec_done = call(&api, "check_sec", json!({"action": "prove"}));
    assert_eq!(sec_done["schema"], "informal.sec/1");
    assert_eq!(sec_done["checks"][0]["status"], "proven");
}

#[test]
fn slice1_aci_diagnose_proposal_validate_poka_yoke_and_prompts() {
    let Some(api) = api(Mode::Propose, "slice1") else {
        return;
    };
    call(
        &api,
        "session_open",
        json!({"script": "tests/fpv/counter/counter.jg.tcl"}),
    );
    call(&api, "prove", json!({}));

    // 1. Poka-Yoke errors: typo property name yields fuzzy candidate + suggested_tool + hint.
    let typo = api
        .call("property_get", &json!({"name": "a_nevr9"}))
        .unwrap_err();
    assert_eq!(typo.code, "not_found");
    assert_eq!(typo.suggested_tool, Some("property_list"));
    assert!(typo.hint.is_some());
    assert!(typo.candidates.iter().any(|c| c == "a_never9"));

    let no_trace = api
        .call("cex_diagnose", &json!({"name": "a_max"}))
        .unwrap_err();
    assert_eq!(no_trace.code, "no_trace");
    assert_eq!(no_trace.suggested_tool, Some("property_get"));
    assert!(no_trace.hint.is_some());

    // 2. Single-call `cex_diagnose`: causal slice + trimmed active trace + signal roles + drivers + triage_hint.
    let diag = call(&api, "cex_diagnose", json!({"name": "a_never9"}));
    assert_eq!(diag["schema"], "informal.cex_diagnose/1");
    assert_eq!(diag["property"]["name"], "a_never9");
    assert_eq!(diag["property"]["status"], "cex");
    assert!(!diag["causal_slice"]["events"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(!diag["active_trace"]["signals"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(!diag["driver_snippets"].as_array().unwrap().is_empty());
    assert!(!diag["triage_hint"].as_str().unwrap().is_empty());
    let roles = diag["signal_roles"].as_object().unwrap();
    assert!(roles
        .values()
        .any(|v| v == "primary_input" || v == "dut_output"));

    // 3. Single-call `convergence_diagnose`: cone + complexity + ranked recommendations.
    let conv = call(&api, "convergence_diagnose", json!({"name": "a_max"}));
    assert_eq!(conv["schema"], "informal.convergence_diagnose/1");
    assert!(!conv["recommendations"].as_array().unwrap().is_empty());

    // 4. `proposal_validate`: zero-side-effect scratch-task soundness oracle.
    let summary_before = call(&api, "session_summary", json!({}));
    let props_before = call(&api, "property_list", json!({}));

    // 4a. Over-constraining assumption (`!en`) kills reachable cover `c_done` -> REJECT.
    let killed = call(
        &api,
        "proposal_validate",
        json!({"kind": "assume", "expr": "!en", "targets": ["a_never9"], "check_covers": true}),
    );
    assert_eq!(killed["schema"], "informal.validate/1");
    assert_eq!(killed["verdict"], "reject");
    assert_eq!(killed["ready_to_propose"], false);
    assert!(killed["lost_covers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c == "c_done"));

    // 4b. Contradictory assumption (`en && !en`) -> environment conflict -> REJECT.
    let conflict = call(
        &api,
        "proposal_validate",
        json!({"kind": "assume", "expr": "en && !en", "check_covers": false}),
    );
    assert_eq!(conflict["verdict"], "reject");
    assert_eq!(conflict["environment"]["conflict"], true);

    // 4c. DUT-output constraint in `assume` -> SVA-FPV-001 static overconstraint warning.
    let out_warn = call(
        &api,
        "proposal_validate",
        json!({"kind": "assume", "expr": "q <= 4'd8", "targets": ["a_never9"], "check_covers": false, "check_dead_end": false}),
    );
    assert!(out_warn["overconstraint_warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w.as_str().unwrap().contains("SVA-FPV-001")));

    // 4d. Sound helper assertion (`q <= 4'd9`) -> PASS.
    let good_lemma = call(
        &api,
        "proposal_validate",
        json!({"kind": "assert", "name": "h_q_bound", "expr": "q <= 4'd9", "targets": []}),
    );
    assert_eq!(good_lemma["verdict"], "pass");
    assert_eq!(good_lemma["ready_to_propose"], true);
    assert_eq!(good_lemma["candidate_result"]["status"], "proven");

    // 4e. False assertion (`q != 4'd9`) -> REJECT.
    let bad_lemma = call(
        &api,
        "proposal_validate",
        json!({"kind": "assert", "name": "h_false", "expr": "q != 4'd9", "targets": []}),
    );
    assert_eq!(bad_lemma["verdict"], "reject");
    assert_eq!(bad_lemma["candidate_result"]["status"], "cex");

    // 4f. Verify ZERO mutation to the real session after all 5 scratch validations!
    assert_eq!(call(&api, "session_summary", json!({})), summary_before);
    assert_eq!(call(&api, "property_list", json!({})), props_before);

    // 5. MCP `prompts/list`, hydrated `prompts/get`, and unknown-tool fuzzy suggestion.
    let (tx, rx) = mpsc::channel::<Value>();
    let server = Server::new(api.clone(), move |msg| {
        let _ = tx.send(msg.clone());
    });
    server.handle(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#);
    let init = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(init["result"]["capabilities"]["prompts"].is_object());

    server.handle(r#"{"jsonrpc":"2.0","id":2,"method":"prompts/list"}"#);
    let plist = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let pnames: Vec<&str> = plist["result"]["prompts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();
    assert!(pnames.contains(&"triage_cex"));
    assert!(pnames.contains(&"close_undetermined"));
    assert!(pnames.contains(&"spec_to_sva"));
    assert!(pnames.contains(&"audit_signoff"));

    server.handle(
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "prompts/get",
            "params": {"name": "spec_to_sva", "arguments": {"requirement": "q never exceeds 9"}}
        })
        .to_string(),
    );
    let pget = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let ptxt = pget["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    assert!(ptxt.contains("counter"));
    assert!(ptxt.contains("en"));
    assert!(ptxt.contains("q[3:0]"));

    // Unknown tool suggestion
    server.handle(r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"cex_diagnoze","arguments":{}}}"#);
    let unk = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(unk["error"]["code"], -32602);
    assert!(unk["error"]["message"]
        .as_str()
        .unwrap()
        .contains("cex_diagnose"));

    // 6. `gui_present_cex` dispatches atomic `present_cex` with auto-synthesized steps & annotations.
    struct RecordingGui(std::sync::Mutex<Vec<Value>>);
    impl informal_mcp::Gui for RecordingGui {
        fn state(&self) -> Result<Value, String> {
            Ok(json!({"selected_property": "a_never9", "view": "trace"}))
        }
        fn act(&self, action: &Value) -> Result<Value, String> {
            self.0.lock().unwrap().push(action.clone());
            Ok(
                json!({"selected_property": "a_never9", "view": "trace", "layout_preset": "cex-debug"}),
            )
        }
    }
    let rec = Arc::new(RecordingGui(std::sync::Mutex::new(Vec::new())));
    api.set_gui(rec.clone());
    let presented = call(
        &api,
        "gui_present_cex",
        json!({"name": "a_never9", "slice_only": true}),
    );
    assert_eq!(presented["done"], "present_cex");
    let acts = rec.0.lock().unwrap();
    assert_eq!(acts.len(), 1);
    assert_eq!(acts[0]["action"], "present_cex");
    assert_eq!(acts[0]["layout_preset"], "cex-debug");
    assert_eq!(acts[0]["slice_only"], true);
    assert!(!acts[0]["steps"].as_array().unwrap().is_empty());
    assert!(!acts[0]["annotations"].as_array().unwrap().is_empty());
}

#[test]
fn spec_audit_verifies_requirements_boundaries_and_signoff_gates() {
    let Some(api) = api(Mode::Auto, "spec-audit") else {
        return;
    };
    call(
        &api,
        "session_open",
        json!({"script": "tests/fpv/fifo/fifo.jg.tcl"}),
    );

    // 1. Zero-arg auto-discovery of `tests/fpv/fifo/fvp.spec.json` + 2QBF dead-end check.
    let summary_before = call(&api, "session_summary", json!({}));
    let audit = call(&api, "spec_audit", json!({"check_dead_end": true}));
    assert_eq!(audit["schema"], "informal.spec_audit/1");
    assert_eq!(audit["signoff_ready"], true, "{audit:#}");
    assert_eq!(audit["boundary_audit"]["ok"], true);
    assert_eq!(audit["boundary_audit"]["dead_end_checked"], true);
    assert_eq!(audit["boundary_audit"]["dead_end_found"], false);
    assert_eq!(audit["summary"]["total_requirements"], 5);
    assert_eq!(audit["summary"]["verified"], 5);
    assert_eq!(audit["summary"]["unmapped_assertions"], json!(["a_wrong"]));
    assert!(audit["uncovered_requirements"]
        .as_array()
        .unwrap()
        .is_empty());
    // Zero side effects on session!
    assert_eq!(call(&api, "session_summary", json!({})), summary_before);

    // 2. Hydrated `audit_signoff` prompt picks up `fvp.spec.json` and `spec_audit`.
    let (tx, rx) = mpsc::channel::<Value>();
    let server = Server::new(api.clone(), move |msg| {
        let _ = tx.send(msg.clone());
    });
    server.handle(
        &json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "prompts/get",
            "params": {"name": "audit_signoff", "arguments": {}}
        })
        .to_string(),
    );
    let pget = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let ptxt = pget["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    assert!(ptxt.contains("spec_audit"));
    assert!(ptxt.contains("fvp.spec.json"));

    // 3. Add a vacuous assertion + an unprocessed assertion, and audit an inline spec
    //    exercising verified, cex, missing_assert, missing_cover, vacuous, undetermined,
    //    and mutation_score_floor / coi_coverage_floor gates.
    call(
        &api,
        "proposal_add",
        json!({"kind": "assert", "name": "a_vac_false", "expr": "1'b0 |-> full", "rationale": "vacuity test"}),
    );
    call(&api, "prove", json!({"names": ["a_vac_false"]}));
    call(
        &api,
        "proposal_add",
        json!({"kind": "assert", "name": "a_unproved", "expr": "count <= 3'd4", "rationale": "unproved test"}),
    );

    let rich_spec = json!({
        "schema": "informal.spec/1",
        "design": {"top": "fifo", "clocks": ["clk"], "reset": "!rst_n"},
        "boundary": {
            "allowed_assume_inputs": ["push", "pop", "din"],
            "forbidden_assume_signals": ["full", "empty", "dout", "count"]
        },
        "requirements": [
            {"id": "REQ-OK", "title": "Valid", "assertions": ["a_count_max"], "covers": ["c_full"]},
            {"id": "REQ-CEX", "title": "Fails", "assertions": ["a_wrong"], "covers": ["c_full"]},
            {"id": "REQ-MISS-A", "title": "Missing assert", "assertions": ["a_missing_xyz"], "covers": []},
            {"id": "REQ-MISS-C", "title": "Unreachable cover", "assertions": ["a_status"], "covers": ["c_count5"]},
            {"id": "REQ-VAC", "title": "Vacuous assert", "assertions": ["a_vac_false"], "covers": []},
            {"id": "REQ-UNDET", "title": "Unprocessed assert", "assertions": ["a_unproved"], "covers": []}
        ],
        "signoff": {
            "proof_target": "proven",
            "max_overconstraints": 0,
            "forbid_vacuous": true,
            "coi_coverage_floor": 80.0,
            "mutation_score_floor": 90.0
        }
    });
    let diag = call(
        &api,
        "spec_audit",
        json!({"spec": rich_spec, "mutation_score": 75.0}),
    );
    assert_eq!(diag["signoff_ready"], false);
    assert_eq!(diag["summary"]["verified"], 1);
    assert_eq!(diag["summary"]["cex"], 1);
    assert_eq!(diag["summary"]["missing_assert"], 1);
    assert_eq!(diag["summary"]["missing_cover"], 1);
    assert_eq!(diag["summary"]["vacuous"], 1);
    assert_eq!(diag["summary"]["undetermined"], 1);

    let status_of = |id: &str| -> String {
        diag["requirements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .unwrap()["status"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(status_of("REQ-OK"), "verified");
    assert_eq!(status_of("REQ-CEX"), "cex");
    assert_eq!(status_of("REQ-MISS-A"), "missing_assert");
    assert_eq!(status_of("REQ-MISS-C"), "missing_cover");
    assert_eq!(status_of("REQ-VAC"), "vacuous");
    assert_eq!(status_of("REQ-UNDET"), "undetermined");

    let gate_ok = |g: &str| -> bool {
        diag["gates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["gate"] == g)
            .unwrap()["ok"]
            .as_bool()
            .unwrap()
    };
    assert!(!gate_ok("coi_coverage"));
    assert!(!gate_ok("mutation_coverage"));

    // 4. Inject an illegal DUT-output assumption (`!full`) -> boundary_audit catches it
    //    and marks requirements `overconstrained`.
    call(
        &api,
        "proposal_add",
        json!({"kind": "assume", "name": "m_bad_out", "expr": "!full", "rationale": "bad output constraint"}),
    );
    let cheated = call(&api, "spec_audit", json!({}));
    assert_eq!(cheated["signoff_ready"], false);
    assert_eq!(cheated["boundary_audit"]["ok"], false);
    assert_eq!(
        cheated["boundary_audit"]["overconstrained_assumptions"][0]["name"],
        "m_bad_out"
    );
    assert_eq!(cheated["summary"]["overconstrained"], 5);
}

#[test]
fn spec_audit_forbid_under_ai_gate_blocks_ai_dependent_proofs() {
    let Some(api) = api(Mode::Auto, "spec-forbid-ai") else {
        return;
    };
    call(
        &api,
        "session_open",
        json!({"script": "tests/fpv/fifo/fifo.jg.tcl"}),
    );

    // Add a legal primary-input AI assumption and re-prove `a_count_max` under it.
    call(
        &api,
        "proposal_add",
        json!({
            "kind": "assume",
            "name": "m_ai_din",
            "expr": "din == 8'h00",
            "rationale": "AI input constraint for provenance gate test"
        }),
    );
    call(&api, "prove", json!({"properties": ["a_count_max"]}));
    let prop = call(&api, "property_get", json!({"name": "a_count_max"}));
    assert_eq!(prop["property"]["status"], "proven");
    assert_eq!(prop["property"]["under_ai"], json!(["m_ai_din"]));

    let make_spec = |forbid_under_ai: bool| {
        json!({
            "schema": "informal.spec/1",
            "design": {"top": "fifo", "clocks": ["clk"], "reset": "!rst_n"},
            "boundary": {
                "allowed_assume_inputs": ["push", "pop", "din"],
                "forbidden_assume_signals": [
                    "full", "empty", "dout", "status", "count", "wp", "rp", "mem", "u_wp.*", "u_rp.*"
                ]
            },
            "requirements": [
                {"id": "REQ-AI-1", "title": "Occupancy bounded", "assertions": ["a_count_max"], "covers": ["c_full"]}
            ],
            "signoff": {
                "proof_target": "proven",
                "forbid_under_ai": forbid_under_ai
            }
        })
    };

    // Allowed when `forbid_under_ai` is false.
    let ok_when_allowed = call(&api, "spec_audit", json!({"spec": make_spec(false)}));
    assert_eq!(ok_when_allowed["signoff_ready"], true);
    assert_eq!(ok_when_allowed["summary"]["verified"], 1);
    assert_eq!(
        ok_when_allowed["requirements"][0]["assertions"][0]["under_ai"],
        json!(["m_ai_din"])
    );

    // Blocked when `forbid_under_ai` is true.
    let blocked = call(&api, "spec_audit", json!({"spec": make_spec(true)}));
    assert_eq!(blocked["signoff_ready"], false);
    assert_eq!(blocked["summary"]["verified"], 0);
    assert_eq!(blocked["summary"]["undetermined"], 1);
    assert_eq!(blocked["requirements"][0]["status"], "undetermined");
    assert!(blocked["requirements"][0]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s
            .as_str()
            .unwrap()
            .contains("stands on AI-added assumption/cutpoint [m_ai_din]")));
    let ai_gate = blocked["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["gate"] == "ai_provenance")
        .unwrap();
    assert_eq!(ai_gate["ok"], false);
    assert_eq!(ai_gate["actual"], "1 under_ai dependencies");
    assert!(ai_gate["detail"]
        .as_str()
        .unwrap()
        .contains("a_count_max -> m_ai_din"));

    // Removing the AI assumption and re-proving restores signoff readiness.
    call(&api, "proposal_undo", json!({}));
    call(&api, "prove", json!({"properties": ["a_count_max"]}));
    let recovered = call(&api, "spec_audit", json!({"spec": make_spec(true)}));
    assert_eq!(recovered["signoff_ready"], true);
    let ai_gate_ok = recovered["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["gate"] == "ai_provenance")
        .unwrap();
    assert_eq!(ai_gate_ok["ok"], true);
    assert_eq!(ai_gate_ok["actual"], "0 under_ai dependencies");
}

#[test]
fn spec_audit_require_dead_end_check_gate_fails_when_unchecked() {
    let Some(api) = api(Mode::ReadOnly, "spec-dead-end-req") else {
        return;
    };
    call(
        &api,
        "session_open",
        json!({"script": "tests/fpv/fifo/fifo.jg.tcl"}),
    );

    let spec = json!({
        "schema": "informal.spec/1",
        "design": {"top": "fifo", "clocks": ["clk"], "reset": "!rst_n"},
        "boundary": {
            "allowed_assume_inputs": ["push", "pop", "din"],
            "forbidden_assume_signals": [
                "full", "empty", "dout", "status", "count", "wp", "rp", "mem", "u_wp.*", "u_rp.*"
            ]
        },
        "requirements": [
            {"id": "REQ-DE-1", "title": "Occupancy bounded", "assertions": ["a_count_max"], "covers": ["c_full"]}
        ],
        "signoff": {
            "proof_target": "proven",
            "require_dead_end_check": true
        }
    });

    // When `check_dead_end` is false, `zero_dead_ends` gate fails signoff even though REQ-DE-1 is verified.
    let unchecked = call(
        &api,
        "spec_audit",
        json!({"spec": spec, "check_dead_end": false}),
    );
    assert_eq!(unchecked["signoff_ready"], false);
    assert_eq!(unchecked["summary"]["verified"], 1);
    assert!(unchecked["uncovered_requirements"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(unchecked["boundary_audit"]["ok"], true);
    assert_eq!(unchecked["boundary_audit"]["dead_end_checked"], false);
    assert_eq!(unchecked["boundary_audit"]["dead_end_found"], false);
    let de_gate = unchecked["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["gate"] == "zero_dead_ends")
        .unwrap();
    assert_eq!(de_gate["ok"], false);
    assert_eq!(de_gate["actual"], "not checked (pass check_dead_end=true)");
    assert!(de_gate["expected"]
        .as_str()
        .unwrap()
        .contains("(2QBF required)"));
    assert!(unchecked["next_actions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a.as_str().unwrap().contains("check_dead_end=true")));

    // Re-running with `check_dead_end: true` discharges the gate.
    let checked = call(
        &api,
        "spec_audit",
        json!({"spec": spec, "check_dead_end": true}),
    );
    assert_eq!(checked["signoff_ready"], true);
    assert_eq!(checked["boundary_audit"]["dead_end_checked"], true);
    assert_eq!(checked["boundary_audit"]["dead_end_found"], false);
    let de_gate_ok = checked["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["gate"] == "zero_dead_ends")
        .unwrap();
    assert_eq!(de_gate_ok["ok"], true);
}

#[test]
fn spec_audit_allow_internal_assumes_waives_internal_state_only() {
    let Some(api) = api(Mode::Auto, "spec-allow-internal") else {
        return;
    };
    call(
        &api,
        "session_open",
        json!({"script": "tests/fpv/fifo/fifo.jg.tcl"}),
    );
    call(
        &api,
        "tcl_eval",
        json!({"command": "assume -name m_internal_cnt {count <= 3'd4}"}),
    );

    let make_spec = |forbidden: Value, allow_internal: Value| {
        json!({
            "schema": "informal.spec/1",
            "design": {"top": "fifo", "clocks": ["clk"], "reset": "!rst_n"},
            "boundary": {
                "allowed_assume_inputs": ["push", "pop", "din"],
                "forbidden_assume_signals": forbidden,
                "allow_internal_assumes": allow_internal
            },
            "requirements": [
                {"id": "REQ-INT-1", "title": "Occupancy bounded", "assertions": ["a_count_max"], "covers": ["c_full"]}
            ],
            "signoff": {"proof_target": "proven", "max_overconstraints": 0}
        })
    };

    // 1. Unwaived internal_state assumption is flagged as over-constraint.
    let unwaived = call(
        &api,
        "spec_audit",
        json!({"spec": make_spec(json!(["full", "empty", "dout", "status"]), json!([]))}),
    );
    assert_eq!(unwaived["signoff_ready"], false);
    assert_eq!(unwaived["boundary_audit"]["ok"], false);
    assert_eq!(unwaived["summary"]["overconstrained"], 1);
    assert_eq!(
        unwaived["boundary_audit"]["overconstrained_assumptions"][0]["name"],
        "m_internal_cnt"
    );
    assert_eq!(
        unwaived["boundary_audit"]["overconstrained_assumptions"][0]["roles"],
        json!(["internal_state"])
    );
    assert_eq!(
        unwaived["boundary_audit"]["overconstrained_assumptions"][0]["forbidden_signals"],
        json!(["count"])
    );

    // 2. Explicitly waiving `count` via `boundary.allow_internal_assumes` passes.
    let waived = call(
        &api,
        "spec_audit",
        json!({"spec": make_spec(json!(["full", "empty", "dout", "status"]), json!(["count"]))}),
    );
    assert_eq!(waived["signoff_ready"], true);
    assert_eq!(waived["boundary_audit"]["ok"], true);
    assert!(waived["boundary_audit"]["overconstrained_assumptions"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(waived["summary"]["overconstrained"], 0);
    assert_eq!(waived["summary"]["verified"], 1);

    // 3. `forbidden_assume_signals` takes precedence over `allow_internal_assumes`.
    let forbidden_override = call(
        &api,
        "spec_audit",
        json!({"spec": make_spec(json!(["full", "empty", "dout", "status", "count"]), json!(["count"]))}),
    );
    assert_eq!(forbidden_override["signoff_ready"], false);
    assert_eq!(forbidden_override["boundary_audit"]["ok"], false);
    assert!(
        forbidden_override["boundary_audit"]["overconstrained_assumptions"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("boundary.forbidden_assume_signals")
    );
}

#[test]
fn spec_audit_bounded_proven_target_respects_min_bound_and_proven() {
    let Some(api) = api(Mode::Auto, "spec-bounded") else {
        return;
    };
    call(
        &api,
        "session_open",
        json!({"script": "tests/fpv/counter/counter.jg.tcl"}),
    );

    // `q != 4'd9` fails at depth 10, so `-bound 4` yields `bounded_proven` at bound 4.
    call(
        &api,
        "tcl_eval",
        json!({"command": "assert -name a_never9_b4 -bound 4 {q != 4'd9}"}),
    );
    call(&api, "prove", json!({"properties": ["a_never9_b4"]}));
    let prop = call(&api, "property_get", json!({"name": "a_never9_b4"}));
    assert_eq!(prop["property"]["status"], "bounded_proven");
    assert_eq!(prop["property"]["depth"], 4);

    let make_spec = |proof_target: &str, min_bound: u32| {
        json!({
            "schema": "informal.spec/1",
            "design": {"top": "counter", "clocks": ["clk"], "reset": "!rst_n"},
            "boundary": {
                "allowed_assume_inputs": ["en"],
                "forbidden_assume_signals": ["q", "done", "nxt", "wrap"]
            },
            "requirements": [
                {"id": "REQ-BOUND", "title": "Bounded check", "assertions": ["a_never9_b4"], "covers": ["c_done"]},
                {"id": "REQ-PROVEN", "title": "Unbounded check", "assertions": ["a_max"], "covers": ["c_done"]}
            ],
            "signoff": {
                "proof_target": proof_target,
                "min_bound": min_bound
            }
        })
    };

    // 1. `proof_target: "proven"` rejects `bounded_proven`.
    let strict_unbounded = call(&api, "spec_audit", json!({"spec": make_spec("proven", 0)}));
    assert_eq!(strict_unbounded["signoff_ready"], false);
    assert_eq!(strict_unbounded["summary"]["verified"], 1);
    assert_eq!(strict_unbounded["summary"]["undetermined"], 1);
    assert_eq!(
        strict_unbounded["requirements"][0]["status"],
        "undetermined"
    );
    assert_eq!(strict_unbounded["requirements"][1]["status"], "verified");

    // 2. `proof_target: "bounded_proven"` with `min_bound: 5` fails bound 4 < 5, while `proven` passes.
    let too_shallow = call(
        &api,
        "spec_audit",
        json!({"spec": make_spec("bounded_proven", 5)}),
    );
    assert_eq!(too_shallow["signoff_ready"], false);
    assert_eq!(too_shallow["summary"]["verified"], 1);
    assert_eq!(too_shallow["summary"]["undetermined"], 1);
    assert_eq!(too_shallow["requirements"][0]["status"], "undetermined");
    assert_eq!(too_shallow["requirements"][1]["status"], "verified");

    // 3. `proof_target: "bounded_proven"` with `min_bound: 4` passes both `bounded_proven` (4 >= 4) and `proven`.
    let meets_bound = call(
        &api,
        "spec_audit",
        json!({"spec": make_spec("bounded_proven", 4)}),
    );
    assert_eq!(meets_bound["signoff_ready"], true);
    assert_eq!(meets_bound["summary"]["verified"], 2);
    assert_eq!(meets_bound["summary"]["undetermined"], 0);
    assert_eq!(meets_bound["requirements"][0]["status"], "verified");
    assert_eq!(meets_bound["requirements"][1]["status"], "verified");
}

#[test]
fn spec_audit_dead_end_found_gates_signoff_and_marks_overconstrained() {
    let Some(api) = api(Mode::Auto, "spec-dead-end-found") else {
        return;
    };
    call(
        &api,
        "session_open",
        json!({"script": "tests/fpv/counter/counter.jg.tcl"}),
    );

    // Baseline `counter/fvp.spec.json` is clean with `check_dead_end: true`.
    let clean = call(&api, "spec_audit", json!({"check_dead_end": true}));
    assert_eq!(clean["signoff_ready"], true);
    assert_eq!(clean["boundary_audit"]["dead_end_found"], false);

    // `counter.jg.tcl` already has `m_en_burst {en |=> en}`. Adding `en |=> !en` on primary input `en`
    // keeps `en == 0` alive forever (`conflict: false`) and obeys the primary-input boundary,
    // but traps any trace with `en == 1` into a 2QBF dead-end on the next cycle.
    call(
        &api,
        "tcl_eval",
        json!({"command": "assume -name m_en_trap {en |=> !en}"}),
    );

    let trapped = call(&api, "spec_audit", json!({"check_dead_end": true}));
    assert_eq!(trapped["signoff_ready"], false);
    assert_eq!(trapped["boundary_audit"]["ok"], false);
    assert_eq!(trapped["boundary_audit"]["conflict"], false);
    assert!(trapped["boundary_audit"]["overconstrained_assumptions"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(trapped["boundary_audit"]["dead_end_checked"], true);
    assert_eq!(trapped["boundary_audit"]["dead_end_found"], true);
    assert_eq!(trapped["summary"]["overconstrained"], 3);
    assert_eq!(trapped["requirements"][0]["status"], "overconstrained");
    assert!(trapped["requirements"][0]["next_action"]
        .as_str()
        .unwrap()
        .contains("2QBF dead-end trap state"));

    let gate_ok = |g: &str| -> bool {
        trapped["gates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["gate"] == g)
            .unwrap()["ok"]
            .as_bool()
            .unwrap()
    };
    assert!(gate_ok("zero_overconstraints"));
    assert!(!gate_ok("zero_dead_ends"));
}

#[test]
fn spec_audit_detects_disabled_assertion_in_named_task() {
    let Some(api) = api(Mode::Auto, "spec-task-disabled") else {
        return;
    };
    call(
        &api,
        "session_open",
        json!({"script": "tests/fpv/fifo/fifo.jg.tcl"}),
    );

    // Create a named task `t_sub` and disable `a_count_max` ONLY in `t_sub` (`<embedded>` remains enabled).
    call(
        &api,
        "tcl_eval",
        json!({"command": "task -create t_sub -copy_asserts -copy_covers; assert -disable -task t_sub a_count_max"}),
    );
    call(
        &api,
        "prove",
        json!({"task": "t_sub", "properties": ["a_full_empty", "c_full"]}),
    );

    let spec = json!({
        "schema": "informal.spec/1",
        "design": {"top": "fifo", "clocks": ["clk"], "reset": "!rst_n"},
        "boundary": {
            "allowed_assume_inputs": ["push", "pop", "din"],
            "forbidden_assume_signals": ["full", "empty", "dout", "status", "count", "wp", "rp", "mem", "u_wp.*", "u_rp.*"]
        },
        "requirements": [
            {"id": "REQ-DISABLED", "title": "Disabled in task", "assertions": ["a_count_max"], "covers": ["c_full"]},
            {"id": "REQ-ENABLED", "title": "Enabled in task", "assertions": ["a_full_empty"], "covers": ["c_full"]}
        ],
        "signoff": {"proof_target": "proven"}
    });

    let audit = call(&api, "spec_audit", json!({"task": "t_sub", "spec": spec}));
    assert_eq!(audit["task"], "t_sub");
    assert_eq!(audit["signoff_ready"], false);
    assert_eq!(audit["summary"]["missing_assert"], 1);
    assert_eq!(audit["summary"]["verified"], 1);
    assert_eq!(audit["requirements"][0]["status"], "missing_assert");
    assert_eq!(audit["requirements"][1]["status"], "verified");
    assert!(audit["requirements"][0]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s
            .as_str()
            .unwrap()
            .contains("assertion `t_sub::a_count_max` exists but is disabled")));
}

#[test]
fn mcp_xprop_and_spv_cex_emit_traces_and_why_cex_slices() {
    let engines = informal_bmc::engines();
    if engines.is_empty() {
        informal_engine::tool_missing("the in-house engines need CaDiCaL");
        return;
    }
    let dir = std::env::temp_dir().join(format!("informal-mcp-xprop-spv-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let sv = dir.join("dut.sv");
    std::fs::write(
        &sv,
        "\
module dut(
    input logic clk,
    input logic rst_n,
    input logic load_en,
    input logic [7:0] pub_data,
    input logic [7:0] secret_key,
    output logic [7:0] clean_out,
    output logic [7:0] leaky_out,
    output logic [7:0] pub_o,
    output logic [7:0] leak_o
);
    logic [7:0] uninit_q;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            clean_out <= 8'd0;
            pub_o <= 8'd0;
            leak_o <= 8'd0;
        end else begin
            clean_out <= pub_data;
            pub_o <= pub_data;
            leak_o <= secret_key;
        end
    end
    always_ff @(posedge clk) begin
        if (load_en)
            uninit_q <= pub_data;
    end
    assign leaky_out = uninit_q;
endmodule
",
    )
    .unwrap();

    let work = dir.join("work");
    let opts = Options {
        mode: Mode::Auto,
        root: dir.clone(),
        workdir: work.clone(),
        max_time: Duration::from_secs(60),
    };
    let api = Arc::new(Api::spawn(engines, work, opts));

    call(
        &api,
        "session_open",
        json!({
            "files": [sv.display().to_string()],
            "top": "dut",
            "clock": "clk",
            "reset": "!rst_n"
        }),
    );

    // 1. check_xprop: clean_out is proven without trace, leaky_out emits trace + why_cex.
    call(
        &api,
        "check_xprop",
        json!({"action": "init", "targets": ["outputs"]}),
    );
    let xp_done = call(&api, "check_xprop", json!({"action": "prove"}));
    let xp_items = xp_done["items"].as_array().unwrap();
    let xp_clean = xp_items
        .iter()
        .find(|i| i["name"] == "dut.clean_out")
        .unwrap();
    assert_eq!(xp_clean["status"], "proven");
    assert!(xp_clean.get("trace").is_none());

    let xp_clean_err = api
        .call("why_cex", &json!({"name": "dut.clean_out"}))
        .unwrap_err();
    assert_eq!(xp_clean_err.code, "no_trace");

    let xp_leaky = xp_items
        .iter()
        .find(|i| i["name"] == "dut.leaky_out")
        .unwrap();
    assert_eq!(xp_leaky["status"], "cex");
    let xp_trace = xp_leaky["trace"].as_str().expect("xprop trace path");
    assert!(Path::new(xp_trace).is_file());
    assert!(Path::new(xp_trace).with_extension("trace.btor2").is_file());
    let xp_vcd_text = std::fs::read_to_string(xp_trace).unwrap();
    informal_mcp::vcd::parse(&xp_vcd_text).expect("xprop VCD must parse");

    let xp_why = call(&api, "why_cex", json!({"name": "dut.leaky_out"}));
    assert_eq!(xp_why["schema"], "informal.why_cex/1");
    assert_eq!(xp_why["status"], "cex");
    assert!(!xp_why["events"].as_array().unwrap().is_empty());

    // 2. check_spv: iso_pub is proven without trace, leak_sec emits trace + why_cex.
    call(
        &api,
        "check_spv",
        json!({
            "action": "create",
            "name": "iso_pub",
            "from": ["secret_key"],
            "to": ["pub_o"]
        }),
    );
    call(
        &api,
        "check_spv",
        json!({
            "action": "create",
            "name": "leak_sec",
            "from": ["secret_key"],
            "to": ["leak_o"]
        }),
    );
    let spv_done = call(&api, "check_spv", json!({"action": "prove"}));
    let spv_rules = spv_done["rules"].as_array().unwrap();
    let spv_clean = spv_rules.iter().find(|r| r["name"] == "iso_pub").unwrap();
    assert_eq!(spv_clean["status"], "proven");
    assert!(spv_clean.get("trace").is_none());

    let spv_clean_err = api
        .call("why_cex", &json!({"name": "iso_pub"}))
        .unwrap_err();
    assert_eq!(spv_clean_err.code, "no_trace");

    let spv_leak = spv_rules.iter().find(|r| r["name"] == "leak_sec").unwrap();
    assert_eq!(spv_leak["status"], "cex");
    let spv_trace = spv_leak["trace"].as_str().expect("spv trace path");
    assert!(Path::new(spv_trace).is_file());
    assert!(Path::new(spv_trace).with_extension("trace.btor2").is_file());
    let spv_vcd_text = std::fs::read_to_string(spv_trace).unwrap();
    informal_mcp::vcd::parse(&spv_vcd_text).expect("spv VCD must parse");

    let spv_why = call(&api, "why_cex", json!({"name": "leak_sec"}));
    assert_eq!(spv_why["schema"], "informal.why_cex/1");
    assert_eq!(spv_why["status"], "cex");
    assert!(!spv_why["events"].as_array().unwrap().is_empty());

    // 3. SVA properties inside formal tasks literally named `xprop` / `spv` / `sec` must resolve
    //    via `why_cex` (falling back to `s.props` when no X-prop/SPV/SEC item matches).
    call(
        &api,
        "tcl_eval",
        json!({
            "command": concat!(
                "task -create xprop; ",
                "assert -name xprop_task_prop -task xprop {pub_o == 8'h00}; ",
                "assert -name xprop_task_clean -task xprop {clean_out == clean_out}; ",
                "task -create spv; ",
                "assert -name spv_task_prop -task spv {pub_o == 8'h00}; ",
                "prove -task xprop -all; ",
                "prove -task spv -all"
            )
        }),
    );

    let xp_task_why = call(&api, "why_cex", json!({"name": "xprop::xprop_task_prop"}));
    assert_eq!(xp_task_why["schema"], "informal.why_cex/1");
    assert_eq!(xp_task_why["property"], "xprop::xprop_task_prop");
    assert_eq!(xp_task_why["status"], "cex");
    assert!(!xp_task_why["events"].as_array().unwrap().is_empty());

    let spv_task_why = call(&api, "why_cex", json!({"name": "spv::spv_task_prop"}));
    assert_eq!(spv_task_why["schema"], "informal.why_cex/1");
    assert_eq!(spv_task_why["property"], "spv::spv_task_prop");
    assert_eq!(spv_task_why["status"], "cex");
    assert!(!spv_task_why["events"].as_array().unwrap().is_empty());

    let xp_task_clean_err = api
        .call("why_cex", &json!({"name": "xprop::xprop_task_clean"}))
        .unwrap_err();
    assert_eq!(xp_task_clean_err.code, "no_trace");

    let _ = std::fs::remove_dir_all(&dir);
}
