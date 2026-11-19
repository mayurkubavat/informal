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
        5
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
    let actions = window.0.lock().unwrap().clone();
    assert_eq!(
        actions,
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

    // Protocol errors: an unknown tool and an unknown method.
    c.server
        .handle(r#"{"jsonrpc":"2.0","id":"x","method":"tools/call","params":{"name":"nope"}}"#);
    let e = c.rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(e["error"]["code"], -32602);
    c.server
        .handle(r#"{"jsonrpc":"2.0","id":"y","method":"resources/list"}"#);
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
