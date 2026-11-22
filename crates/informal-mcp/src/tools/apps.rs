use super::{b, list, n, obj, one_of, property, s, schema, Tool};

pub(super) fn all() -> Vec<Tool> {
    vec![
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
                                    ("trace", s("")),
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
                                    ("trace", s("")),
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
    ]
}
