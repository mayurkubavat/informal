use super::{b, list, n, obj, one_of, property, proposal, s, schema, Tool};
use serde_json::json;

pub(super) fn cex_and_convergence() -> Vec<Tool> {
    vec![
        Tool {
            name: "why_cex",
            title: "Root-cause slice of a counterexample trace",
            description: "Dynamic backward slice from a falsified assertion (`cex`, including `check_sec`, `check_xprop`, and `check_spv` counterexamples) or reached cover (`covered`) at failure frame T back to frame 0: walks only active mux arms (`ite`), controlling boolean operands, and register state transitions, pruning untaken datapaths and idle hold cycles.",
            input: obj(
                &[
                    ("name", s("Property, SEC check, X-prop item, or SPV rule name")),
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
            name: "cex_diagnose",
            title: "Single-call counterexample root-cause dossier",
            description: "Preferred first tool for any `cex` or `covered` property: fuses `property_get`, backward `why_cex` causal slice, trimmed trace on active causal signals only, primary-input vs DUT-output/internal signal role classification, RTL driver source snippets (`signal_nav`), `formal_lint` findings, and a triage hint into one compact reply.",
            input: obj(
                &[
                    ("name", s("Property name (must have status cex or covered)")),
                    ("window", n("Cycles before failure_frame to include in active_trace (default: 6, max: 32)")),
                    ("max_events", n("Maximum causal events (default: 24, max: 64)")),
                    ("source_context_lines", n("Context lines above/below each RTL driver (default: 4, max: 12)")),
                ],
                &["name"],
            ),
            output: obj(
                &[
                    schema("informal.cex_diagnose/1"),
                    ("property", property()),
                    ("causal_slice", json!({"type": "object"})),
                    ("active_trace", json!({"type": "object"})),
                    ("signal_roles", json!({"type": "object", "description": "Active signal -> primary_input | dut_output | inout | internal_state | unknown"})),
                    ("driver_snippets", list(json!({"type": "object"}), "RTL driver snippets for top active causal signals")),
                    ("lint_findings", list(json!({"type": "object"}), "Formal-readiness lint hits on design RTL")),
                    ("triage_hint", s("Deterministic root-cause triage summary")),
                ],
                &[
                    "schema",
                    "property",
                    "causal_slice",
                    "active_trace",
                    "signal_roles",
                    "driver_snippets",
                    "lint_findings",
                    "triage_hint",
                ],
            ),
            changes: false,
        },
        Tool {
            name: "convergence_diagnose",
            title: "Single-call proof convergence & complexity dossier",
            description: "Preferred first tool for `undetermined` or slow properties: fuses per-engine telemetry (`stats`, `max_bound`), COI input/register counts, `fv_complexity` arithmetic/memory/blackbox hotspots, active assumptions/cutpoints, and ranked proof-convergence tactics.",
            input: obj(
                &[("name", s("Assert or cover property name"))],
                &["name"],
            ),
            output: obj(
                &[
                    schema("informal.convergence_diagnose/1"),
                    ("property", property()),
                    ("cone", json!({"type": "object"})),
                    ("complexity", json!({"type": "object"})),
                    ("assumptions_in_force", list(s(""), "")),
                    ("cut_points_in_force", list(s(""), "")),
                    ("recommendations", list(s(""), "Ranked convergence actions")),
                ],
                &[
                    "schema",
                    "property",
                    "cone",
                    "complexity",
                    "assumptions_in_force",
                    "cut_points_in_force",
                    "recommendations",
                ],
            ),
            changes: false,
        },
    ]
}

pub(super) fn rtl_and_spec() -> Vec<Tool> {
    vec![
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
                    ("nodes", list(json!({"type": "object"}), "Schematic nodes with id, label, signal, kind, op, width, distance, causal, hotspot, expandable, power_domain (when UPF loaded), ports")),
                    ("edges", list(json!({"type": "object"}), "Schematic edges with id, source, source_port, target, target_port, width, inverted, causal, signal")),
                ],
                &["schema", "property", "max_depth", "truncated", "nodes", "edges"],
            ),
            changes: false,
        },
        Tool {
            name: "formal_lint",
            title: "Formal-readiness lint & quick-fixes",
            description: "Run svlang lint with the `formal-ready` rule pack (`SV-FPV-001..005`, SVA, CDC, FSM) on the elaborated design's RTL files and return findings with structured 1-click quick-fix byte edits.",
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
            name: "spec_audit",
            title: "Audit session against fvp.spec.json",
            description: "Cross-check the active session against a machine-verifiable formal verification spec (`fvp.spec.json` / `informal.spec/1`): verifies design/clock/reset intent, legal primary-input-only assumption boundary, per-requirement assertion + witness-cover status (`verified`, `cex`, `undetermined`, `missing_assert`, `missing_cover`, `vacuous`, `overconstrained`), signoff gates, and prioritized next actions. Auto-discovers `fvp.spec.json` next to the design files when `spec_file` and `spec` are omitted.",
            input: obj(
                &[
                    ("spec_file", s("Optional path to fvp.spec.json (auto-discovered next to open design files when omitted)")),
                    ("spec", json!({"type": "object", "description": "Optional inline `informal.spec/1` JSON object instead of reading a file"})),
                    ("task", s("Optional task name to audit (defaults to current_task)")),
                    ("check_dead_end", b("Run live environment consistency + 2QBF dead-end freedom check (default false)")),
                    ("depth", n("Bounded depth for check_dead_end (default 10)")),
                    ("mutation_score", json!({"type": "number", "description": "Optional mutant-kill percentage (0..100) to evaluate against signoff.mutation_score_floor"})),
                ],
                &[],
            ),
            output: obj(
                &[
                    schema("informal.spec_audit/1"),
                    ("spec_source", s("Canonical fvp.spec.json path or <inline>")),
                    ("task", s("Audited task name")),
                    ("signoff_ready", b("True iff all requirements are verified and all signoff gates pass")),
                    (
                        "design_check",
                        obj(
                            &[
                                ("ok", b("")),
                                ("expected_top", json!({"type": ["string", "null"]})),
                                ("actual_top", json!({"type": ["string", "null"]})),
                                ("expected_reset", json!({"type": ["string", "null"]})),
                                ("actual_reset", json!({"type": ["string", "null"]})),
                                ("clocks_ok", b("")),
                                ("mismatches", list(s(""), "")),
                            ],
                            &["ok", "clocks_ok", "mismatches"],
                        ),
                    ),
                    (
                        "boundary_audit",
                        obj(
                            &[
                                ("ok", b("")),
                                (
                                    "overconstrained_assumptions",
                                    list(
                                        obj(
                                            &[
                                                ("name", s("")),
                                                ("expr", s("")),
                                                ("forbidden_signals", list(s(""), "")),
                                                ("roles", list(s(""), "")),
                                                ("reason", s("")),
                                            ],
                                            &["name", "expr", "forbidden_signals", "roles", "reason"],
                                        ),
                                        "",
                                    ),
                                ),
                                ("conflict", b("")),
                                ("conflict_set", list(s(""), "")),
                                ("dead_end_checked", b("")),
                                ("dead_end_found", b("")),
                            ],
                            &["ok", "overconstrained_assumptions", "conflict", "conflict_set", "dead_end_checked", "dead_end_found"],
                        ),
                    ),
                    (
                        "summary",
                        obj(
                            &[
                                ("total_requirements", n("")),
                                ("verified", n("")),
                                ("cex", n("")),
                                ("undetermined", n("")),
                                ("missing_assert", n("")),
                                ("missing_cover", n("")),
                                ("vacuous", n("")),
                                ("overconstrained", n("")),
                                ("unmapped_assertions", list(s(""), "")),
                            ],
                            &["total_requirements", "verified", "cex", "undetermined", "missing_assert", "missing_cover", "vacuous", "overconstrained", "unmapped_assertions"],
                        ),
                    ),
                    (
                        "gates",
                        list(
                            obj(
                                &[
                                    ("gate", s("")),
                                    ("ok", b("")),
                                    ("expected", s("")),
                                    ("actual", s("")),
                                    ("detail", s("")),
                                ],
                                &["gate", "ok", "expected", "actual", "detail"],
                            ),
                            "",
                        ),
                    ),
                    (
                        "requirements",
                        list(
                            obj(
                                &[
                                    ("id", s("")),
                                    ("title", s("")),
                                    (
                                        "status",
                                        one_of(
                                            &[
                                                "verified",
                                                "cex",
                                                "undetermined",
                                                "missing_assert",
                                                "missing_cover",
                                                "vacuous",
                                                "overconstrained",
                                            ],
                                            "",
                                        ),
                                    ),
                                    (
                                        "assertions",
                                        list(
                                            obj(
                                                &[
                                                    ("name", s("")),
                                                    ("found", b("")),
                                                    ("status", s("")),
                                                    ("vacuity", s("")),
                                                    ("under_ai", list(s(""), "")),
                                                ],
                                                &["name", "found", "status", "vacuity", "under_ai"],
                                            ),
                                            "",
                                        ),
                                    ),
                                    (
                                        "covers",
                                        list(
                                            obj(
                                                &[
                                                    ("name", s("")),
                                                    ("found", b("")),
                                                    ("status", s("")),
                                                ],
                                                &["name", "found", "status"],
                                            ),
                                            "",
                                        ),
                                    ),
                                    ("issues", list(s(""), "")),
                                    ("next_action", s("")),
                                ],
                                &["id", "title", "status", "assertions", "covers", "issues", "next_action"],
                            ),
                            "",
                        ),
                    ),
                    ("uncovered_requirements", list(s(""), "")),
                    ("next_actions", list(s(""), "")),
                ],
                &[
                    "schema",
                    "spec_source",
                    "task",
                    "signoff_ready",
                    "design_check",
                    "boundary_audit",
                    "summary",
                    "gates",
                    "requirements",
                    "uncovered_requirements",
                    "next_actions",
                ],
            ),
            changes: false,
        },
    ]
}
