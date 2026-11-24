use super::{b, list, n, obj, one_of, s, schema, Tool};
use serde_json::{json, Value};

pub(super) fn open_workbench() -> Tool {
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
pub(super) fn all_gui() -> Vec<Tool> {
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
            description: "Switch the centre view (`table`, `source`, `trace`, `schematic`, `apps`), workspace layout preset (`triage`, `cex-debug`, `signoff`, `split`), Formal App tab (`cov`, `xprop`, `spv`, `sec`, `csr`, `protocol`, `lp`), bottom dock tab, or property filter.",
            input: obj(
                &[
                    ("view", one_of(&["table", "source", "trace", "schematic", "apps"], "Centre view to activate")),
                    ("layout_preset", one_of(&["triage", "cex-debug", "signoff", "split"], "Multi-pane workspace layout preset")),
                    ("app_tab", one_of(&["cov", "xprop", "spv", "sec", "csr", "protocol", "lp"], "Formal Apps tab to select")),
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
            name: "gui_present_cex",
            title: "Atomically present a synchronized CEX debug walkthrough",
            description: "Preferred single-call desktop visual tour for a `cex` or `covered` property: switches to `cex-debug` (or chosen layout preset), opens the trace at `frame`, activates the `Why CEX` causal slice, pins AI callout badges, and launches the interactive walkthrough bar. Auto-synthesizes steps and callouts from `why_cex` when omitted.",
            input: obj(
                &[
                    ("name", s("Property name (status cex or covered)")),
                    ("title", s("Optional walkthrough title (default: Why <name> fails)")),
                    ("frame", n("Optional initial cursor frame (default: failure_frame)")),
                    ("layout_preset", one_of(&["triage", "cex-debug", "signoff", "split"], "Workspace layout preset (default: cex-debug)")),
                    ("slice_only", b("Filter waveform lanes to active causal signals only (default: false)")),
                    ("steps", list(json!({"type": "object"}), "Optional custom walkthrough steps (auto-generated from why_cex if omitted)")),
                    ("annotations", list(json!({"type": "object"}), "Optional custom waveform callout badges (auto-generated from why_cex if omitted)")),
                    ("capture", b("Also capture a standalone trace SVG into window.capture (default: false)")),
                ],
                &["name"],
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
