//! Design compilation, reset/property lowering, and Btor2 model construction.

use crate::types::{ClockInfo, DesignStats, MemoryInfo, ResetInfo, RtlProperty, TRACE_MARKER};
use crate::Request;
use svlang::btor2;
use svlang::netlist::{Netlist, NetlistDiag, PropertyOptions, PropertyOutcome};
use svlang::semantic::PropKind;

pub(crate) fn driver(req: &Request) -> svlang::Driver {
    let mut d = svlang::Driver::new();
    for (k, v) in &req.defines {
        d = d.define(k.clone(), v.clone());
    }
    for i in &req.incdirs {
        d = d.include_dir(i.clone());
    }
    if let Some(t) = &req.top {
        d = d.top(t.clone());
    }
    for (k, v) in &req.params {
        d = d.param(k.clone(), v.clone());
    }
    for f in &req.files {
        d = d.add_file(f.clone());
    }
    if !req.black_box_modules.is_empty() {
        d = d.black_box_modules(req.black_box_modules.clone());
    }
    d
}

/// For the two constructs a user may choose to have left out: how.
pub(crate) fn hint(message: &str) -> &'static str {
    if message.contains("$isunknown") {
        " (`set_lowering isunknown_is_false on` lowers $isunknown as 0)"
    } else if message.contains("immediate assertion on a non-constant value inside initial block") {
        " (`set_lowering skip_initial_checks on` leaves such checks out)"
    } else {
        ""
    }
}

pub(crate) fn show(d: &NetlistDiag) -> String {
    if d.file.is_empty() {
        d.message.clone()
    } else {
        format!("{}:{}: {}", d.file, d.loc.line, d.message)
    }
}

/// Compile the concurrent properties written in the RTL into checks, and
/// list them: each with its full hierarchical name, and why not when it
/// uses an operator svlang does not compile yet.
/// How properties are compiled: the cycles in reset whose attempts are left
/// out, for every clock and for one clock and edge.
fn property_options(
    skip_cycles: u32,
    clock_skips: &[(String, bool, u32)],
    preconditions: bool,
    witnesses: bool,
) -> PropertyOptions {
    let mut opts = PropertyOptions::default();
    opts.precondition_covers = preconditions;
    opts.witness_covers = witnesses;
    opts.skip_cycles = skip_cycles;
    opts.skip_by_clock = clock_skips
        .iter()
        .map(|(input, posedge, ticks)| svlang::netlist::ClockSkip {
            input: input.clone(),
            posedge: *posedge,
            ticks: *ticks,
        })
        .collect();
    opts
}

fn antecedent_cover_expr(expr: &svlang::PropExpr) -> Option<svlang::PropExpr> {
    match expr {
        svlang::PropExpr::OverlappedImplication { lhs, .. }
        | svlang::PropExpr::NonOverlappedImplication { lhs, .. } => {
            Some(svlang::PropExpr::Seq(lhs.clone()))
        }
        svlang::PropExpr::Clocked { clock, prop } => Some(svlang::PropExpr::Clocked {
            clock: clock.clone(),
            prop: Box::new(antecedent_cover_expr(prop)?),
        }),
        _ => None,
    }
}

/// Synthesize a `cover property` over the antecedent sequence of an implication
/// assertion (`lhs |-> rhs` or `lhs |=> rhs`), preserving its clock and
/// `disable iff`. Used when `svlang`'s built-in `related_cover` omits
/// `<name>:precondition` (e.g. liveness `s_eventually` / `until` consequents).
fn precondition_cover_of(p: &svlang::BoundProperty) -> Option<svlang::BoundProperty> {
    if p.kind != PropKind::Assert {
        return None;
    }
    let expr = antecedent_cover_expr(&p.expr)?;
    let mut cov = p.clone();
    cov.kind = PropKind::Cover;
    cov.expr = expr;
    Some(cov)
}

/// Bind the properties given by command in the top module's scope and
/// compile them into checks named as asked. Each comes back under its name,
/// with why not when it does not bind or uses an operator svlang does not
/// compile yet.
fn command_properties(
    comp: &svlang::Compilation,
    design: &svlang::ElaboratedDesign,
    nl: &mut Netlist,
    properties: &[(String, String)],
    opts: &PropertyOptions,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Vec<RtlProperty>, String> {
    if properties.is_empty() {
        return Ok(Vec::new());
    }
    let cancelled = || cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::SeqCst));
    let top = nl.top.clone();
    let mut arenas = Vec::new();
    let mut bound = Vec::new();
    for (_, text) in properties {
        if cancelled() {
            return Err("cancelled".into());
        }
        let mut arena = svlang::ExprArena::new();
        bound.push(
            comp.bind_property_in_scope(design, &top, text, &mut arena)
                .map_err(|ds| {
                    ds.iter()
                        .map(|d| d.message.clone())
                        .collect::<Vec<_>>()
                        .join("; ")
                }),
        );
        arenas.push(arena);
    }
    if cancelled() {
        return Err("cancelled".into());
    }
    let external: Vec<svlang::netlist::ExternalProperty<'_>> = properties
        .iter()
        .zip(arenas.iter().zip(&bound))
        .filter_map(|((name, _), (arena, property))| {
            Some(svlang::netlist::ExternalProperty {
                arena,
                property: property.as_ref().ok()?,
                name: name.clone(),
            })
        })
        .collect();
    let compiled_vec =
        nl.compile_external_properties_with_trees(comp.trees(), design, &external, opts);
    if opts.precondition_covers {
        let existing: std::collections::HashSet<String> =
            nl.checks.iter().filter_map(|c| c.label.clone()).collect();
        let mut cmd_missing: Vec<(&svlang::ExprArena, svlang::BoundProperty, String)> = Vec::new();
        let mut comp_iter = compiled_vec.iter();
        for ((name, _), (arena, b)) in properties.iter().zip(arenas.iter().zip(&bound)) {
            let Ok(p) = b else { continue };
            let Some(Ok(_)) = comp_iter.next() else {
                continue;
            };
            let pre = format!("{name}:precondition");
            if existing.contains(&pre) {
                continue;
            }
            if let Some(cov) = precondition_cover_of(p) {
                cmd_missing.push((arena, cov, pre));
            }
        }
        if !cmd_missing.is_empty() {
            let ext: Vec<svlang::netlist::ExternalProperty<'_>> = cmd_missing
                .iter()
                .map(|(arena, prop, pre)| svlang::netlist::ExternalProperty {
                    arena,
                    property: prop,
                    name: pre.clone(),
                })
                .collect();
            let mut cov_opts = PropertyOptions::default();
            cov_opts.skip_cycles = opts.skip_cycles;
            cov_opts.skip_by_clock = opts.skip_by_clock.clone();
            let _ =
                nl.compile_external_properties_with_trees(comp.trees(), design, &ext, &cov_opts);
        }
    }
    let mut compiled = compiled_vec.into_iter();
    let out = properties
        .iter()
        .zip(&bound)
        .map(|((name, _), b)| {
            let (kind, made, error) = match b {
                Err(e) => ("assert", None, Some(e.clone())),
                Ok(p) => (
                    match p.kind {
                        PropKind::Assert => "assert",
                        PropKind::Assume | PropKind::Restrict => "assume",
                        PropKind::Cover => "cover",
                    },
                    compiled.next(),
                    None,
                ),
            };
            use svlang::netlist::Compiled;
            let liveness = matches!(
                made,
                Some(Ok(Compiled::Liveness(_) | Compiled::CheckAndLiveness(..)))
            );
            let unsupported = made.and_then(Result::err);
            RtlProperty {
                name: name.clone(),
                kind: kind.into(),
                unsupported,
                error,
                liveness,
                text: None,
                file: None,
                line: None,
            }
        })
        .collect::<Vec<_>>();
    Ok(out)
}

/// The property specification that starts at `line` and `column` (both from
/// 1, the column in bytes) of `source`: what stands between the parentheses
/// after `property` (or `sequence`, for `cover sequence`), with runs of
/// white space made one space. `None` when the text there does not look
/// like a concurrent assertion.
fn property_text(source: &str, line: u32, column: u32) -> Option<String> {
    let start: usize = source
        .split_inclusive('\n')
        .take(line.checked_sub(1)? as usize)
        .map(str::len)
        .sum::<usize>()
        + column.checked_sub(1)? as usize;
    let rest = source.get(start..)?;
    // The statement ends at its semicolon; the keyword comes before that.
    let end = rest.find(';').unwrap_or(rest.len());
    let keyword = ["property", "sequence"]
        .iter()
        .filter_map(|k| rest[..end].find(k).map(|i| i + k.len()))
        .min()?;
    let open = keyword + rest[keyword..].find('(')?;
    if !rest[keyword..open].trim().is_empty() {
        return None;
    }
    let mut depth = 0usize;
    for (i, c) in rest[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    let inner = &rest[open + 1..open + i];
                    let one_line = inner.split_whitespace().collect::<Vec<_>>().join(" ");
                    return (!one_line.is_empty()).then_some(one_line);
                }
            }
            _ => {}
        }
    }
    None
}

/// The checked expression of an immediate assertion, cover or severity task
/// starting at `line` and `column` (both from 1, the column in bytes) of
/// `source`, collapsed onto one line.
fn immediate_text(source: &str, line: u32, column: u32) -> Option<String> {
    let start: usize = source
        .split_inclusive('\n')
        .take(line.checked_sub(1)? as usize)
        .map(str::len)
        .sum::<usize>()
        + column.checked_sub(1)? as usize;
    let rest = source.get(start..)?;
    let end = rest.find(';').unwrap_or(rest.len());
    let stmt = &rest[..end];
    if let Some(kw) = ["assert", "assume", "cover"]
        .iter()
        .filter_map(|k| stmt.find(k).map(|i| i + k.len()))
        .min()
    {
        let open = kw + stmt[kw..].find('(')?;
        let between = stmt[kw..open].trim();
        if !between.is_empty() && between != "#0" && between != "final" {
            return None;
        }
        let mut depth = 0usize;
        for (i, c) in stmt[open..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        let inner = &stmt[open + 1..open + i];
                        let one_line = inner.split_whitespace().collect::<Vec<_>>().join(" ");
                        return (!one_line.is_empty()).then_some(one_line);
                    }
                }
                _ => {}
            }
        }
        return None;
    }
    if stmt.trim_start().starts_with("$error") || stmt.trim_start().starts_with("$fatal") {
        let one_line = stmt.split_whitespace().collect::<Vec<_>>().join(" ");
        return (!one_line.is_empty()).then_some(one_line);
    }
    None
}

pub(crate) fn rtl_properties(
    nl: &mut Netlist,
    skip_cycles: u32,
    clock_skips: &[(String, bool, u32)],
    preconditions: bool,
    witnesses: bool,
) -> Vec<RtlProperty> {
    let opts = property_options(skip_cycles, clock_skips, preconditions, witnesses);
    let outcomes = nl.compile_properties(&opts);
    // The source files the properties stand in, read once each.
    let mut sources: std::collections::HashMap<u32, Option<String>> = Default::default();
    let mut text_of = |loc: svlang::semantic::Loc| -> Option<String> {
        // Code from a macro has no place of its own in a file.
        if loc.expansion.is_some() {
            return None;
        }
        let source = sources
            .entry(loc.file)
            .or_insert_with(|| {
                let path = nl.files.get(loc.file as usize)?;
                std::fs::read_to_string(path).ok()
            })
            .as_deref()?;
        property_text(source, loc.line, loc.column)
    };
    nl.properties
        .iter()
        .zip(&nl.property_names)
        .zip(outcomes)
        .map(|((p, name), outcome)| RtlProperty {
            text: text_of(p.loc),
            liveness: matches!(outcome, PropertyOutcome::Liveness { .. }),
            name: name.clone(),
            kind: match p.kind {
                PropKind::Assert => "assert",
                PropKind::Assume | PropKind::Restrict => "assume",
                PropKind::Cover => "cover",
            }
            .into(),
            unsupported: match outcome {
                PropertyOutcome::Compiled { .. } | PropertyOutcome::Liveness { .. } => None,
                PropertyOutcome::Unsupported(why) => Some(why),
            },
            error: None,
            file: nl
                .files
                .get(p.loc.file as usize)
                .filter(|f| !f.is_empty())
                .cloned(),
            line: (p.loc.line > 0).then_some(p.loc.line),
        })
        .collect()
}

/// Adds the immediate assertions, assumptions and covers of `nl` to `listed`
/// under their unique full hierarchical names
/// (`<inst>.[<scope>.]<label_or__kind_n>`). Related vacuity/witness covers
/// (`<name>:precondition`, `<name>:witness`) are skipped.
pub(crate) fn immediate_checks(nl: &Netlist, listed: &mut Vec<RtlProperty>) {
    use svlang::netlist::CheckKind;
    let mut sources: std::collections::HashMap<u32, Option<String>> = Default::default();
    let mut text_of = |loc: svlang::semantic::Loc| -> Option<String> {
        if loc.expansion.is_some() {
            return None;
        }
        let source = sources
            .entry(loc.file)
            .or_insert_with(|| {
                let path = nl.files.get(loc.file as usize)?;
                std::fs::read_to_string(path).ok()
            })
            .as_deref()?;
        immediate_text(source, loc.line, loc.column)
    };
    for c in &nl.checks {
        if !c.immediate {
            continue;
        }
        let Some(name) = c.label.as_deref().filter(|l| !l.contains(':')) else {
            continue;
        };
        let file = nl
            .files
            .get(c.loc.file as usize)
            .filter(|f| !f.is_empty())
            .cloned();
        let line = (c.loc.line > 0).then_some(c.loc.line);
        listed.push(RtlProperty {
            name: name.to_string(),
            kind: match c.kind {
                CheckKind::Assert => "assert",
                CheckKind::Assume => "assume",
                CheckKind::Cover => "cover",
            }
            .into(),
            unsupported: None,
            error: None,
            liveness: false,
            text: text_of(c.loc),
            file,
            line,
        });
    }
}

/// The value of the reset condition `text`, an expression in the top
/// module's scope, in `nl`. Lowering it may re-index the netlist's values,
/// so nothing else may be added to `nl` after this.
fn reset_value(
    comp: &svlang::Compilation,
    design: &svlang::ElaboratedDesign,
    nl: &mut Netlist,
    text: &str,
) -> Result<svlang::netlist::ValueId, String> {
    let said = |ds: Vec<String>| format!("reset {{{text}}}: {}", ds.join("; "));
    let mut arena = svlang::ExprArena::new();
    let top = nl.top.clone();
    let prop = comp
        .bind_property_in_scope(design, &top, &format!("({text})"), &mut arena)
        .map_err(|ds| said(ds.into_iter().map(|d| d.message).collect()))?;
    let svlang::PropExpr::Seq(svlang::SeqExpr::Bool(e)) = prop.expr else {
        return Err(said(vec!["not a boolean expression".into()]));
    };
    let pairs = nl
        .lower_external_property_with_trees(comp.trees(), design, &arena, &prop, false)
        .map_err(|ds| said(ds.into_iter().map(|d| d.message).collect()))?;
    pairs
        .into_iter()
        .find(|(id, _)| *id == e)
        .map(|(_, v)| v)
        .ok_or_else(|| said(vec!["the expression was not lowered".into()]))
}

/// One model of `base`: the properties compiled, the reset applied and the
/// netlist written, with the reset's length and the cycles left out of the
/// properties as given, polling `cancel` between stages.
#[allow(clippy::too_many_arguments)]
pub(crate) fn model_cancel(
    req: &Request,
    comp: &svlang::Compilation,
    design: &svlang::ElaboratedDesign,
    base: &Netlist,
    reset_steps: u32,
    skip_cycles: u32,
    clock_skips: &[(String, bool, u32)],
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<(btor2::Model, Vec<RtlProperty>), Vec<String>> {
    let cancelled = || cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::SeqCst));
    if cancelled() {
        return Err(vec!["cancelled".into()]);
    }
    let mut nl = base.clone();
    if req.branch_covers {
        nl.add_branch_covers(skip_cycles);
    }
    if req.toggle_covers {
        nl.add_toggle_covers(skip_cycles);
    }
    if cancelled() {
        return Err(vec!["cancelled".into()]);
    }
    let mut properties = rtl_properties(
        &mut nl,
        skip_cycles,
        clock_skips,
        req.precondition_covers,
        req.witness_covers,
    );
    if cancelled() {
        return Err(vec!["cancelled".into()]);
    }
    if req.precondition_covers {
        let existing: std::collections::HashSet<String> =
            nl.checks.iter().filter_map(|c| c.label.clone()).collect();
        let rtl_missing: Vec<(String, svlang::BoundProperty)> = nl
            .properties
            .iter()
            .zip(&nl.property_names)
            .zip(&properties)
            .filter_map(|((p, name), listed)| {
                if listed.unsupported.is_some() || listed.error.is_some() {
                    return None;
                }
                let pre = format!("{name}:precondition");
                if existing.contains(&pre) {
                    return None;
                }
                Some((pre, precondition_cover_of(p)?))
            })
            .collect();
        if !rtl_missing.is_empty() {
            let rtl_arena = nl.sva_arena.clone();
            let ext: Vec<svlang::netlist::ExternalProperty<'_>> = rtl_missing
                .iter()
                .map(|(pre, prop)| svlang::netlist::ExternalProperty {
                    arena: &rtl_arena,
                    property: prop,
                    name: pre.clone(),
                })
                .collect();
            let cov_opts = property_options(skip_cycles, clock_skips, false, false);
            let _ =
                nl.compile_external_properties_with_trees(comp.trees(), design, &ext, &cov_opts);
        }
    }
    // The labelled immediate assertions are targets of the model too, under
    // their labels (the ones that cannot be listed were reported at compile).
    immediate_checks(&nl, &mut properties);
    properties.extend(
        command_properties(
            comp,
            design,
            &mut nl,
            &req.properties,
            &property_options(
                skip_cycles,
                clock_skips,
                req.precondition_covers,
                req.witness_covers,
            ),
            cancel,
        )
        .map_err(|e| vec![e])?,
    );
    if cancelled() {
        return Err(vec!["cancelled".into()]);
    }
    // Leaving an assumption out would make the model weaker than the design says.
    let lost: Vec<String> = properties
        .iter()
        .filter(|p| p.kind == "assume")
        .filter_map(|p| match (&p.unsupported, &p.error) {
            (Some(why), _) => Some(format!(
                "not supported yet: assumption {} uses {why}",
                p.name
            )),
            (None, Some(e)) => Some(format!("assumption {}: {e}", p.name)),
            (None, None) => None,
        })
        .collect();
    if !lost.is_empty() {
        return Err(lost);
    }
    let mut options = btor2::Options::default();
    options.trace_outputs = req.trace_outputs;
    options.trace_marker = TRACE_MARKER.into();
    options.name_prefix = "informal".into();
    options.covers = req.cover_targets;
    // A design on both edges of its clock gets two model steps per cycle.
    options.both_edges = true;
    options.clocks = req
        .clocks
        .iter()
        .map(|(input, half_period, phase)| btor2::Clock {
            input: input.clone(),
            half_period: *half_period,
            phase: *phase,
        })
        .collect();
    if let Some(text) = &req.reset {
        let value = reset_value(comp, design, &mut nl, text).map_err(|e| vec![e])?;
        options.reset = Some(btor2::Reset {
            value,
            steps: reset_steps,
        });
    }
    let mut m = btor2::write(&nl, &options).map_err(|e| vec![e.to_string()])?;
    if req.trace_outputs {
        append_alias_trace_outputs(&nl, &mut m.btor2);
    }
    Ok((m, properties))
}

/// Emits `output <nid> <alias>` lines in the trace section for nets/ports in
/// `nl.traces` that alias an `input` or `state` node under a different name
/// (e.g. `assign out_port = reg_q;`), which `svlang::btor2` omits because the
/// underlying value is already a named register or primary input.
fn append_alias_trace_outputs(nl: &Netlist, btor2: &mut String) {
    use std::collections::{HashMap, HashSet};
    use std::fmt::Write as _;
    use svlang::netlist::ValueKind;

    let sym_clean = |s: &str| -> String {
        s.chars()
            .map(|c| if c.is_whitespace() { '_' } else { c })
            .collect()
    };

    let mut max_id = 0u32;
    let mut sym_to_nid: HashMap<String, u32> = HashMap::new();
    let mut emitted: HashSet<String> = HashSet::new();
    for line in btor2.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with(';') {
            continue;
        }
        let mut toks = trimmed.split_whitespace();
        let Some(id_tok) = toks.next() else { continue };
        let Ok(nid) = id_tok.parse::<u32>() else {
            continue;
        };
        max_id = max_id.max(nid);
        let Some(op) = toks.next() else { continue };
        match op {
            "input" | "state" => {
                let _sort = toks.next();
                if let Some(sym) = toks.next() {
                    sym_to_nid.insert(sym.to_string(), nid);
                    emitted.insert(sym.to_string());
                }
            }
            "output" => {
                let _arg = toks.next();
                if let Some(sym) = toks.next() {
                    emitted.insert(sym.to_string());
                }
            }
            _ => {}
        }
    }

    let mut aliases: Vec<(String, u32)> = Vec::new();
    for t in &nl.traces {
        let alias = sym_clean(&t.name);
        if emitted.contains(&alias) {
            continue;
        }
        let canon = match nl.value(t.value).kind {
            ValueKind::Input(i) => Some(sym_clean(&nl.inputs[i].name)),
            ValueKind::StateOut(s) => Some(sym_clean(&nl.states[s.0 as usize].name)),
            _ => None,
        };
        if let Some(c) = canon.filter(|c| c != &alias) {
            if let Some(&nid) = sym_to_nid.get(&c) {
                aliases.push((alias, nid));
            }
        }
    }
    aliases.sort_by(|a, b| a.0.cmp(&b.0));
    aliases.dedup_by(|a, b| a.0 == b.0);
    for (alias, nid) in aliases {
        max_id += 1;
        let _ = writeln!(btor2, "{max_id} output {nid} {alias}");
    }
}

/// Count what `nl` holds (before its properties are compiled, which adds
/// registers of their own).
pub(crate) fn design_stats(nl: &Netlist) -> DesignStats {
    use svlang::netlist::ValueKind;
    let name_of = |id: svlang::netlist::ValueId| -> String {
        let v = nl.value(id);
        match (&v.name, &v.kind) {
            (Some(n), _) => n.clone(),
            (None, ValueKind::Input(i)) => nl.inputs[*i].name.clone(),
            _ => format!("(expression {})", id.0),
        }
    };
    let mut st = DesignStats {
        inputs: nl.inputs.len(),
        input_bits: nl.inputs.iter().map(|p| u64::from(p.width)).sum(),
        outputs: nl.outputs.len(),
        output_bits: nl.outputs.iter().map(|p| u64::from(p.width)).sum(),
        undriven: nl.undriven.len(),
        ..Default::default()
    };
    let clock_use = |st: &mut DesignStats, c: &svlang::netlist::ClockSpec| {
        let signal = name_of(c.signal);
        let edge = c.edge.as_str().to_string();
        match st.clocks.iter_mut().find(|k| k.signal == signal) {
            Some(k) => {
                k.registers += 1;
                if !k.edges.contains(&edge) {
                    k.edges.push(edge);
                }
            }
            None => st.clocks.push(ClockInfo {
                signal,
                edges: vec![edge],
                registers: 1,
            }),
        }
    };
    for r in &nl.states {
        if r.name.contains("._sva_") {
            continue;
        }
        if r.latch_enable.is_some() {
            st.latches += 1;
            st.latch_bits += u64::from(r.width);
        } else {
            st.flops += 1;
            st.flop_bits += u64::from(r.width);
        }
        if let Some(c) = &r.clock {
            clock_use(&mut st, c);
        }
        if let Some(a) = &r.async_reset {
            let signal = name_of(a.signal);
            match st
                .resets
                .iter_mut()
                .find(|k| k.signal == signal && k.active_high == a.active_high)
            {
                Some(k) => k.registers += 1,
                None => st.resets.push(ResetInfo {
                    signal,
                    active_high: a.active_high,
                    registers: 1,
                }),
            }
        }
    }
    // A memory's write ports count as registers on their clocks.
    for w in nl.memories.iter().flat_map(|m| &m.writes) {
        if let Some(c) = &w.clock {
            clock_use(&mut st, c);
        }
    }
    st.memories = nl
        .memories
        .iter()
        .map(|m| MemoryInfo {
            name: m.name.clone(),
            words: u64::from(m.depth),
            width: m.width,
        })
        .collect();
    st
}

#[cfg(test)]
mod tests {
    use super::{immediate_text, property_text};

    #[test]
    fn the_text_of_an_immediate_check_is_extracted() {
        let src = "module m;\n  a_x: assert (a &&\n    b);\n  assert #0 (c == d);\n  assert final (e);\n  cover (f || g);\n  $error(\"bad\");\nendmodule\n";
        assert_eq!(immediate_text(src, 2, 3).as_deref(), Some("a && b"));
        assert_eq!(immediate_text(src, 4, 3).as_deref(), Some("c == d"));
        assert_eq!(immediate_text(src, 5, 3).as_deref(), Some("e"));
        assert_eq!(immediate_text(src, 6, 3).as_deref(), Some("f || g"));
        assert_eq!(
            immediate_text(src, 7, 3).as_deref(),
            Some("$error(\"bad\")")
        );
    }

    #[test]
    fn the_text_of_a_property_is_what_its_parentheses_hold() {
        let src = "module m;\n  a_x: assert property (@(posedge clk) disable iff (rst)\n      req |-> ##[1:3] (gnt && !err));\n  cover sequence (a ##1 b);\n  assign y = f(x);\nendmodule\n";
        assert_eq!(
            property_text(src, 2, 3).as_deref(),
            Some("@(posedge clk) disable iff (rst) req |-> ##[1:3] (gnt && !err)")
        );
        // From the keyword itself, as well as from the label.
        assert_eq!(
            property_text(src, 2, 8).as_deref().map(|t| t.len()),
            Some(62)
        );
        assert_eq!(property_text(src, 4, 3).as_deref(), Some("a ##1 b"));
        // Not an assertion, or no such place.
        assert_eq!(property_text(src, 5, 3), None);
        assert_eq!(property_text(src, 40, 1), None);
        assert_eq!(property_text(src, 0, 0), None);
    }
}
