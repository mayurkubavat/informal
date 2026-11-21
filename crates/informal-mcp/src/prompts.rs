//! Server-hosted MCP Prompts (`prompts/list`, `prompts/get`): expert formal
//! verification playbooks dynamically hydrated with the live session's top
//! module, port directions, clocks/reset, and failing/undetermined properties.

use serde_json::{json, Value};

pub struct PromptSpec {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub arguments: &'static [(&'static str, &'static str, bool)],
}

const PROMPTS: &[PromptSpec] = &[
    PromptSpec {
        name: "triage_cex",
        title: "Triage a failing counterexample (CEX)",
        description: "Expert root-cause workflow: fuse causal slice + driver snippets via `cex_diagnose`, classify RTL bug vs missing input assumption vs broken SVA, validate any fix with `proposal_validate`, and optionally launch a visual walkthrough via `gui_present_cex`.",
        arguments: &[(
            "property",
            "Name of the failing (`cex`) property to triage (optional: defaults to first failing property)",
            false,
        )],
    },
    PromptSpec {
        name: "close_undetermined",
        title: "Close an undetermined formal proof",
        description: "Structured convergence ladder for `undetermined` properties: inspect complexity/telemetry via `convergence_diagnose`, mine inductive helper invariants with `proposal_validate(kind=\"assert\")`, or apply sound cutpoint/blackbox/case-split abstractions.",
        arguments: &[(
            "property",
            "Name of the `undetermined` property to converge (optional: defaults to first undetermined property)",
            false,
        )],
    },
    PromptSpec {
        name: "spec_to_sva",
        title: "Synthesize verifiable SVA from a specification requirement",
        description: "Spec-driven SVA engineering hydrated with the DUT's exact input/output port boundary: write reset-guarded safety assertions paired with non-vacuity witness covers and verify them via `proposal_validate` before proposing.",
        arguments: &[
            (
                "requirement",
                "Natural-language hardware specification requirement to formalize",
                true,
            ),
            (
                "module",
                "Optional module/scope name (defaults to top module)",
                false,
            ),
        ],
    },
    PromptSpec {
        name: "audit_signoff",
        title: "Formal verification signoff & soundness audit",
        description: "Run a complete signoff gate across the live session: `spec_audit` against `fvp.spec.json` (if present), `formal_lint`, assumption over-constraint/vacuity audit, `under_ai` provenance review, cover reachability, and `check_cov` COI/UNR coverage.",
        arguments: &[(
            "spec_file",
            "Optional path to `fvp.spec.json` (`informal.spec/1`); auto-discovered when omitted",
            false,
        )],
    },
];

pub fn list() -> Value {
    let items: Vec<Value> = PROMPTS
        .iter()
        .map(|p| {
            let args: Vec<Value> = p
                .arguments
                .iter()
                .map(|(name, desc, req)| {
                    json!({
                        "name": name,
                        "description": desc,
                        "required": req,
                    })
                })
                .collect();
            json!({
                "name": p.name,
                "title": p.title,
                "description": p.description,
                "arguments": args,
            })
        })
        .collect();
    json!({"prompts": items})
}

pub fn get(name: &str, args: &Value, ctx: &Value) -> Result<Value, String> {
    let top = ctx["top"].as_str().unwrap_or("(not elaborated)");
    let clocks = ctx["clocks"].as_str().unwrap_or("none");
    let reset = ctx["reset"].as_str().unwrap_or("none");
    let mode = ctx["mode"].as_str().unwrap_or("propose");
    let has_gui = ctx["has_gui"].as_bool().unwrap_or(false);
    let inputs = ctx["inputs"].as_str().unwrap_or("(unknown)");
    let outputs = ctx["outputs"].as_str().unwrap_or("(unknown)");
    let cex_list = ctx["cex_properties"].as_str().unwrap_or("none");
    let undet_list = ctx["undetermined_properties"].as_str().unwrap_or("none");

    let arg = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };

    let (description, text) = match name {
        "triage_cex" => {
            let target = arg("property")
                .map(String::from)
                .or_else(|| {
                    cex_list
                        .split(',')
                        .map(str::trim)
                        .find(|s| !s.is_empty() && *s != "none")
                        .map(String::from)
                })
                .unwrap_or_else(|| "<failing_property>".into());
            let gui_step = if has_gui {
                format!("5. **Visual walkthrough**: Call `gui_present_cex(name=\"{target}\")` to show the engineer the synchronized CEX Debug layout, Why-CEX slice, callout pins, and walkthrough bar.")
            } else {
                "5. **Interactive workbench**: Call `open_workbench` if helpful so the engineer can inspect the waveform and causal schematic.".into()
            };
            (
                PROMPTS[0].description,
                format!(
                    "You are triaging failing formal property `{target}` on top module `{top}` (clocks: {clocks}; reset: {reset}; server mode: {mode}).\n\
                     Current failing (`cex`) properties: {cex_list}\n\
                     DUT primary inputs: {inputs}\n\
                     DUT outputs: {outputs}\n\n\
                     Follow this exact expert triage protocol:\n\
                     1. **Single-call diagnosis**: Call `cex_diagnose(name=\"{target}\")` first. Do NOT dump full traces before reading the causal slice, signal roles, and RTL driver snippets.\n\
                     2. **Classify root cause** into exactly one bucket with concrete cycle/value evidence:\n\
                        - **RTL bug**: Legal primary-input stimulus drives an illegal DUT state/output transition. Explain the exact cycle, signal values, and offending RTL line. NEVER mask an RTL bug with an `assume`.\n\
                        - **Missing environment assumption**: Unconstrained primary input(s) violate the interface protocol. Propose the **weakest** primary-input-only constraint (never constrain DUT outputs or internal registers).\n\
                        - **Property / reset-guard bug**: e.g. missing `disable iff`, off-by-one `|->` vs `|=>`, or `$past` sampled at cycle 0.\n\
                     3. **Validate before proposing**: Before calling `proposal_add`, ALWAYS call `proposal_validate` on any candidate `assume`, `assert`, or `cut_point`. Confirm `verdict` is not `reject` (no environment conflict, no dead-end trap state, zero lost covers, zero vacuous assertions).\n\
                     4. **Decision D17**: Never claim `{target}` holds until a `prove` (or `proposal_validate` preview + user approval + `prove`) returns `proven` and non-vacuous.\n\
                     {gui_step}"
                ),
            )
        }
        "close_undetermined" => {
            let target = arg("property")
                .map(String::from)
                .or_else(|| {
                    undet_list
                        .split(',')
                        .map(str::trim)
                        .find(|s| !s.is_empty() && *s != "none")
                        .map(String::from)
                })
                .unwrap_or_else(|| "<undetermined_property>".into());
            (
                PROMPTS[1].description,
                format!(
                    "You are closing `undetermined` formal property `{target}` on `{top}` (clocks: {clocks}; reset: {reset}; mode: {mode}).\n\
                     Current undetermined properties: {undet_list}\n\n\
                     Follow this proof-convergence ladder:\n\
                     1. **Diagnose stall**: Call `convergence_diagnose(name=\"{target}\")` to read per-engine bounds (`max_bound`), COI register count, and `fv_complexity` arithmetic/memory hotspots.\n\
                     2. **Pick the lowest-risk tactic matching the bottleneck**:\n\
                        - *Inductive weakness* (deep BMC bound with no CEX, IC3 stalls): Inspect RTL state invariants (FIFO count vs pointers, one-hot FSM encodings, monotonic bounds). Test each candidate helper invariant with `proposal_validate(kind=\"assert\", expr=..., targets=[\"{target}\"])` — this proves the lemma AND tests whether promoting it closes `{target}` without touching the user session.\n\
                        - *Wide arithmetic / memory hotspot*: Test an over-approximate abstraction with `proposal_validate(kind=\"cut_point\", signal=..., targets=[\"{target}\"])`. Remember a `cut_point` proof is sound for the real design (spurious CEXs are possible, never false proofs).\n\
                        - *Case explosion*: Partition mutually exclusive operating modes into separate tasks.\n\
                     3. **Guard against vacuity**: Confirm any resulting proof is `non-vacuous` and document every `under_ai` dependency clearly."
                ),
            )
        }
        "spec_to_sva" => {
            let req = arg("requirement")
                .ok_or_else(|| "`requirement` argument is required".to_string())?;
            let scope = arg("module").unwrap_or(top);
            let spec_note = match ctx["spec_file"].as_str() {
                Some(sf) => format!(
                    "\n5. Cross-check requirement coverage with `spec_audit(spec_file=\"{sf}\")` so every `REQ-*` maps to proven assertions and reachable witness covers."
                ),
                None => "\n5. If a `fvp.spec.json` (`informal.spec/1`) specification exists or is requested, run `spec_audit` to verify requirement-to-assertion/cover completeness.".into(),
            };
            (
                PROMPTS[2].description,
                format!(
                    "Formalize this specification requirement for `{scope}` (top: `{top}`; clocks: {clocks}; reset: {reset}):\n\
                     > {req}\n\n\
                     Boundary contract (DO NOT violate):\n\
                     - **Primary inputs (legal targets for `assume`)**: {inputs}\n\
                     - **DUT outputs (obligations for `assert` — NEVER constrain in `assume`)**: {outputs}\n\n\
                     Engineering checklist:\n\
                     1. Read relevant RTL lines (`design_source` / `signal_nav`) to verify exact signal names, widths, active polarity, and pipeline latency.\n\
                     2. Run `formal_lint` if not yet checked to verify reset/clock hygiene.\n\
                     3. Draft each SVA property with:\n\
                        - Explicit reset guard (`disable iff ({reset})`) when checking post-reset behavior.\n\
                        - Exact sampling semantics (`|->` same-cycle vs `|=>` next-cycle; guard `$past(...)` against cycle 0).\n\
                        - A **companion non-vacuity witness `cover`** proving the antecedent/scenario is reachable.\n\
                     4. Validate every candidate with `proposal_validate` (confirming non-vacuity and cover reachability) BEFORE calling `proposal_add`.{spec_note}"
                ),
            )
        }
        "audit_signoff" => {
            let sf = arg("spec_file").or_else(|| ctx["spec_file"].as_str());
            let spec_step = match sf {
                Some(p) => format!(
                    "1. **Machine-Verifiable Spec Audit**: Call `spec_audit(spec_file=\"{p}\", check_dead_end=true)` first — report `signoff_ready`, per-`REQ-*` status (`verified`, `cex`, `undetermined`, `missing_assert`, `missing_cover`, `vacuous`, `overconstrained`), boundary over-constraints, and failed gates."
                ),
                None => "1. **Machine-Verifiable Spec Audit**: Call `spec_audit(check_dead_end=true)` if an `fvp.spec.json` (`informal.spec/1`) file is present (or pass inline `spec`), and call `session_summary` + `property_list` to tally `proven`, `cex`, `undetermined`, `covered`, and `unreachable`.".into(),
            };
            (
                PROMPTS[3].description,
                format!(
                    "Perform a rigorous formal verification signoff audit on `{top}` (clocks: {clocks}; reset: {reset}; mode: {mode}).\n\
                     Failing (`cex`) properties: {cex_list}\n\
                     Undetermined properties: {undet_list}\n\n\
                     Audit checklist (report findings with exact evidence, no hand-waving):\n\
                     {spec_step}\n\
                     2. **Lint & Anti-Cheat**: Call `formal_lint` (`SV-FPV-001..005`) — flag any output/state-constraining assumptions or missing `disable iff` reset guards.\n\
                     3. **Vacuity & AI Provenance**: Inspect every `proven` assertion via `property_get` for `vacuity` (`non-vacuous` vs `vacuous` / `not checked`) and `under_ai` dependencies.\n\
                     4. **Assumption Sanity & Covers**: Verify environment assumptions are conflict-free and dead-end-free, and no witness `cover` is unexpectedly `unreachable`.\n\
                     5. **Formal Coverage**: Check COI/UNR coverage via `check_cov` if initialized, and summarize concrete signoff blockers."
                ),
            )
        }
        _ => {
            let names: Vec<&str> = PROMPTS.iter().map(|p| p.name).collect();
            return Err(format!(
                "unknown prompt `{name}` (available prompts: {})",
                names.join(", ")
            ));
        }
    };

    Ok(json!({
        "description": description,
        "messages": [
            {
                "role": "user",
                "content": {
                    "type": "text",
                    "text": text,
                }
            }
        ]
    }))
}
