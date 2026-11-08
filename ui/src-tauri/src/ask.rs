//! The console's Ask mode: what the model is told and how its answer is
//! read. The app never runs a command from here; `ask-done` hands the UI an
//! optional Tcl command and the UI waits for the user to press Enter.

use serde_json::Value;

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
    let tool = if flavor == "vcf" { "Synopsys VC Formal" } else { "Cadence JasperGold" };
    format!(
        "You are the Ask assistant of FormalDV, a formal property verification shell. \
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
    out += &format!("top: {top}  design files: {files}  elaborated: {}\n", s["elaborated"].as_bool().unwrap_or(false));
    let clocks: Vec<String> = s["clocks"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| format!("{} ({})", c["signal"].as_str().unwrap_or("?"), c["edge"].as_str().unwrap_or("?")))
        .collect();
    out += &format!("clocks: {}\n", if clocks.is_empty() { "none".into() } else { clocks.join(", ") });
    out += &format!("reset: {}\n", s["reset"].as_str().unwrap_or("none"));
    let props = s["props"].as_array().map(Vec::as_slice).unwrap_or_default();
    if props.is_empty() {
        out += "properties: none\n";
        return out;
    }
    let shown = props.len().min(MAX_PROPS);
    out += &format!("properties ({}{}): name kind status\n", props.len(), if shown < props.len() { format!(", first {shown}") } else { String::new() });
    for p in &props[..shown] {
        let st = &p["status"];
        let mut word = st["status"].as_str().unwrap_or("unprocessed").to_string();
        if let Some(d) = st["depth"].as_u64() {
            word += &format!(" depth {d}");
        }
        if p["enabled"].as_bool() == Some(false) {
            word += " (disabled)";
        }
        out += &format!("  {} {} {}\n", p["name"].as_str().unwrap_or("?"), p["kind"].as_str().unwrap_or("?"), word);
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
    let explanation = format!("{} {}", before.trim(), after.trim()).trim().to_string();
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
    use super::*;
    use serde_json::json;

    #[test]
    fn splits_the_tcl_block_from_the_explanation() {
        let (e, c) = split_answer("Proving only that one.\n```tcl\nprove -property a_never9\n```\n");
        assert_eq!((e.as_str(), c.as_deref()), ("Proving only that one.", Some("prove -property a_never9")));
        let (e, c) = split_answer("Two failed: a_x and a_y.");
        assert_eq!((e.as_str(), c), ("Two failed: a_x and a_y.", None));
        assert_eq!(split_answer("```Tcl\nprove -all\n```").1.as_deref(), Some("prove -all"));
        assert_eq!(split_answer("```\nreport -summary\n```").1.as_deref(), Some("report -summary"));
        // A non-Tcl block is skipped; the Tcl one after it is taken.
        let (_, c) = split_answer("```sv\nassert property (x);\n```\nthen\n```tcl\nprove -all\n```");
        assert_eq!(c.as_deref(), Some("prove -all"));
        assert_eq!(split_answer("```python\nprint(1)\n```").1, None);
        // Unclosed fence (stream cut by max_tokens) and empty block.
        assert_eq!(split_answer("Run\n```tcl\nprove -all").1.as_deref(), Some("prove -all"));
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
        assert!(t.contains("top: counter") && t.contains("clk (posedge)") && t.contains("reset: !rst_n"), "{t}");
        assert!(t.contains("a_never9 assert cex depth 9") && t.contains("c_wrap cover unprocessed (disabled)"), "{t}");
        let many: Vec<Value> = (0..250).map(|i| json!({"name": format!("p{i}"), "kind": "assert", "status": {"status": "proven"}})).collect();
        let t = session_summary(&json!({"props": many}));
        assert!(t.contains("properties (250, first 200)") && t.contains("p199 ") && !t.contains("p200 "), "{t}");
        assert_eq!(session_summary(&Value::Null), "(no session yet)");
    }

    #[test]
    fn prompt_names_the_flavor_and_its_commands() {
        let p = system_prompt("vcf", &["check_fv", "fvassert"], &Value::Null);
        assert!(p.contains("VC Formal") && p.contains("check_fv fvassert") && p.contains("```tcl"));
        assert!(system_prompt("jg", &["prove"], &Value::Null).contains("JasperGold"));
    }
}
