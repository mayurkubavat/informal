use super::{b, list, n, obj, one_of, property, proposal, s, schema, summary, Tool, STATUS};
use serde_json::json;

pub(super) fn session_and_proposals() -> Vec<Tool> {
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
            name: "proposal_validate",
            title: "Validate a candidate assumption, helper assertion, cover, or cut point",
            description: "Zero-side-effect scratch-task soundness oracle: tests a candidate `assume`, helper `assert` (lemma), `cover`, or `cut_point` BEFORE calling `proposal_add`. Checks syntax, SVA-FPV-001 DUT-output over-constraint hygiene, bounded environment consistency + UNSAT conflict core, 2QBF dead-end trap states, vacuity, cover non-regression (`lost_covers`), and whether target assertions become `proven`. Leaves the live session completely untouched.",
            input: obj(
                &[
                    ("kind", one_of(&["assume", "assert", "cover", "cut_point"], "")),
                    ("name", s("Optional property name")),
                    ("expr", s("Property expression (for assume, assert, or cover)")),
                    ("signal", s("Signal path to cut (for cut_point)")),
                    ("targets", list(s(""), "Optional target assertions to test impact on (default: all cex/undetermined assertions in current task)")),
                    ("check_covers", b("Re-check existing covers to detect over-constraint (default: true for assume/cut_point)")),
                    ("check_dead_end", b("Run 2QBF dead-end trap-state detection on assumptions (default: true for assume)")),
                    ("depth", n("Bounded environment check depth in model steps (default: 20)")),
                    ("time_limit_s", json!({"type": "number", "minimum": 1, "description": "Per-property time limit in seconds (default: 15)"})),
                ],
                &["kind"],
            ),
            output: obj(
                &[
                    schema("informal.validate/1"),
                    ("verdict", one_of(&["pass", "warn", "reject"], "pass = sound & effective; warn = check warnings/coverage; reject = unsafe or failed")),
                    ("ready_to_propose", b("True when verdict is pass or warn")),
                    ("kind", one_of(&["assume", "assert", "cover", "cut_point"], "")),
                    ("text", s("")),
                    ("environment", json!({"type": "object"})),
                    ("dead_end", json!({"type": "object"})),
                    ("overconstraint_warnings", list(s(""), "Static SVA-FPV-001 / internal-signal warnings")),
                    ("candidate_result", json!({"type": ["object", "null"], "description": "Prove result of candidate assert/cover itself"})),
                    ("newly_proven", list(s(""), "Target assertions that became proven and non-vacuous")),
                    ("vacuous_assertions", list(s(""), "Target assertions whose proof became vacuous")),
                    ("lost_covers", list(s(""), "Existing covers that became unreachable (over-constraint)")),
                    ("still_failing", list(s(""), "Target assertions still cex, undetermined, or error")),
                    ("target_results", list(json!({"type": "object"}), "")),
                    ("cover_results", list(json!({"type": "object"}), "")),
                    ("next_step", s("Actionable guidance for the next tool call")),
                ],
                &[
                    "schema",
                    "verdict",
                    "ready_to_propose",
                    "kind",
                    "text",
                    "environment",
                    "dead_end",
                    "overconstraint_warnings",
                    "newly_proven",
                    "vacuous_assertions",
                    "lost_covers",
                    "still_failing",
                    "next_step",
                ],
            ),
            changes: false,
        },
        Tool {
            name: "proposal_add",
            title: "Propose a change",
            description: "Propose an assumption, a helper assertion, a cover (`expr`, SystemVerilog in the top module's scope) or a cut point (`signal`). Call `proposal_validate` first to verify soundness, non-vacuity, and cover non-regression. In propose mode it waits for a person (state pending); in automatic mode it is applied at once, tagged AI-added.",
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
    ]
}

pub(super) fn prove_cache() -> Tool {
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
        }
}

pub(super) fn tcl_eval() -> Tool {
    Tool {
            name: "tcl_eval",
            title: "Evaluate a Tcl command",
            description: "The escape hatch: evaluate a JasperGold or VC Formal style command in the session's shell. In automatic mode it runs; in propose mode it is recorded as a proposal that waits for a person (give `rationale`). Prefer the other tools: their replies are data.",
            input: obj(&[("command", s("")), ("rationale", s("Why (required in propose mode)"))], &["command"]),
            output: obj(&[("schema", s("informal.tcl/1, or informal.proposal/1 in propose mode")), ("ok", b("")), ("result", s("")), ("error", s("")), ("proposal", proposal())], &["schema"]),
            changes: true,
        }
}
