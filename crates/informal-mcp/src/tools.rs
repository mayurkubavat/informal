//! The tools as a client sees them: name, what it does, and JSON Schemas
//! of its arguments and its reply.

use crate::api::Mode;
use serde_json::{json, Value};

pub struct Tool {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub input: Value,
    pub output: Value,
    /// Adds to or changes the session (left out of a read-only server).
    pub changes: bool,
}

/// An object schema: `(name, schema)` properties and the required names.
fn obj(props: &[(&str, Value)], required: &[&str]) -> Value {
    let p: serde_json::Map<String, Value> = props
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    json!({"type": "object", "properties": p, "required": required})
}

/// `schema` with a description, when there is one to give.
fn described(mut schema: Value, description: &str) -> Value {
    if !description.is_empty() {
        schema["description"] = json!(description);
    }
    schema
}

fn s(description: &str) -> Value {
    described(json!({"type": "string"}), description)
}

fn n(description: &str) -> Value {
    described(json!({"type": "integer", "minimum": 0}), description)
}

fn b(description: &str) -> Value {
    described(json!({"type": "boolean"}), description)
}

fn list(items: Value, description: &str) -> Value {
    described(json!({"type": "array", "items": items}), description)
}

fn one_of(values: &[&str], description: &str) -> Value {
    described(json!({"type": "string", "enum": values}), description)
}

/// The reply's `schema` field: its name and version.
fn schema(name: &str) -> (&'static str, Value) {
    ("schema", json!({"type": "string", "const": name}))
}

const STATUS: &[&str] = &[
    "unprocessed",
    "proven",
    "bounded_proven",
    "cex",
    "covered",
    "unreachable",
    "undetermined",
    "error",
];

fn property() -> Value {
    obj(
        &[
            ("name", s("")),
            ("kind", one_of(&["assert", "assume", "cover"], "")),
            ("expr", s("The property as written (SystemVerilog)")),
            ("enabled", b("")),
            ("origin", one_of(&["user", "rtl", "ai"], "Who wrote it: the user's script, the RTL, or an AI through this interface")),
            ("status", one_of(STATUS, "proven and unreachable are proofs; cex and covered come with a trace")),
            ("engine", s("The engine that settled it")),
            ("time_ms", n("")),
            ("depth", n("Frame of the trace in which the property fails or the cover is hit, from 0")),
            ("detail", s("Why it is undetermined or in error")),
            ("has_trace", b("")),
            ("source", obj(&[("file", s("")), ("line", n(""))], &["file", "line"])),
            ("under_ai", list(s(""), "AI-added assumptions and cut points that were in the model when this status was found: the result stands on them")),
            ("engines", list(json!({"type": "object"}), "Every engine's run in the last prove")),
            ("task", s("The task it belongs to: <embedded>, or one a `task -create` made (its name is then <task>::<name>)")),
            ("stats", json!({"type": "object", "description": "What the last prove measured: engine, wall_ms, cpu_ms (process and engine processes, may exceed wall time), peak_rss_kb (only when the high-water mark rose during this property), depth and depth_kind (trace, k, frames or bound), max_bound, invariant_clauses, evidence, certificate. A field the engines did not measure is left out"})),
            ("vacuity", s("For a proven assertion: what the vacuity checks found, or `not checked`")),
            ("copy_of", s("For a copy of a property written in the RTL, the original: its check in the model is this one's")),
        ],
        &["name", "kind", "status", "origin", "has_trace"],
    )
}

fn proposal() -> Value {
    obj(
        &[
            ("id", n("")),
            (
                "kind",
                one_of(&["assume", "assert", "cover", "cut_point", "command"], ""),
            ),
            ("name", s("")),
            ("text", s("The expression, the signal, or the Tcl command")),
            ("rationale", s("")),
            (
                "state",
                one_of(
                    &[
                        "pending",
                        "applied",
                        "rejected",
                        "withdrawn",
                        "undone",
                        "failed",
                        "gone",
                    ],
                    "pending waits for a person; applied is in the session, tagged AI-added",
                ),
            ),
            ("error", s("")),
        ],
        &["id", "kind", "text", "rationale", "state"],
    )
}

fn summary() -> Value {
    obj(
        &[
            schema("informal.summary/1"),
            ("mode", one_of(&["read-only", "propose", "auto"], "")),
            ("elaborated", b("")),
            ("top", json!({"type": ["string", "null"]})),
            ("files", list(s(""), "")),
            ("clocks", list(json!({"type": "object"}), "")),
            (
                "reset",
                json!({"type": ["string", "null"], "description": "The condition that holds during reset"}),
            ),
            ("cut_points", list(s(""), "The current task's")),
            ("current_task", s("The task commands and tools without a task apply to")),
            (
                "tasks",
                list(
                    obj(
                        &[
                            ("name", s("")),
                            ("properties", n("")),
                            ("cut_points", list(s(""), "")),
                        ],
                        &["name", "properties", "cut_points"],
                    ),
                    "Each task: its own properties, assumptions and cut points, proved on its own model",
                ),
            ),
            ("black_boxes", list(s(""), "")),
            ("time_limit_s", json!({"type": "number"})),
            ("engines", list(s(""), "")),
            (
                "properties",
                obj(
                    &[
                        ("assert", n("")),
                        ("assume", n("")),
                        ("cover", n("")),
                        ("by_status", json!({"type": "object"})),
                    ],
                    &["assert", "assume", "cover", "by_status"],
                ),
            ),
            (
                "ai",
                obj(
                    &[
                        ("properties", n("")),
                        ("cut_points", n("")),
                        ("pending_proposals", n("")),
                    ],
                    &["properties", "cut_points", "pending_proposals"],
                ),
            ),
        ],
        &["schema", "mode", "elaborated", "properties", "ai"],
    )
}

/// Every tool, in the order a client lists them.
pub fn all() -> Vec<Tool> {
    let name = (
        "name",
        s("Property name, exactly as property_list gives it"),
    );
    vec![
        Tool {
            name: "session_open",
            title: "Open a design",
            description: "Start the session over from a JasperGold or VC Formal Tcl script (`script`), or from SystemVerilog files with a top module, clock and reset (`files`). A script runs as written, including any prove in it. Returns the session summary; a script that stops with an error gives ok=false and the error, with the session as far as it got.",
            input: obj(
                &[
                    ("script", s("Tcl script path")),
                    ("files", list(s(""), "SystemVerilog files, instead of a script")),
                    ("top", s("Top module (with files)")),
                    ("clock", s("Clock signal (with files)")),
                    ("reset", s("Condition that holds during reset, e.g. !rst_n (with files)")),
                    ("flavor", one_of(&["jg", "vcf"], "Command set of the script; detected when left out")),
                ],
                &[],
            ),
            output: obj(&[schema("informal.open/1"), ("ok", b("")), ("error", s("")), ("summary", summary())], &["schema", "ok", "summary"]),
            changes: false,
        },
        Tool {
            name: "script_check",
            title: "Check a script without running it",
            description: "Report, per line, every command or option of a Tcl script that the tool would reject, ignore or does not know. Nothing is elaborated or proved and the open session is not touched.",
            input: obj(&[("script", s("Tcl script path")), ("flavor", one_of(&["jg", "vcf"], ""))], &["script"]),
            output: obj(
                &[
                    schema("informal.check/1"),
                    ("script", s("")),
                    ("clean", b("True when the script would run to its end")),
                    ("findings", list(obj(&[("kind", s("error, unknown, ignored or tcl")), ("file", s("")), ("line", n("")), ("command", s("")), ("message", s(""))], &["kind", "file", "line", "command", "message"]), "")),
                    ("commands", json!({"type": "object"})),
                ],
                &["schema", "clean", "findings"],
            ),
            changes: false,
        },
        Tool {
            name: "session_summary",
            title: "Session summary",
            description: "The design setup (top, files, clocks, reset, cut points), the engines, property counts by kind and status, and what an AI has added or proposed.",
            input: obj(&[], &[]),
            output: summary(),
            changes: false,
        },
        Tool {
            name: "session_log",
            title: "Console log",
            description: "The session's last console lines: notes and warnings from elaboration and proving, and every AI-added change.",
            input: obj(&[("last", n("How many lines (default 50)"))], &[]),
            output: obj(&[schema("informal.log/1"), ("lines", list(s(""), ""))], &["schema", "lines"]),
            changes: false,
        },
        Tool {
            name: "property_list",
            title: "List properties",
            description: "The assertions, assumptions and covers of the session with their status, optionally filtered.",
            input: obj(
                &[
                    ("kind", one_of(&["assert", "assume", "cover"], "")),
                    ("status", one_of(STATUS, "")),
                    ("name", s("Glob on the name (* and ?)")),
                    ("origin", one_of(&["user", "rtl", "ai"], "")),
                    ("task", s("Only this task's properties")),
                ],
                &[],
            ),
            output: obj(&[schema("informal.properties/1"), ("total", n("Properties in the session, before filtering")), ("properties", list(property(), ""))], &["schema", "total", "properties"]),
            changes: false,
        },
        Tool {
            name: "property_get",
            title: "One property",
            description: "One property with its status, the engine and time that settled it, its source location and every engine's run.",
            input: obj(std::slice::from_ref(&name), &["name"]),
            output: obj(&[schema("informal.property/1"), ("property", property())], &["schema", "property"]),
            changes: false,
        },
        Tool {
            name: "property_cone",
            title: "Signals a property depends on",
            description: "The inputs and registers in a property's cone of influence, nearest first: distance is the number of register stages between the signal and the property.",
            input: obj(&[name.clone(), ("helpers", b("Also list state the tool added to check the property"))], &["name"]),
            output: obj(
                &[
                    schema("informal.cone/1"),
                    ("property", s("")),
                    ("signals", list(obj(&[("name", s("")), ("width", n("Bits; 0 for a memory")), ("kind", one_of(&["input", "register", "memory"], "")), ("distance", n("")), ("helper", b(""))], &["name", "width", "kind", "distance", "helper"]), "")),
                ],
                &["schema", "property", "signals"],
            ),
            changes: false,
        },
        Tool {
            name: "design_hierarchy",
            title: "Design hierarchy",
            description: "The instance tree of the elaborated design: each scope's module, ports and signals with widths.",
            input: obj(
                &[
                    ("scope", s("Start at this instance path (default: the top)")),
                    ("depth", n("Levels of children to include (default 2)")),
                    ("signals", b("Include each scope's signals (default true)")),
                ],
                &[],
            ),
            output: obj(&[schema("informal.hierarchy/1"), ("top", json!({"type": ["string", "null"]})), ("scopes", list(json!({"type": "object"}), "Each: path, instance, module, signals (name, width, kind, dir), children"))], &["schema", "scopes"]),
            changes: false,
        },
        Tool {
            name: "design_info",
            title: "Design information",
            description: "What the design holds, counted on its netlist (instances, inputs, flops, latches, memories, clocks and resets found), what the script declared (clocks, reset, black boxes, the current task's cut points and properties), and the size of the current task's model after lowering (Btor2 nodes, states, inputs, constraints, targets, and the bit-blasted AIG). Builds the model, so it takes as long as one lowering.",
            input: obj(&[], &[]),
            output: obj(
                &[
                    schema("informal.design_info/1"),
                    ("top", json!({"type": ["string", "null"]})),
                    ("design", json!({"type": ["object", "null"], "description": "Counts on the netlist as elaborated (before black boxes and cut points)"})),
                    ("task", s("")),
                    ("model", json!({"type": ["object", "null"], "description": "The current task's model as the engines get it"})),
                    ("model_error", s("Why the model was not built")),
                ],
                &["schema", "task"],
            ),
            changes: false,
        },
        Tool {
            name: "design_source",
            title: "Source text",
            description: "Lines of a design file or script, by line range (at most 400 lines a call).",
            input: obj(&[("file", s("Path, as a property's source or the summary's files give it")), ("first_line", n("From 1 (default 1)")), ("last_line", n(""))], &["file"]),
            output: obj(&[schema("informal.source/1"), ("file", s("")), ("first_line", n("")), ("last_line", n("")), ("total_lines", n("")), ("lines", list(s(""), ""))], &["schema", "file", "first_line", "last_line", "total_lines", "lines"]),
            changes: false,
        },
        Tool {
            name: "trace_get",
            title: "Counterexample or cover trace as data",
            description: "The trace of a property whose status is cex or covered: one value per signal and frame (a frame is a clock cycle; frame 0 is the reset cycle). Words are in hex, single bits 0 or 1. hit_at lists the frames in which the assertion fails or the cover is hit. Without `signals`, the signals the property names come first, then the rest of its cone.",
            input: obj(
                &[
                    name.clone(),
                    ("signals", list(s(""), "Globs on signal names (full path, path below the top, or last part)")),
                    ("first", n("First frame (default 0)")),
                    ("last", n("Last frame (default: the last of the trace)")),
                    ("helpers", b("Also give state the tool added to check the property")),
                ],
                &["name"],
            ),
            output: obj(
                &[
                    schema("informal.trace/1"),
                    ("property", property()),
                    ("unit", one_of(&["cycle", "model_step"], "What a frame is; model_step is half a period of the fastest clock")),
                    ("frames", n("Frames in the whole trace")),
                    ("first", n("")),
                    ("last", n("")),
                    ("hit_at", list(n(""), "")),
                    ("signals", list(obj(&[("name", s("")), ("width", n("")), ("values", list(s(""), "One per frame from first to last"))], &["name", "width", "values"]), "")),
                    ("signals_omitted", n("Matching signals left out of this reply (64 at most are given)")),
                    ("notes", list(s(""), "")),
                ],
                &["schema", "property", "unit", "frames", "first", "last", "hit_at", "signals", "signals_omitted"],
            ),
            changes: false,
        },
        Tool {
            name: "prove",
            title: "Prove properties",
            description: "Run the engines on the named assertions and covers (the enabled ones of the task, or of the current task, when none is named) and return each one's result. Each task is proved on its own model, with its own assumptions and cut points. Only this gives a verdict. With wait=false it returns at once: follow with prove_status, end with prove_stop.",
            input: obj(
                &[
                    ("properties", list(s(""), "Names or globs")),
                    ("task", s("Prove this task's enabled assertions and covers (with properties: look the names up in it)")),
                    ("time_limit_s", json!({"type": "number", "description": "Per property; the session's limit when left out, and never above the server's"})),
                    ("wait", b("Default true")),
                ],
                &[],
            ),
            output: obj(
                &[
                    schema("informal.prove/1"),
                    ("started", b("With wait=false")),
                    ("time_limit_s", json!({"type": "number"})),
                    ("stopped", b("")),
                    ("counts", json!({"type": "object", "description": "Number of results per status"})),
                    ("results", list(property(), "")),
                ],
                &["schema"],
            ),
            changes: false,
        },
        Tool {
            name: "prove_status",
            title: "Progress of a prove",
            description: "Whether a prove is running, each property's state so far, and the result of the last prove started with wait=false once it has ended.",
            input: obj(&[], &[]),
            output: obj(
                &[
                    schema("informal.progress/1"),
                    ("running", b("")),
                    ("total", n("")),
                    ("done", n("")),
                    ("elapsed_ms", n("")),
                    ("properties", list(json!({"type": "object"}), "")),
                    ("result", json!({"type": "object"})),
                    ("error", json!({"type": "object"})),
                ],
                &["schema", "running"],
            ),
            changes: false,
        },
        Tool {
            name: "prove_stop",
            title: "Stop a prove",
            description: "Ask a running prove to stop. Properties not settled yet become undetermined (stopped).",
            input: obj(&[], &[]),
            output: obj(&[schema("informal.stop/1"), ("requested", b(""))], &["schema", "requested"]),
            changes: false,
        },
        Tool {
            name: "proposal_add",
            title: "Propose a change",
            description: "Propose an assumption, a helper assertion, a cover (`expr`, SystemVerilog in the top module's scope) or a cut point (`signal`). In propose mode it waits for a person (state pending); in automatic mode it is applied at once, tagged AI-added. A proposal is not a result: prove afterwards, and note that a proof under an AI-added assumption stands on that assumption.",
            input: obj(
                &[
                    ("kind", one_of(&["assume", "assert", "cover", "cut_point"], "")),
                    ("name", s("Name for the new property")),
                    ("expr", s("Property expression, e.g. req |=> gnt")),
                    ("signal", s("Signal to cut from its drivers (cut_point)")),
                    ("rationale", s("Why: shown to the person who decides")),
                ],
                &["kind", "rationale"],
            ),
            output: obj(&[schema("informal.proposal/1"), ("proposal", proposal())], &["schema", "proposal"]),
            changes: true,
        },
        Tool {
            name: "proposal_list",
            title: "List proposals",
            description: "Every change proposed through this interface since the design was opened, with its state.",
            input: obj(&[("state", s("Only proposals in this state"))], &[]),
            output: obj(&[schema("informal.proposals/1"), ("mode", s("")), ("proposals", list(proposal(), ""))], &["schema", "proposals"]),
            changes: false,
        },
        Tool {
            name: "proposal_undo",
            title: "Take back proposals",
            description: "Take back one proposal (`id`) or all of them: a pending one is withdrawn, an applied one is removed from the session, and every result that was found with it in the model goes back to unprocessed (`reset`).",
            input: obj(&[("id", n("Left out: every pending and applied proposal"))], &[]),
            output: obj(&[schema("informal.undo/1"), ("undone", list(n(""), "")), ("reset", list(s(""), "Properties to prove again"))], &["schema", "undone", "reset"]),
            changes: true,
        },
        Tool {
            name: "why_cex",
            title: "Root-cause slice of a counterexample trace",
            description: "Dynamic backward slice from a falsified assertion (`cex`) or reached cover (`covered`) at failure frame T back to frame 0: walks only active mux arms (`ite`), controlling boolean operands, and register state transitions, pruning untaken datapaths and idle hold cycles.",
            input: obj(
                &[
                    ("name", s("Property name")),
                    ("max_events", n("Maximum causal events to return (default 64)")),
                ],
                &["name"],
            ),
            output: obj(
                &[
                    schema("informal.why_cex/1"),
                    ("property", s("")),
                    ("status", one_of(&["cex", "covered"], "")),
                    ("failure_frame", n("")),
                    ("total_frames", n("")),
                    ("static_cone_signals", n("")),
                    ("active_slice_signals", n("")),
                    ("active_signals", list(s(""), "")),
                    (
                        "events",
                        list(
                            obj(
                                &[
                                    ("frame", n("")),
                                    ("signal", s("")),
                                    ("kind", one_of(&["input", "register", "wire"], "")),
                                    ("width", n("")),
                                    ("value", s("")),
                                    ("prev_value", s("")),
                                    ("reason", s("")),
                                ],
                                &["frame", "signal", "kind", "width", "value", "reason"],
                            ),
                            "",
                        ),
                    ),
                ],
                &[
                    "schema",
                    "property",
                    "status",
                    "failure_frame",
                    "total_frames",
                    "static_cone_signals",
                    "active_slice_signals",
                    "active_signals",
                    "events",
                ],
            ),
            changes: false,
        },
        Tool {
            name: "prove_cache",
            title: "Configure or inspect the COI proof cache",
            description: "Enable, disable, or inspect the cone-of-influence SHA-256 proof cache (`set_prove_cache*`).",
            input: obj(
                &[
                    ("enabled", b("Turn proof caching on or off")),
                    ("path", s("Directory below the workspace root for cached verdicts")),
                    ("mode", one_of(&["rw", "ro", "wo"], "Read-write, read-only, or write-only")),
                ],
                &[],
            ),
            output: obj(
                &[
                    schema("informal.prove_cache/1"),
                    ("enabled", b("")),
                    ("path", s("")),
                    ("mode", one_of(&["rw", "ro", "wo"], "")),
                ],
                &["schema", "enabled", "mode"],
            ),
            changes: true,
        },
        Tool {
            name: "check_cov",
            title: "Formal COI observability and UNR dead-code coverage",
            description: "Initialize, measure, query, or clear formal coverage (`coi`, `branch`, `toggle`, `functional`).",
            input: obj(
                &[
                    ("action", one_of(&["init", "measure", "list", "clear"], "")),
                    ("models", list(one_of(&["coi", "branch", "toggle", "functional", "all"], ""), "")),
                    ("proof_core", b("Use minimal proof-core COI on proven assertions")),
                ],
                &["action"],
            ),
            output: obj(
                &[
                    schema("informal.cov/1"),
                    ("initialized", b("")),
                    ("measured", b("")),
                    ("proof_core", b("")),
                    ("models", list(s(""), "")),
                    ("report", s("")),
                    (
                        "items",
                        list(
                            obj(
                                &[
                                    ("id", s("")),
                                    ("task", s("")),
                                    ("model", s("")),
                                    ("name", s("")),
                                    ("status", s("")),
                                    ("detail", s("")),
                                ],
                                &["id", "model", "name", "status"],
                            ),
                            "",
                        ),
                    ),
                ],
                &["schema", "initialized", "measured", "models", "items"],
            ),
            changes: true,
        },
        Tool {
            name: "check_conn",
            title: "Formal CSV connectivity verification",
            description: "Load a connectivity CSV map into the `Connectivity` task, prove its rules, list statuses, or clear it.",
            input: obj(
                &[
                    ("action", one_of(&["load", "prove", "list", "clear"], "")),
                    ("file", s("Connectivity CSV path below the workspace root (for load)")),
                ],
                &["action"],
            ),
            output: obj(
                &[
                    schema("informal.conn/1"),
                    ("count", n("")),
                    ("properties", list(property(), "")),
                ],
                &["schema", "count", "properties"],
            ),
            changes: true,
        },
        Tool {
            name: "check_xprop",
            title: "Exact 2-copy formal X-propagation verification",
            description: "Initialize, prove, list, or clear 2-copy self-composition X-propagation checks across outputs, control muxes, memories, assertions, or explicit signals.",
            input: obj(
                &[
                    ("action", one_of(&["init", "prove", "list", "clear"], "")),
                    ("targets", list(one_of(&["outputs", "control", "memories", "assertions", "all"], ""), "")),
                    ("x_sources", list(s(""), "Additional signals allowed to diverge with X")),
                    ("to", list(s(""), "Explicit sink signals to check")),
                ],
                &["action"],
            ),
            output: obj(
                &[
                    schema("informal.xprop/1"),
                    ("initialized", b("")),
                    ("report", s("")),
                    (
                        "items",
                        list(
                            obj(
                                &[
                                    ("name", s("")),
                                    ("category", s("")),
                                    ("status", s("")),
                                    ("depth", n("")),
                                    ("engine", s("")),
                                ],
                                &["name", "category", "status"],
                            ),
                            "",
                        ),
                    ),
                ],
                &["schema", "initialized", "items"],
            ),
            changes: true,
        },
        Tool {
            name: "check_spv",
            title: "2-safety formal security taint / non-interference verification",
            description: "Create, prove, list, or clear 2-safety non-interference rules proving secret `-from` signals never influence observable `-to` signals.",
            input: obj(
                &[
                    ("action", one_of(&["create", "prove", "list", "clear"], "")),
                    ("name", s("Rule name (optional for create)")),
                    ("from", list(s(""), "Secret/tainted source signals")),
                    ("to", list(s(""), "Untrusted/observable sink signals")),
                    ("cond", s("Optional SystemVerilog guard expression")),
                ],
                &["action"],
            ),
            output: obj(
                &[
                    schema("informal.spv/1"),
                    ("report", s("")),
                    (
                        "rules",
                        list(
                            obj(
                                &[
                                    ("name", s("")),
                                    ("task", s("")),
                                    ("from", list(s(""), "")),
                                    ("to", list(s(""), "")),
                                    ("cond", s("")),
                                    ("status", s("")),
                                    ("depth", n("")),
                                    ("engine", s("")),
                                ],
                                &["name", "from", "to", "status"],
                            ),
                            "",
                        ),
                    ),
                ],
                &["schema", "rules"],
            ),
            changes: true,
        },
        Tool {
            name: "check_sec",
            title: "Dual-design formal sequential (SEC) and combinational (LEC) equivalence checking",
            description: "Configure, prove, list, or clear dual-design equivalence checks between a reference `spec` RTL and a refactored/pipelined `imp` RTL (`lec: false` for sequential SEC with optional `latency`, `lec: true` for combinational keypoint LEC).",
            input: obj(
                &[
                    ("action", one_of(&["setup", "prove", "list", "clear"], "")),
                    ("spec_files", list(s(""), "Spec SystemVerilog file paths")),
                    ("spec_top", s("Optional spec top module name")),
                    ("imp_files", list(s(""), "Implementation SystemVerilog file paths")),
                    ("imp_top", s("Optional implementation top module name")),
                    ("clock", s("Clock port name")),
                    ("reset", s("Active-high in-reset expression (e.g. `!rst_n`)")),
                    ("lec", b("Combinational keypoint LEC mode instead of sequential SEC")),
                    ("latency", n("Additional pipeline cycles in imp relative to spec")),
                    ("assumptions", list(s(""), "Shared SystemVerilog environment assumptions")),
                ],
                &["action"],
            ),
            output: obj(
                &[
                    schema("informal.sec/1"),
                    ("configured", b("")),
                    ("lec", b("")),
                    ("latency", n("")),
                    ("report", s("")),
                    (
                        "checks",
                        list(
                            obj(
                                &[
                                    ("name", s("")),
                                    ("kind", s("")),
                                    ("spec_sig", s("")),
                                    ("imp_sig", s("")),
                                    ("width", n("")),
                                    ("latency", n("")),
                                    ("status", s("")),
                                    ("depth", n("")),
                                    ("engine", s("")),
                                    ("trace", s("")),
                                ],
                                &[
                                    "name",
                                    "kind",
                                    "spec_sig",
                                    "imp_sig",
                                    "width",
                                    "latency",
                                    "status",
                                ],
                            ),
                            "",
                        ),
                    ),
                ],
                &["schema", "configured", "lec", "latency", "checks"],
            ),
            changes: true,
        },
        Tool {
            name: "check_csr",
            title: "Automated CSR / RAL formal register verification",
            description: "Load an OpenTitan HJSON/JSON, CSV, or UVM RAL `.sv` (`uvm_reg_block`) register specification into task `CSR` and prove reset, read-back, `RW`/`RO`/`W1C`/`W1S`/`W0C`/`WO`/`RC` access, `regwen` lock, and address non-aliasing properties.",
            input: obj(
                &[
                    ("action", one_of(&["load", "prove", "list", "clear"], "")),
                    ("file", s("Path to HJSON, JSON, CSV, or UVM RAL `.sv` file (required for `load`)")),
                    ("prefix", s("Optional RTL register/field signal prefix (e.g. `u_reg.`)")),
                    ("addr", s("Optional CSR address signal override")),
                    ("wdata", s("Optional CSR write-data signal override")),
                    ("rdata", s("Optional CSR read-data signal override")),
                    ("we", s("Optional CSR write-enable expression override")),
                    ("re", s("Optional CSR read-enable expression override")),
                ],
                &["action"],
            ),
            output: obj(
                &[
                    schema("informal.csr/1"),
                    ("count", n("")),
                    ("properties", list(property(), "")),
                ],
                &["schema", "count", "properties"],
            ),
            changes: true,
        },
        Tool {
            name: "check_protocol",
            title: "Formal bus protocol VIP SVA generator (`tlul`, `axi4lite`, `apb`)",
            description: "Generate, prove, list, or clear formal bus protocol assertion/assumption/cover VIP suites in task `Protocol` for TileLink-UL (`tlul`), AMBA AXI4-Lite (`axi4lite`), or AMBA APB (`apb`).",
            input: obj(
                &[
                    ("action", one_of(&["create", "prove", "list", "clear"], "")),
                    ("protocol", one_of(&["tlul", "axi4lite", "apb"], "Required when action is `create`")),
                    ("role", one_of(&["master", "slave", "monitor"], "Default: `slave`")),
                    ("name", s("Optional VIP prefix name")),
                    ("prefix", s("Optional RTL bus signal prefix")),
                ],
                &["action"],
            ),
            output: obj(
                &[
                    schema("informal.protocol/1"),
                    ("count", n("")),
                    ("properties", list(property(), "")),
                ],
                &["schema", "count", "properties"],
            ),
            changes: true,
        },
        Tool {
            name: "open_workbench",
            title: "InFormal",
            description: "Open or refresh the interactive InFormal verification workbench (MCP App side pane or full-page view): property status table, waveform viewer, causal schematic, RTL source, and Formal Apps.",
            input: obj(
                &[
                    ("focus_property", s("Optional property name to focus on open")),
                    ("view", one_of(&["table", "source", "trace", "schematic", "apps"], "Optional initial view")),
                ],
                &[],
            ),
            output: obj(
                &[
                    schema("informal.workbench/1"),
                    ("focus_property", json!({"type": ["string", "null"]})),
                    ("view", json!({"type": ["string", "null"]})),
                    ("session", json!({"type": "object", "description": "Full SessionView snapshot for the UI"})),
                ],
                &["schema", "session"],
            ),
            changes: false,
        },
        Tool {
            name: "schematic_get",
            title: "RTL cone & causal schematic graph",
            description: "Extract a bounded, port-aware RTL netlist schematic subgraph in the cone of influence of `name`, annotated with active `why_cex` causal paths and arithmetic state-space hotspots (`mul`, `div`, wide datapath).",
            input: obj(
                &[
                    ("name", s("Target assert or cover property name")),
                    ("max_depth", n("Register stages to expand backward from the property (default: 2)")),
                    ("max_nodes", n("Maximum schematic operator/register nodes (default: 80)")),
                ],
                &["name"],
            ),
            output: obj(
                &[
                    schema("informal.schematic/1"),
                    ("property", s("")),
                    ("max_depth", n("")),
                    ("truncated", b("")),
                    ("nodes", list(json!({"type": "object"}), "Schematic nodes with id, label, signal, kind, op, width, distance, causal, hotspot, expandable, ports")),
                    ("edges", list(json!({"type": "object"}), "Schematic edges with id, source, source_port, target, target_port, width, inverted, causal, signal")),
                ],
                &["schema", "property", "max_depth", "truncated", "nodes", "edges"],
            ),
            changes: false,
        },
        Tool {
            name: "formal_lint",
            title: "Formal-readiness lint & quick-fixes",
            description: "Run svlang dv-lint with the `formal-ready` rule pack (`SV-FPV-001..005`, SVA, CDC, FSM) on the elaborated design's RTL files and return findings with structured 1-click quick-fix byte edits.",
            input: obj(
                &[("file", s("Optional design file filter"))],
                &[],
            ),
            output: obj(
                &[
                    schema("informal.lint/1"),
                    ("count", n("")),
                    ("findings", list(json!({"type": "object"}), "Findings with rule, name, severity, message, file, line, column, fixes")),
                ],
                &["schema", "count", "findings"],
            ),
            changes: false,
        },
        Tool {
            name: "formal_fix_apply",
            title: "Apply a formal-readiness quick-fix",
            description: "Apply a byte-range quick-fix from `formal_lint` to a design file under the workspace root and re-elaborate (or record as a proposal in propose mode).",
            input: obj(
                &[
                    ("file", s("Design file path")),
                    ("start", n("0-based UTF-8 start byte offset")),
                    ("end", n("0-based UTF-8 end byte offset")),
                    ("replacement", s("Replacement SystemVerilog text")),
                    ("rationale", s("Why (required in propose mode)")),
                ],
                &["file", "start", "end", "replacement"],
            ),
            output: obj(
                &[
                    ("schema", s("informal.fix/1, or informal.proposal/1 in propose mode")),
                    ("applied", b("")),
                    ("file", s("")),
                    ("proposal", proposal()),
                ],
                &["schema"],
            ),
            changes: true,
        },
        Tool {
            name: "signal_nav",
            title: "Go-to-driver and go-to-load navigation",
            description: "Resolve declarations, sequential/combinational drivers, and SVA/expression loads of an RTL signal across the design's files.",
            input: obj(
                &[("signal", s("Signal name or hierarchical path, e.g. fifo.count"))],
                &["signal"],
            ),
            output: obj(
                &[
                    schema("informal.signal_nav/1"),
                    ("signal", s("")),
                    ("leaf", s("")),
                    ("declarations", list(json!({"type": "object"}), "")),
                    ("drivers", list(json!({"type": "object"}), "")),
                    ("loads", list(json!({"type": "object"}), "")),
                ],
                &["schema", "signal", "leaf", "declarations", "drivers", "loads"],
            ),
            changes: false,
        },
        Tool {
            name: "fv_complexity",
            title: "Formal complexity & hotspot report",
            description: "Analyze COI state bits, arithmetic operators, memories, and blackbox candidates (`get_fv_complexity`) for a property or the whole task.",
            input: obj(
                &[("property", s("Optional property name filter"))],
                &[],
            ),
            output: obj(
                &[
                    schema("informal.complexity/1"),
                    ("report", json!({"type": "object", "description": "Complexity metrics and hotspot list"})),
                ],
                &["schema", "report"],
            ),
            changes: false,
        },
        Tool {
            name: "tcl_eval",
            title: "Evaluate a Tcl command",
            description: "The escape hatch: evaluate a JasperGold or VC Formal style command in the session's shell. In automatic mode it runs; in propose mode it is recorded as a proposal that waits for a person (give `rationale`). Prefer the other tools: their replies are data.",
            input: obj(&[("command", s("")), ("rationale", s("Why (required in propose mode)"))], &["command"]),
            output: obj(&[("schema", s("informal.tcl/1, or informal.proposal/1 in propose mode")), ("ok", b("")), ("result", s("")), ("error", s("")), ("proposal", proposal())], &["schema"]),
            changes: true,
        },
    ]
}

/// MCP App resource URI served by `resources/list` and `resources/read` (SEP-1865).
pub const APP_URI: &str = "ui://informal/workbench.html";

/// `_meta` attached to `tools/list` entries that open or update the MCP App UI.
pub fn ui_meta(name: &str) -> Option<Value> {
    match name {
        "open_workbench" => Some(json!({
            "ui": {"resourceUri": APP_URI},
            "com.google.antigravity/ui": {
                "entrypoints": [
                    {"type": "aux_pane", "title": "InFormal"},
                    {"type": "global", "title": "InFormal"},
                ]
            }
        })),
        "prove" | "why_cex" | "trace_get" | "schematic_get" | "check_csr" | "check_sec"
        | "check_cov" => Some(json!({
            "ui": {"resourceUri": APP_URI}
        })),
        _ => None,
    }
}

/// What the window shows, as every `gui_` tool returns it.
fn window() -> Value {
    obj(
        &[
            schema("informal.gui/1"),
            ("done", s("The action that was done")),
            (
                "window",
                obj(
                    &[
                        ("selected_property", json!({"type": ["string", "null"]})),
                        (
                            "view",
                            one_of(
                                &["table", "source", "trace", "schematic", "apps"],
                                "What the centre of the window shows",
                            ),
                        ),
                        ("filter", s("The property table's filter text")),
                        (
                            "status_filter",
                            json!({"type": ["string", "null"], "description": "Active status filter on the property table"}),
                        ),
                        (
                            "layout_preset",
                            json!({"type": ["string", "null"], "description": "Active workspace layout preset: triage, cex-debug, signoff, or split"}),
                        ),
                        (
                            "active_app",
                            json!({"type": ["string", "null"], "description": "Active Formal Apps tab when open"}),
                        ),
                        (
                            "bottom_tab",
                            json!({"type": ["string", "null"], "description": "Active bottom dock tab"}),
                        ),
                        (
                            "follow_agent",
                            json!({"type": ["boolean", "null"], "description": "Whether Follow-Agent auto-navigation is active"}),
                        ),
                        (
                            "trace",
                            json!({"type": ["object", "null"], "description": "The open trace: property, cursor, signals, why_cex_open, slice_only, annotations"}),
                        ),
                        (
                            "source",
                            json!({"type": ["object", "null"], "description": "The source in view: file, first_line and last_line of the highlighted lines"}),
                        ),
                        (
                            "schematic",
                            json!({"type": ["object", "null"], "description": "Open schematic cone state: property and max_depth"}),
                        ),
                        (
                            "walkthrough",
                            json!({"type": ["object", "null"], "description": "Active guided CEX walkthrough: title, step, total_steps, label"}),
                        ),
                        (
                            "focus_context",
                            json!({"type": ["array", "null"], "items": {"type": "object"}, "description": "Contextual @signal / @file focus pills pinned by the user"}),
                        ),
                        (
                            "capture",
                            json!({"type": ["object", "null"], "description": "Latest deterministic SVG/view capture produced by gui_capture_view"}),
                        ),
                    ],
                    &["selected_property", "view"],
                ),
            ),
        ],
        &["schema", "window"],
    )
}

/// The tools that work on a person's window: offered only by a desktop
/// app's session. They show the person something or read what they see;
/// none changes the session.
pub fn gui() -> Vec<Tool> {
    vec![
        Tool {
            name: "gui_state",
            title: "What the person is looking at",
            description: "The window of the person working on this session: the property they selected, the view and layout preset, the open trace with their cursor's frame, callouts and signals, the source lines in view, schematic state, and any @-focus pills.",
            input: obj(&[], &[]),
            output: window(),
            changes: false,
        },
        Tool {
            name: "gui_select_property",
            title: "Select a property in the window",
            description: "Select a property in the person's property table, as a click would: its details and source line show.",
            input: obj(&[("name", s("Property name"))], &["name"]),
            output: window(),
            changes: false,
        },
        Tool {
            name: "gui_open_trace",
            title: "Open a trace in the window",
            description: "Open the waveform of a property whose status is cex or covered in the person's window, with the cursor on `frame` (default: the frame where it fails or is hit). Use it to show the person the cycle you are talking about.",
            input: obj(&[("name", s("Property name")), ("frame", n("Frame for the cursor, from 0"))], &["name"]),
            output: window(),
            changes: false,
        },
        Tool {
            name: "gui_add_signals",
            title: "Add signals to the open waveform",
            description: "Add signals, by full name as trace_get gives them, to the waveform that is open in the person's window. Fails when no trace is open or a signal is not in that trace.",
            input: obj(&[("signals", list(s(""), "Full signal names, e.g. fifo.u_wp.p"))], &["signals"]),
            output: window(),
            changes: false,
        },
        Tool {
            name: "gui_highlight_source",
            title: "Show source lines in the window",
            description: "Open a design file or script in the person's source view and highlight a range of lines.",
            input: obj(&[("file", s("Path, as a property's source or the summary's files give it")), ("first_line", n("From 1")), ("last_line", n("Default: first_line"))], &["file", "first_line"]),
            output: window(),
            changes: false,
        },
        Tool {
            name: "gui_set_view",
            title: "Switch window view, layout preset, dock tab, or filter",
            description: "Switch the centre view (`table`, `source`, `trace`, `schematic`, `apps`), workspace layout preset (`triage`, `cex-debug`, `signoff`, `split`), Formal App tab (`cov`, `xprop`, `spv`, `sec`, `csr`, `protocol`), bottom dock tab, or property filter.",
            input: obj(
                &[
                    ("view", one_of(&["table", "source", "trace", "schematic", "apps"], "Centre view to activate")),
                    ("layout_preset", one_of(&["triage", "cex-debug", "signoff", "split"], "Multi-pane workspace layout preset")),
                    ("app_tab", one_of(&["cov", "xprop", "spv", "sec", "csr", "protocol"], "Formal Apps tab to select")),
                    ("bottom_tab", one_of(&["console", "details", "messages", "log", "info", "profiler", "apps", "proposals"], "Bottom dock tab")),
                    ("filter", s("Property filter text")),
                    ("status_filter", json!({"type": ["string", "null"], "description": "Status filter word or null to clear"})),
                ],
                &[],
            ),
            output: window(),
            changes: false,
        },
        Tool {
            name: "gui_toggle_why_cex",
            title: "Toggle Why CEX causal root-cause slice overlay",
            description: "Open or close the `Why CEX` backward causal slice overlay on the open waveform, and optionally filter lanes to causal signals only (`slice_only`).",
            input: obj(
                &[
                    ("open", b("Whether the Why CEX slice bar and cell highlights are shown")),
                    ("slice_only", b("Optional: show only signals in the active causal slice")),
                ],
                &["open"],
            ),
            output: window(),
            changes: false,
        },
        Tool {
            name: "gui_annotate_trace",
            title: "Pin AI callout badges onto waveform cells",
            description: "Pin visual callout badges onto specific `(signal, frame)` cells in the open waveform (or clear them) so the engineer sees exact root-cause transitions.",
            input: obj(
                &[
                    (
                        "annotations",
                        list(
                            obj(
                                &[
                                    ("frame", n("Cycle/frame index from 0")),
                                    ("signal", s("Signal name or leaf")),
                                    ("label", s("Short badge text")),
                                    ("tone", one_of(&["accent", "danger", "ok"], "Optional badge tone")),
                                ],
                                &["frame", "signal", "label"],
                            ),
                            "Callout pins to show on the waveform",
                        ),
                    ),
                    ("clear", b("Clear existing AI waveform callout pins")),
                ],
                &[],
            ),
            output: window(),
            changes: false,
        },
        Tool {
            name: "gui_present_walkthrough",
            title: "Present or step a guided multi-step CEX walkthrough",
            description: "Publish, step, or dismiss an interactive multi-step root-cause tour bar in the window that synchronizes the waveform cursor, callout pins, and RTL source lines on each step.",
            input: obj(
                &[
                    ("title", s("Walkthrough title")),
                    (
                        "steps",
                        list(
                            obj(
                                &[
                                    ("label", s("Short step title")),
                                    ("note", s("Explanation shown on the walkthrough bar")),
                                    ("property", s("Optional trace property to open")),
                                    ("frame", n("Optional trace frame to jump to")),
                                    ("signals", list(s(""), "Optional signals to ensure visible / callout")),
                                    ("file", s("Optional source file to highlight")),
                                    ("first_line", n("Optional source start line")),
                                    ("last_line", n("Optional source end line")),
                                ],
                                &["label", "note"],
                            ),
                            "Ordered walkthrough steps",
                        ),
                    ),
                    ("step", n("0-based step index to jump to")),
                    ("clear", b("Dismiss the active walkthrough bar")),
                ],
                &[],
            ),
            output: window(),
            changes: false,
        },
        Tool {
            name: "gui_open_schematic",
            title: "Open the interactive COI & causal schematic",
            description: "Open the schematic viewer for property `name` (default: selected property) at `max_depth` register stages.",
            input: obj(
                &[
                    ("name", s("Optional property name (defaults to selected property)")),
                    ("max_depth", n("Optional register stage depth (default: 2)")),
                ],
                &[],
            ),
            output: window(),
            changes: false,
        },
        Tool {
            name: "gui_capture_view",
            title: "Capture deterministic SVG or visual summary of the active view",
            description: "Render a standalone SVG of the open waveform or schematic (including Why CEX highlights and AI callout badges) into `window.capture` for visual verification or Markdown reports.",
            input: obj(
                &[(
                    "target",
                    one_of(&["trace", "schematic", "window"], "View to capture (default: trace)"),
                )],
                &[],
            ),
            output: window(),
            changes: false,
        },
    ]
}

/// The tools a server in `mode` offers, with the window's when there is
/// one.
pub fn for_mode(mode: Mode, with_gui: bool) -> Vec<Tool> {
    let mut t = all();
    if mode == Mode::ReadOnly {
        t.retain(|t| !t.changes);
    }
    if with_gui {
        t.extend(gui());
    }
    t
}
