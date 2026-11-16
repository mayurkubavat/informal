//! The console's Ask mode: what the model is told and how its answer is
//! read. The app never runs a command from here; `ask-done` hands the UI an
//! optional Tcl command and the UI waits for the user to press Enter.

use informal_llm::agent::ToolHost;
use informal_llm::ToolSpec;
use informal_mcp::Api;
use serde_json::Value;
use std::sync::Arc;

/// The tools Ask may use: those that read the session or the window.
/// Nothing that proves, proposes or evaluates Tcl; a command is still only
/// suggested, and the user presses Enter.
pub const TOOLS: &[&str] = &[
    "session_summary",
    "property_list",
    "property_get",
    "property_cone",
    "design_hierarchy",
    "design_info",
    "design_source",
    "trace_get",
    "session_log",
    "gui_state",
];

/// Added to the system prompt when the model has the tools.
pub const TOOLS_NOTE: &str = "You can look up what the session summary below does not show: a failing property's trace (trace_get), the design's source (design_source), a property's details and the signals it depends on, and what the user has selected (gui_state). Look before you explain a failure; say what the values show.";

/// [`TOOLS`] as the model is given them, from the AI interface's own
/// definitions.
pub fn tool_specs() -> Vec<ToolSpec> {
    let all = informal_mcp::tools::all()
        .into_iter()
        .chain(informal_mcp::tools::gui());
    all.filter(|t| TOOLS.contains(&t.name))
        .map(|t| ToolSpec {
            name: t.name.to_string(),
            description: t.description.to_string(),
            parameters: t.input,
        })
        .collect()
}

/// Runs Ask's tools on the app's session.
pub struct Host(pub Arc<Api>);

impl ToolHost for Host {
    fn call(&self, name: &str, arguments: &Value) -> Result<String, String> {
        if !TOOLS.contains(&name) {
            return Err(format!("{name} is not a tool Ask may use"));
        }
        match self.0.call(name, arguments) {
            Ok(reply) => Ok(reply.to_string()),
            Err(e) => Err(serde_json::json!({"code": e.code, "message": e.message}).to_string()),
        }
    }
}

/// For a model without tools: there is nothing to call.
pub struct NoTools;

impl ToolHost for NoTools {
    fn call(&self, name: &str, _arguments: &Value) -> Result<String, String> {
        Err(format!("no tool named {name}"))
    }
}

/// A tool call as one console line: `trace_get name=a_wrong signals=count,push`.
pub fn step_text(name: &str, arguments: &Value) -> String {
    let mut out = name.to_string();
    for (k, v) in arguments.as_object().into_iter().flatten() {
        let v = match v {
            Value::String(s) => s.clone(),
            Value::Array(a) => a
                .iter()
                .map(|x| x.as_str().map_or(x.to_string(), String::from))
                .collect::<Vec<_>>()
                .join(","),
            other => other.to_string(),
        };
        out += &format!(" {k}={v}");
    }
    out
}

/// At most this many properties are listed in the prompt.
pub const MAX_PROPS: usize = 200;

/// Usage of the commands people ask for most, per flavor: small models
/// get the argument syntax right far more often with these in front of them.
fn examples(flavor: &str) -> &'static str {
    if flavor == "vcf" {
        "check_fv                          ;# prove every property\n\
         check_fv -property {a_x}           ;# prove only a_x\n\
         report_fv -list                    ;# list properties and results\n\
         get_props -status falsified        ;# names of failed properties\n\
         fvdisable {a_x} / fvenable {a_x}   ;# switch a property off / on\n\
         set_fml_var fml_max_time 10m       ;# time limit"
    } else {
        "prove -all                         ;# prove every property\n\
         prove -property {a_x}              ;# prove only a_x\n\
         report -summary                    ;# results table\n\
         get_property_list -include {status cex}   ;# names of failed properties\n\
         get_status a_x                     ;# one property's status\n\
         set_prove_time_limit 10m           ;# time limit"
    }
}

/// The system prompt: the flavor, its command names (the list Tab
/// completion uses), and a summary of the session.
pub fn system_prompt(flavor: &str, commands: &[&str], session: &Value) -> String {
    let tool = if flavor == "vcf" {
        "Synopsys VC Formal"
    } else {
        "Cadence JasperGold"
    };
    format!(
        "You are the Ask assistant of InFormal, a formal property verification shell. \
         The shell accepts {tool} Tcl commands.\n\
         Rules:\n\
         - Answer briefly, in plain text: a sentence or two, no headings, no tables.\n\
         - When the user wants something done, end with exactly one Tcl command in a fenced ```tcl block. \
         Use only the commands listed below and property names from the session. Never invent either.\n\
         - When the user only asks a question (for example which properties failed), answer it from the session below \
         and give no code block, unless a command is needed to find out.\n\
         - Nothing runs until the user presses Enter on your command.\n\n\
         Commands: {}\n\n\
         Examples:\n{}\n\n\
         Session:\n{}",
        commands.join(" "),
        examples(flavor),
        session_summary(session),
    )
}

/// Top, clocks, reset and properties (name, kind, status), from the
/// session JSON the shell returns with every reply.
pub fn session_summary(s: &Value) -> String {
    if s.is_null() {
        return "(no session yet)".into();
    }
    let mut out = String::new();
    let top = s["design"]["top"].as_str().unwrap_or("(not set)");
    let files = s["design"]["files"].as_array().map_or(0, Vec::len);
    out += &format!(
        "top: {top}  design files: {files}  elaborated: {}\n",
        s["elaborated"].as_bool().unwrap_or(false)
    );
    let clocks: Vec<String> = s["clocks"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            format!(
                "{} ({})",
                c["signal"].as_str().unwrap_or("?"),
                c["edge"].as_str().unwrap_or("?")
            )
        })
        .collect();
    out += &format!(
        "clocks: {}\n",
        if clocks.is_empty() {
            "none".into()
        } else {
            clocks.join(", ")
        }
    );
    out += &format!("reset: {}\n", s["reset"].as_str().unwrap_or("none"));
    let props = s["props"].as_array().map(Vec::as_slice).unwrap_or_default();
    if props.is_empty() {
        out += "properties: none\n";
        return out;
    }
    // Spelled out, so a small model does not count an unreachable cover
    // as a failure.
    let with = |st: &str, kind: &str| -> Vec<&str> {
        props
            .iter()
            .filter(|p| {
                p["status"]["status"].as_str() == Some(st) && p["kind"].as_str() == Some(kind)
            })
            .filter_map(|p| p["name"].as_str())
            .collect()
    };
    let list = |v: Vec<&str>| {
        if v.is_empty() {
            "none".to_string()
        } else {
            v.join(", ")
        }
    };
    out += &format!(
        "failed (assertions with a CEX): {}\n",
        list(with("cex", "assert"))
    );
    out += &format!(
        "unreachable covers (not failures): {}\n",
        list(with("unreachable", "cover"))
    );
    let shown = props.len().min(MAX_PROPS);
    out += &format!(
        "properties ({}{}): name kind status\n",
        props.len(),
        if shown < props.len() {
            format!(", first {shown}")
        } else {
            String::new()
        }
    );
    for p in &props[..shown] {
        let st = &p["status"];
        let mut word = st["status"].as_str().unwrap_or("unprocessed").to_string();
        if let Some(d) = st["depth"].as_u64() {
            word += &format!(" depth {d}");
        }
        if p["enabled"].as_bool() == Some(false) {
            word += " (disabled)";
        }
        out += &format!(
            "  {} {} {}\n",
            p["name"].as_str().unwrap_or("?"),
            p["kind"].as_str().unwrap_or("?"),
            word
        );
    }
    out
}

/// The answer split into the explanation and the Tcl command of its first
/// fenced ```tcl block (also ```Tcl and a bare ``` fence). An empty block
/// gives no command. Several lines inside the block are kept as one
/// command string; the UI shows it and the user decides.
pub fn split_answer(text: &str) -> (String, Option<String>) {
    let Some(open) = find_fence(text) else {
        return (text.trim().to_string(), None);
    };
    let (before, rest) = text.split_at(open.0);
    let body = &rest[open.1..];
    let (cmd, after) = match body.find("```") {
        Some(end) => (&body[..end], &body[end + 3..]),
        None => (body, ""),
    };
    let cmd = cmd.trim();
    let explanation = format!("{} {}", before.trim(), after.trim())
        .trim()
        .to_string();
    (explanation, (!cmd.is_empty()).then(|| cmd.to_string()))
}

/// Byte offset of the first fence that opens a Tcl (or untagged) block,
/// and the length of the fence line including its newline.
fn find_fence(text: &str) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(i) = text[from..].find("```") {
        let at = from + i;
        let line_end = text[at..].find('\n').map_or(text.len(), |n| at + n);
        let tag = text[at + 3..line_end].trim().to_ascii_lowercase();
        if tag.is_empty() || tag == "tcl" {
            return Some((at, (line_end - at + 1).min(text.len() - at)));
        }
        // Some other language: skip past its closing fence.
        from = line_end + text[line_end.min(text.len())..].find("```")? + 3;
    }
    None
}

#[cfg(test)]
mod tests {
    /// The whole of Ask with tools against the configured model server:
    /// `cargo test -- --ignored live_ask --nocapture` (needs the server and
    /// CaDiCaL). Prints what the model looked up and what it answered.
    #[test]
    #[ignore = "needs a running model server"]
    fn live_ask_with_tools_on_the_fifo() {
        use informal_llm::agent::{run, Event, Limits, ToolMode};
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cfg = informal_llm::LlmConfig::load(&root.join("config/llm.toml")).unwrap();
        let name = cfg
            .selected(std::env::var("INFORMAL_LLM_PROVIDER").ok().as_deref())
            .unwrap();
        let provider = informal_llm::build(&name, &cfg.providers[&name]).unwrap();
        let work = std::env::temp_dir().join(format!("informal-live-ask-{}", std::process::id()));
        let api = std::sync::Arc::new(informal_mcp::Api::spawn(
            informal_bmc::engines(),
            work.clone(),
            informal_mcp::Options {
                mode: informal_mcp::Mode::ReadOnly,
                root: root.clone(),
                workdir: work,
                max_time: std::time::Duration::from_secs(60),
            },
        ));
        let open = api.call(
            "session_open",
            &serde_json::json!({"script": "tests/fpv/fifo/fifo.jg.tcl"}),
        );
        let session = open.unwrap()["summary"].clone();
        let system = format!(
            "You are the Ask assistant of InFormal, a formal property verification shell. \
             Answer briefly, in plain text.\n\n{}\n\nSession: {session}",
            super::TOOLS_NOTE
        );
        let messages = vec![
            informal_llm::Message::system(system),
            informal_llm::Message::user("Why does a_wrong fail? Use the trace."),
        ];
        let host = super::Host(api);
        let out = tauri::async_runtime::block_on(run(
            provider.as_ref(),
            ToolMode::of(&cfg.providers[&name]),
            &super::tool_specs(),
            &host,
            messages,
            Limits::default(),
            &mut |e| match e {
                Event::Call { name, arguments } => {
                    eprintln!("looked up {}", super::step_text(name, arguments))
                }
                Event::Result { name, ok } => eprintln!("  {name} -> {ok}"),
                Event::Text(_) => {}
            },
        ))
        .unwrap();
        eprintln!("{} steps, {} ms\n{}", out.steps, out.total_ms, out.text);
        assert!(!out.text.trim().is_empty());
    }

    #[test]
    fn ask_gets_only_tools_that_read() {
        let specs = super::tool_specs();
        let names: Vec<&str> = specs.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names.len(), super::TOOLS.len(), "{names:?}");
        for forbidden in [
            "prove",
            "proposal_add",
            "tcl_eval",
            "session_open",
            "gui_open_trace",
        ] {
            assert!(!names.contains(&forbidden));
        }
        let changing: Vec<&str> = informal_mcp::tools::all()
            .iter()
            .filter(|t| t.changes)
            .map(|t| t.name)
            .collect();
        assert!(names.iter().all(|n| !changing.contains(n)));
        assert_eq!(
            super::step_text(
                "trace_get",
                &serde_json::json!({"name": "a_wrong", "signals": ["count", "push"], "first": 2})
            ),
            "trace_get first=2 name=a_wrong signals=count,push"
        );
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn splits_the_tcl_block_from_the_explanation() {
        let (e, c) =
            split_answer("Proving only that one.\n```tcl\nprove -property a_never9\n```\n");
        assert_eq!(
            (e.as_str(), c.as_deref()),
            ("Proving only that one.", Some("prove -property a_never9"))
        );
        let (e, c) = split_answer("Two failed: a_x and a_y.");
        assert_eq!((e.as_str(), c), ("Two failed: a_x and a_y.", None));
        assert_eq!(
            split_answer("```Tcl\nprove -all\n```").1.as_deref(),
            Some("prove -all")
        );
        assert_eq!(
            split_answer("```\nreport -summary\n```").1.as_deref(),
            Some("report -summary")
        );
        // A non-Tcl block is skipped; the Tcl one after it is taken.
        let (_, c) =
            split_answer("```sv\nassert property (x);\n```\nthen\n```tcl\nprove -all\n```");
        assert_eq!(c.as_deref(), Some("prove -all"));
        assert_eq!(split_answer("```python\nprint(1)\n```").1, None);
        // Unclosed fence (stream cut by max_tokens) and empty block.
        assert_eq!(
            split_answer("Run\n```tcl\nprove -all").1.as_deref(),
            Some("prove -all")
        );
        assert_eq!(split_answer("```tcl\n```").1, None);
        let (e, _) = split_answer("Before\n```tcl\nprove -all\n```\nAfter.");
        assert_eq!(e, "Before After.");
    }

    #[test]
    fn summary_lists_the_session_and_caps_properties() {
        let s = json!({
            "design": {"top": "counter", "files": ["c.sv"]},
            "elaborated": true,
            "clocks": [{"signal": "clk", "edge": "posedge"}],
            "reset": "!rst_n",
            "props": [
                {"name": "a_never9", "kind": "assert", "enabled": true, "status": {"status": "cex", "depth": 9}},
                {"name": "c_wrap", "kind": "cover", "enabled": false, "status": {"status": "unprocessed"}},
            ],
        });
        let t = session_summary(&s);
        assert!(
            t.contains("top: counter")
                && t.contains("clk (posedge)")
                && t.contains("reset: !rst_n"),
            "{t}"
        );
        assert!(
            t.contains("a_never9 assert cex depth 9")
                && t.contains("c_wrap cover unprocessed (disabled)"),
            "{t}"
        );
        assert!(
            t.contains("failed (assertions with a CEX): a_never9\n")
                && t.contains("unreachable covers (not failures): none"),
            "{t}"
        );
        let many: Vec<Value> = (0..250).map(|i| json!({"name": format!("p{i}"), "kind": "assert", "status": {"status": "proven"}})).collect();
        let t = session_summary(&json!({"props": many}));
        assert!(
            t.contains("properties (250, first 200)")
                && t.contains("p199 ")
                && !t.contains("p200 "),
            "{t}"
        );
        assert_eq!(session_summary(&Value::Null), "(no session yet)");
    }

    #[test]
    fn prompt_names_the_flavor_and_its_commands() {
        let p = system_prompt("vcf", &["check_fv", "fvassert"], &Value::Null);
        assert!(p.contains("VC Formal") && p.contains("check_fv fvassert") && p.contains("```tcl"));
        assert!(system_prompt("jg", &["prove"], &Value::Null).contains("JasperGold"));
    }
}
