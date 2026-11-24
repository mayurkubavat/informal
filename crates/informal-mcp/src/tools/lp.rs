use super::{b, list, n, obj, one_of, s, schema, Tool};
use serde_json::json;

pub(super) fn all() -> Vec<Tool> {
    vec![
        Tool {
            name: "load_upf",
            title: "Load IEEE 1801 UPF or CPF power intent",
            description: "Parse and resolve an IEEE 1801 UPF (1.0–4.0) or CPF (1.1–2.0) power-intent file against the elaborated design, optionally loading Liberty `.lib` files alongside it.",
            input: obj(
                &[
                    (
                        "file",
                        s("Path to UPF or CPF file below the workspace root"),
                    ),
                    (
                        "scope",
                        s("Optional hierarchical instance scope for scoped UPF loading"),
                    ),
                    (
                        "version",
                        one_of(
                            &["1.0", "2.0", "2.1", "3.0", "3.1", "4.0"],
                            "Optional UPF version override",
                        ),
                    ),
                    (
                        "format",
                        one_of(
                            &["upf", "cpf"],
                            "Optional format override (`upf` or `cpf`; auto-detected from file extension when omitted)",
                        ),
                    ),
                    (
                        "cpf",
                        b("Load file as Cadence CPF (alias for `format: \"cpf\"`)"),
                    ),
                    (
                        "liberty",
                        list(
                            s(""),
                            "Optional Liberty `.lib` file path(s) below the workspace root",
                        ),
                    ),
                ],
                &["file"],
            ),
            output: obj(
                &[
                    schema("informal.upf/1"),
                    ("file", s("Canonical loaded UPF/CPF path")),
                    ("upf_version", json!({"type": ["string", "null"]})),
                    (
                        "domains",
                        list(
                            obj(
                                &[
                                    ("name", s("")),
                                    ("scope", s("")),
                                    ("is_top", b("")),
                                    ("primary_supply_set", json!({"type": ["string", "null"]})),
                                    ("elements", list(s(""), "")),
                                    ("instance_count", n("")),
                                ],
                                &["name", "scope", "is_top", "elements", "instance_count"],
                            ),
                            "Resolved power domains",
                        ),
                    ),
                    (
                        "supply_sets",
                        list(
                            obj(
                                &[
                                    ("name", s("")),
                                    ("power_net", json!({"type": ["string", "null"]})),
                                    ("ground_net", json!({"type": ["string", "null"]})),
                                    ("states", list(s(""), "")),
                                ],
                                &["name", "states"],
                            ),
                            "Resolved supply sets",
                        ),
                    ),
                    (
                        "supply_nets",
                        list(
                            obj(
                                &[
                                    ("name", s("")),
                                    ("scope", s("")),
                                    ("ports", list(s(""), "")),
                                ],
                                &["name", "scope", "ports"],
                            ),
                            "Resolved supply nets",
                        ),
                    ),
                    (
                        "pst",
                        obj(
                            &[
                                ("consistent", json!({"type": ["boolean", "null"]})),
                                ("tables", n("")),
                                ("power_states", n("")),
                                ("illegal_combinations", n("")),
                            ],
                            &["tables", "power_states", "illegal_combinations"],
                        ),
                    ),
                    (
                        "strategies",
                        obj(
                            &[
                                ("isolation", n("")),
                                ("level_shifter", n("")),
                                ("retention", n("")),
                                ("power_switch", n("")),
                                ("repeater", n("")),
                            ],
                            &[
                                "isolation",
                                "level_shifter",
                                "retention",
                                "power_switch",
                                "repeater",
                            ],
                        ),
                    ),
                    (
                        "diagnostics",
                        list(
                            obj(
                                &[
                                    ("code", s("")),
                                    ("severity", s("")),
                                    ("message", s("")),
                                    ("file", json!({"type": ["string", "null"]})),
                                    ("line", json!({"type": ["integer", "null"]})),
                                ],
                                &["code", "severity", "message"],
                            ),
                            "UPF/CPF parse and resolution diagnostics",
                        ),
                    ),
                ],
                &[
                    "schema",
                    "file",
                    "domains",
                    "supply_sets",
                    "supply_nets",
                    "pst",
                    "strategies",
                    "diagnostics",
                ],
            ),
            changes: true,
        },
        Tool {
            name: "check_lp",
            title: "Run static and formal low-power verification",
            description: "Execute UPF consistency (`upf`), RTL crossing checks (`rtl`/`design`), gate/PG netlist checks (`netlist`, `pg`), formal low-power AAG/corruption/certified auto-waiver verification (`formal`, `prove_corruption`, `prove_waivers`), or power-aware equivalence (`sec`, `lec`).",
            input: obj(
                &[
                    (
                        "stage",
                        one_of(
                            &["upf", "rtl", "design", "netlist", "pg", "formal", "all"],
                            "Verification stage to run (default: `all`)",
                        ),
                    ),
                    (
                        "prove_corruption",
                        b("Run exact 2-copy corruption propagation proofs"),
                    ),
                    (
                        "prove_waivers",
                        b("Run closed-loop certified auto-waiver proofs on open crossing violations"),
                    ),
                    (
                        "sec",
                        b("Run power-aware sequential equivalence check (`LowPower` SEC)"),
                    ),
                    (
                        "lec",
                        b("Run power-aware combinational equivalence check (`LowPower` LEC)"),
                    ),
                    (
                        "tol_mv",
                        json!({"type": "number", "description": "Voltage comparison tolerance in millivolts (default: 1.0)"}),
                    ),
                ],
                &[],
            ),
            output: obj(
                &[
                    schema("informal.lp/1"),
                    ("stages_run", list(s(""), "")),
                    ("pst_consistent", json!({"type": ["boolean", "null"]})),
                    (
                        "summary",
                        obj(
                            &[
                                ("total", n("")),
                                ("errors", n("")),
                                ("warnings", n("")),
                                ("notes", n("")),
                                ("open", n("")),
                                ("waived", n("")),
                                ("formally_waived", n("")),
                                ("cex", n("")),
                                ("by_rule", list(json!({"type": "object"}), "")),
                            ],
                            &[
                                "total",
                                "errors",
                                "warnings",
                                "notes",
                                "open",
                                "waived",
                                "formally_waived",
                                "cex",
                                "by_rule",
                            ],
                        ),
                    ),
                    ("stats", json!({"type": ["object", "null"]})),
                    (
                        "violations",
                        list(
                            json!({"type": "object"}),
                            "LP violations with id, rule_id, severity, stage, domain_src, domain_dst, signal, status, pst_witness, upf_loc, rtl_loc, formal_prop, trace, auto_formal_meta",
                        ),
                    ),
                    ("waivers", list(json!({"type": "object"}), "")),
                ],
                &["schema", "stages_run", "summary", "violations"],
            ),
            changes: true,
        },
        Tool {
            name: "lp_diagnose",
            title: "Single-call low-power violation diagnostic dossier",
            description: "Read-only diagnostic dossier for a low-power violation: fuses violation metadata, symbolic PST witness, UPF source snippet, RTL crossing path and driver snippets, `why_cex` backward causal slice (when a formal CEX exists), and deterministic remediation guidance.",
            input: obj(
                &[
                    (
                        "id",
                        json!({
                            "type": ["integer", "string"],
                            "description": "Violation ID from `check_lp` (integer e.g. `1`, or formatted string e.g. `\"LP-0001\"`)"
                        }),
                    ),
                    (
                        "violation",
                        s("Optional violation selector alias (ID, rule ID, or formal property name)"),
                    ),
                    (
                        "rule",
                        s("Optional rule ID filter when `id` is omitted (e.g. `ISO_MISSING`)"),
                    ),
                    (
                        "signal",
                        s("Optional signal substring filter when `id` is omitted"),
                    ),
                    (
                        "source_context_lines",
                        n("Context lines above/below UPF and RTL source snippets (default: 4, max: 12)"),
                    ),
                    (
                        "max_events",
                        n("Maximum `why_cex` causal events when a formal trace exists (default: 24, max: 64)"),
                    ),
                ],
                &[],
            ),
            output: obj(
                &[
                    schema("informal.lp_diagnose/1"),
                    ("violation", json!({"type": "object"})),
                    ("pst_witness", json!({"type": "object"})),
                    ("upf_loc", json!({"type": ["object", "null"]})),
                    ("upf_snippet", json!({"type": ["object", "null"]})),
                    ("rtl_loc", json!({"type": ["string", "null"]})),
                    (
                        "rtl_crossing_path",
                        obj(
                            &[
                                ("signal", s("")),
                                ("domain_src", json!({"type": ["string", "null"]})),
                                ("domain_dst", json!({"type": ["string", "null"]})),
                                ("width_bits", n("")),
                                ("raw_crossings", n("")),
                                ("rtl_loc", json!({"type": ["string", "null"]})),
                                ("summary", s("")),
                            ],
                            &["signal", "width_bits", "raw_crossings", "summary"],
                        ),
                    ),
                    (
                        "driver_snippets",
                        list(
                            json!({"type": "object"}),
                            "RTL source and driver snippets around the crossing signal",
                        ),
                    ),
                    ("why_cex", json!({"type": ["object", "null"]})),
                    ("hint", s("Deterministic remediation guidance")),
                ],
                &[
                    "schema",
                    "violation",
                    "pst_witness",
                    "rtl_crossing_path",
                    "driver_snippets",
                    "hint",
                ],
            ),
            changes: false,
        },
    ]
}
