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
            name: "tcl_eval",
            title: "Evaluate a Tcl command",
            description: "The escape hatch: evaluate a JasperGold or VC Formal style command in the session's shell. In automatic mode it runs; in propose mode it is recorded as a proposal that waits for a person (give `rationale`). Prefer the other tools: their replies are data.",
            input: obj(&[("command", s("")), ("rationale", s("Why (required in propose mode)"))], &["command"]),
            output: obj(&[("schema", s("informal.tcl/1, or informal.proposal/1 in propose mode")), ("ok", b("")), ("result", s("")), ("error", s("")), ("proposal", proposal())], &["schema"]),
            changes: true,
        },
    ]
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
                                &["table", "source", "trace"],
                                "What the centre of the window shows",
                            ),
                        ),
                        ("filter", s("The property table's filter text")),
                        (
                            "trace",
                            json!({"type": ["object", "null"], "description": "The open trace: property, cursor (the frame the person's cursor is on) and signals (those in the waveform)"}),
                        ),
                        (
                            "source",
                            json!({"type": ["object", "null"], "description": "The source in view: file, first_line and last_line of the highlighted lines"}),
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
            description: "The window of the person working on this session: the property they selected, the view that is open, the open trace with their cursor's frame and the signals in the waveform, and the source lines in view. Use it to answer about \"this property\" or \"this signal\".",
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
