//! Deterministic ACI diagnostic helpers: fuzzy candidate matching for
//! Poka-Yoke error recovery, signal role classification, static assumption
//! hygiene checks (`SV-FPV-001`), RTL source snippet extraction, and
//! deterministic CEX triage & proof-convergence recommendations.

use informal_fpv::{Property, Status};
use informal_svlang::Scope;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Simple Levenshtein edit distance (ASCII/UTF-8 char-level) for short names.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Top `limit` closest strings in `pool` to `query` (substring/suffix matches
/// ranked first, then small Levenshtein distance).
pub fn closest_matches<'a>(
    query: &str,
    pool: impl IntoIterator<Item = &'a str>,
    limit: usize,
) -> Vec<String> {
    if query.trim().is_empty() || limit == 0 {
        return Vec::new();
    }
    let q = query.to_ascii_lowercase();
    let q_leaf = q.rsplit(['.', ':']).next().unwrap_or(&q);
    let mut scored: Vec<(usize, usize, String)> = Vec::new();
    let mut seen = BTreeSet::new();
    for item in pool {
        if !seen.insert(item) {
            continue;
        }
        let lower = item.to_ascii_lowercase();
        let leaf = lower.rsplit(['.', ':']).next().unwrap_or(&lower);
        let max_dist = (q.len().max(lower.len()) / 2).clamp(2, 6);
        let score = if lower == q {
            Some((0, 0))
        } else if leaf == q_leaf && !q_leaf.is_empty() {
            Some((0, 1))
        } else if lower.contains(&q) || q.contains(&lower) {
            Some((1, lower.len().abs_diff(q.len())))
        } else {
            let d_full = edit_distance(&q, &lower);
            let d_leaf = edit_distance(q_leaf, leaf);
            let d = d_full.min(d_leaf);
            (d <= max_dist).then_some((2, d))
        };
        if let Some((tier, dist)) = score {
            scored.push((tier, dist, item.to_string()));
        }
    }
    scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    scored.into_iter().take(limit).map(|(_, _, s)| s).collect()
}

/// Flattened hierarchical signal paths from the elaborated design scopes.
pub fn all_signal_paths(scopes: &[Scope]) -> Vec<String> {
    fn walk(sc: &Scope, out: &mut Vec<String>) {
        for sig in &sc.signals {
            if sig.kind != "parameter" {
                out.push(format!("{}.{}", sc.path, sig.name));
            }
        }
        for ch in &sc.children {
            walk(ch, out);
        }
    }
    let mut out = Vec::new();
    for sc in scopes {
        walk(sc, &mut out);
    }
    out
}

/// Top-scope port summary split cleanly by direction (`inputs`, `outputs`, `inouts`).
pub fn top_ports(scopes: &[Scope]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let (mut ins, mut outs, mut inouts) = (Vec::new(), Vec::new(), Vec::new());
    let Some(top) = scopes.first() else {
        return (ins, outs, inouts);
    };
    for sig in &top.signals {
        if sig.kind != "port" {
            continue;
        }
        let label = if sig.width > 1 {
            format!("{}[{}:0]", sig.name, sig.width - 1)
        } else {
            sig.name.clone()
        };
        match sig.dir.as_deref() {
            Some("input") => ins.push(label),
            Some("output") => outs.push(label),
            _ => inouts.push(label),
        }
    }
    (ins, outs, inouts)
}

/// Classify a signal (hierarchical path or top-scope leaf name) relative to
/// the top module boundary: `primary_input`, `dut_output`, `inout`,
/// `internal_state`, or `unknown`.
pub fn classify_signal_role(signal: &str, scopes: &[Scope]) -> &'static str {
    let Some(top) = scopes.first() else {
        return "unknown";
    };
    let rel = signal
        .strip_prefix(&format!("{}.", top.path))
        .unwrap_or(signal);
    if !rel.contains('.') {
        if let Some(sig) = top.signals.iter().find(|s| s.name == rel) {
            return match (sig.kind.as_str(), sig.dir.as_deref()) {
                ("port", Some("input")) => "primary_input",
                ("port", Some("output")) => "dut_output",
                ("port", Some("inout" | "ref")) => "inout",
                ("variable" | "net", _) => "internal_state",
                _ => "unknown",
            };
        }
    }
    fn find_nested(sc: &Scope, full: &str) -> Option<&'static str> {
        for sig in &sc.signals {
            if format!("{}.{}", sc.path, sig.name) == full {
                return Some("internal_state");
            }
        }
        for ch in &sc.children {
            if let Some(r) = find_nested(ch, full) {
                return Some(r);
            }
        }
        None
    }
    find_nested(top, signal).unwrap_or("unknown")
}

/// Extract SV identifiers (including hierarchical `a.b`) from an SVA expression,
/// stripping system calls (`$past`) and SV/SVA keywords.
pub fn expr_identifiers(expr: &str) -> Vec<String> {
    const KEYWORDS: &[&str] = &[
        "posedge",
        "negedge",
        "edge",
        "disable",
        "iff",
        "property",
        "sequence",
        "assert",
        "assume",
        "cover",
        "not",
        "and",
        "or",
        "until",
        "s_until",
        "always",
        "s_always",
        "eventually",
        "s_eventually",
        "nexttime",
        "s_nexttime",
        "throughout",
        "within",
        "intersect",
        "first_match",
        "if",
        "else",
        "inside",
    ];
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let bytes = expr.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'$' {
            // Skip system function name like `$past`, `$rose`, `$stable`.
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            continue;
        }
        if b.is_ascii_digit() {
            // Skip sized literals like `4'd9`, `8'hff`.
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'\'' || bytes[i] == b'_')
            {
                i += 1;
            }
            continue;
        }
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.')
            {
                i += 1;
            }
            let tok = &expr[start..i];
            if !KEYWORDS.contains(&tok) && seen.insert(tok.to_string()) {
                out.push(tok.to_string());
            }
            continue;
        }
        i += 1;
    }
    out
}

/// Static anti-cheat check for `assume` expressions (`SV-FPV-001` equivalent):
/// returns warnings if the assumption references DUT outputs or internal state
/// rather than only primary inputs.
pub fn assume_overconstraint_warnings(expr: &str, scopes: &[Scope]) -> Vec<String> {
    if scopes.is_empty() {
        return Vec::new();
    }
    let mut outputs = Vec::new();
    let mut internals = Vec::new();
    for id in expr_identifiers(expr) {
        match classify_signal_role(&id, scopes) {
            "dut_output" => outputs.push(id),
            "internal_state" => internals.push(id),
            _ => {}
        }
    }
    let mut warnings = Vec::new();
    if !outputs.is_empty() {
        warnings.push(format!(
            "SV-FPV-001 risk: assumption references DUT output port(s) [{}]. Environment assumptions should constrain primary inputs only; if this is an internal invariant, prove it as an `assert` first and promote via `assume -from_assert`.",
            outputs.join(", ")
        ));
    }
    if !internals.is_empty() {
        warnings.push(format!(
            "Over-constraint risk: assumption references internal DUT signal(s) [{}]. Verify this models a genuine interface precondition rather than masking an RTL bug, and confirm companion covers remain reachable.",
            internals.join(", ")
        ));
    }
    warnings
}

/// Read `±radius` lines around 1-based `line` from `file` (best-effort).
pub fn read_snippet(file: &str, line: usize, radius: usize) -> Option<Value> {
    let text = std::fs::read_to_string(file).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() || line == 0 {
        return None;
    }
    let first = line.saturating_sub(radius).max(1);
    let last = (line + radius).min(lines.len());
    let snippet: Vec<String> = (first..=last)
        .map(|ln| {
            let marker = if ln == line { ">" } else { " " };
            format!("{marker}{ln:>4}: {}", lines[ln - 1])
        })
        .collect();
    Some(json!({
        "file": file,
        "line": line,
        "first_line": first,
        "last_line": last,
        "snippet": snippet.join("\n"),
    }))
}

/// Deterministic CEX triage hint summarizing root-cause signal controllability
/// and lint diagnostics.
pub fn cex_triage_hint(
    prop: &Property,
    active_signals: &[String],
    signal_roles: &BTreeMap<String, String>,
    lint_rules: &[String],
) -> String {
    let mut hints = Vec::new();
    if lint_rules.iter().any(|r| r == "SV-FPV-003") {
        hints.push("SV-FPV-003 (missing `disable iff` reset guard) detected in RTL; check whether the failure fires during or immediately after reset.".to_string());
    }
    let primary_inputs: Vec<&str> = active_signals
        .iter()
        .filter(|s| signal_roles.get(*s).map(String::as_str) == Some("primary_input"))
        .map(String::as_str)
        .collect();
    let dut_state: Vec<&str> = active_signals
        .iter()
        .filter(|s| {
            matches!(
                signal_roles.get(*s).map(String::as_str),
                Some("dut_output" | "internal_state")
            )
        })
        .map(String::as_str)
        .collect();
    if prop.kind == informal_fpv::Kind::Cover {
        hints.push("This is a cover witness trace showing a reachable execution; confirm the sequence matches legal protocol behavior.".to_string());
    } else if !primary_inputs.is_empty() {
        hints.push(format!(
            "Active causal inputs: [{}]; DUT state/outputs: [{}]. Decide whether the driving input values are illegal environment stimulus (validate a minimal primary-input `assume` with `proposal_validate`) or a genuine RTL bug in the driver snippets.",
            primary_inputs.join(", "),
            dut_state.join(", ")
        ));
    } else if !dut_state.is_empty() {
        hints.push(format!(
            "Causal slice involves DUT state/outputs [{}] with no unconstrained primary inputs — strong indicator of an RTL bug or property mismatch rather than a missing input assumption.",
            dut_state.join(", ")
        ));
    } else {
        hints.push("Inspect the active causal events from failure frame backward to cycle 0 and cross-check against the specification.".to_string());
    }
    hints.join(" ")
}

/// Deterministic proof-convergence tactic recommendations for an `undetermined`
/// or slow property.
pub fn convergence_recommendations(
    prop: &Property,
    registers: usize,
    inputs: usize,
    complexity: &Value,
    assumptions_count: usize,
    cut_points_count: usize,
) -> Vec<String> {
    let mut recs = Vec::new();
    match &prop.status {
        Status::Proven { .. } | Status::Unreachable { .. } => {
            recs.push(
                "Property is already proved; run vacuity/cover checks and `check_cov` for signoff."
                    .into(),
            );
            return recs;
        }
        Status::Cex { .. } => {
            recs.push("Property has a counterexample (`cex`); call `cex_diagnose` instead of convergence tuning.".into());
            return recs;
        }
        Status::Error { message } => {
            recs.push(format!(
                "Resolve lowering/elaboration error first: {message}"
            ));
            return recs;
        }
        _ => {}
    }

    let max_bound = prop.stats.as_ref().and_then(|s| s.max_bound).unwrap_or(0);
    let hotspots = complexity
        .pointer("/hotspots")
        .or_else(|| complexity.pointer("/arithmetic_hotspots"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let memories = complexity
        .pointer("/memories")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let bbox_candidates = complexity
        .pointer("/blackbox_candidates")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);

    if hotspots > 0 {
        recs.push(format!(
            "Complexity report flags {hotspots} wide arithmetic hotspot(s): isolate datapath values via a `cut_point` (`stopat`) or `black_box` and validate with `proposal_validate(kind=\"cut_point\")`."
        ));
    }
    if memories > 0 {
        recs.push(format!(
            "Cone contains {memories} memory array(s): reduce memory depth via parameter override (`-param`) or abstract unchanged memory entries with a cutpoint/symbolic index."
        ));
    }
    if bbox_candidates > 0 {
        recs.push(format!(
            "{bbox_candidates} blackbox candidate(s) identified in COI: abstract non-critical submodules or replace with lightweight interface assumptions."
        ));
    }
    if max_bound >= 15 {
        recs.push(format!(
            "BMC reached depth k={max_bound} with no CEX while unbounded engines stalled: classic inductive-weakness signature. Mine a helper invariant (e.g. pointer/count/FSM state relationship) and test via `proposal_validate(kind=\"assert\", targets=[\"{}\"])`.",
            prop.name
        ));
    } else if registers >= 64 {
        recs.push(format!(
            "Large sequential COI ({registers} registers, {inputs} inputs): decompose with case-splitting `task`s or abstract independent datapath registers via `cut_point`."
        ));
    } else {
        recs.push(format!(
            "Try increasing prove `time_limit` or mining an inductive helper invariant via `proposal_validate(kind=\"assert\", targets=[\"{}\"])`.",
            prop.name
        ));
    }
    if assumptions_count == 0 && inputs > 2 {
        recs.push("Zero environment assumptions are active; verify protocol inputs don't need legal handshake constraints (check with `formal_lint` and `proposal_validate`).".into());
    }
    if cut_points_count > 0 {
        recs.push(format!(
            "{cut_points_count} cut point(s) are already active in this task; ensure any proof is sound without them or discharged via assume-guarantee."
        ));
    }
    recs
}

#[cfg(test)]
mod tests {
    use super::*;
    use informal_svlang::ScopeSignal;

    fn sample_scope() -> Vec<Scope> {
        vec![Scope {
            path: "counter".into(),
            name: "counter".into(),
            module: "counter".into(),
            signals: vec![
                ScopeSignal {
                    name: "clk".into(),
                    width: 1,
                    kind: "port".into(),
                    dir: Some("input".into()),
                },
                ScopeSignal {
                    name: "en".into(),
                    width: 1,
                    kind: "port".into(),
                    dir: Some("input".into()),
                },
                ScopeSignal {
                    name: "q".into(),
                    width: 4,
                    kind: "port".into(),
                    dir: Some("output".into()),
                },
                ScopeSignal {
                    name: "cnt_int".into(),
                    width: 4,
                    kind: "variable".into(),
                    dir: None,
                },
            ],
            children: vec![],
        }]
    }

    #[test]
    fn fuzzy_matcher_ranks_typos_and_suffixes() {
        let pool = ["a_never9", "a_max", "a_step", "c_done"];
        let hits = closest_matches("a_nevr9", pool, 3);
        assert_eq!(hits.first().map(String::as_str), Some("a_never9"));
        let leaf = closest_matches("q", ["counter.clk", "counter.en", "counter.q"], 2);
        assert_eq!(leaf.first().map(String::as_str), Some("counter.q"));
    }

    #[test]
    fn assume_hygiene_flags_outputs_and_internals_but_allows_inputs() {
        let scopes = sample_scope();
        assert!(assume_overconstraint_warnings("en |=> en", &scopes).is_empty());
        let out_warn = assume_overconstraint_warnings("q <= 4'd8", &scopes);
        assert!(out_warn
            .iter()
            .any(|w| w.contains("SV-FPV-001") && w.contains("q")));
        let int_warn = assume_overconstraint_warnings("cnt_int != 4'd9", &scopes);
        assert!(int_warn.iter().any(|w| w.contains("cnt_int")));
    }
}
