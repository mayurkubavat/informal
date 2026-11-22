//! The tools as a client sees them: name, what it does, and JSON Schemas
//! of its arguments and its reply.

use crate::api::Mode;
use serde_json::{json, Value};

mod apps;
mod core;
mod diagnose;
mod gui;

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
pub(super) fn described(mut schema: Value, description: &str) -> Value {
    if !description.is_empty() {
        schema["description"] = json!(description);
    }
    schema
}

pub(super) fn s(description: &str) -> Value {
    described(json!({"type": "string"}), description)
}

pub(super) fn n(description: &str) -> Value {
    described(json!({"type": "integer", "minimum": 0}), description)
}

pub(super) fn b(description: &str) -> Value {
    described(json!({"type": "boolean"}), description)
}

pub(super) fn list(items: Value, description: &str) -> Value {
    described(json!({"type": "array", "items": items}), description)
}

pub(super) fn one_of(values: &[&str], description: &str) -> Value {
    described(json!({"type": "string", "enum": values}), description)
}

/// The reply's `schema` field: its name and version.
pub(super) fn schema(name: &str) -> (&'static str, Value) {
    ("schema", json!({"type": "string", "const": name}))
}

pub(super) const STATUS: &[&str] = &[
    "unprocessed",
    "proven",
    "bounded_proven",
    "cex",
    "covered",
    "unreachable",
    "undetermined",
    "error",
];

pub(super) fn property() -> Value {
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

pub(super) fn proposal() -> Value {
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

pub(super) fn summary() -> Value {
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
    let mut out = core::session_and_proposals();
    out.extend(diagnose::cex_and_convergence());
    out.push(core::prove_cache());
    out.extend(apps::all());
    out.push(gui::open_workbench());
    out.extend(diagnose::rtl_and_spec());
    out.push(core::tcl_eval());
    out
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
        "prove" | "why_cex" | "cex_diagnose" | "trace_get" | "schematic_get" | "check_csr"
        | "check_sec" | "check_cov" => Some(json!({
            "ui": {"resourceUri": APP_URI}
        })),
        _ => None,
    }
}

/// The tools that work on a person's window: offered only by a desktop
/// app's session. They show the person something or read what they see;
/// none changes the session.
pub fn gui() -> Vec<Tool> {
    gui::all_gui()
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
