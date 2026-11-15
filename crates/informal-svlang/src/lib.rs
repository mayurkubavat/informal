//! SystemVerilog front end on svlang (github.com/mayurkubavat/svlang), the
//! pure-Rust IEEE 1800 compiler: svlang elaborates the design and lowers it
//! to a word-level netlist, compiles the concurrent properties written in the
//! RTL into checks, and writes the netlist as Btor2 (`svlang::btor2`).
//!
//! The request and result types are the slang front end's
//! ([`informal_slang::Request`], [`informal_slang::Lowered`]), so the
//! session can use either; svlang is the default (decision D14).

use informal_slang::{
    Design, Diagnostic, Instance, Lowered, Request, RtlProperty, Scope, ScopeSignal, Signal,
};
use svlang::btor2;
use svlang::netlist::{
    LowerConfig, LowerOutcome, Netlist, NetlistDiag, PropertyOptions, PropertyOutcome,
};
use svlang::semantic::PropKind;
use svlang::{ElaboratedDesign, ScopeNode, Severity, SymbolKind};

/// Memories up to this many words become one register per word; larger
/// ones stay Btor2 arrays (which only some engines take).
pub const BLAST_WORDS: usize = 4096;

fn driver(req: &Request) -> svlang::Driver {
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
    d
}

fn show(d: &NetlistDiag) -> String {
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
fn property_options(skip_cycles: u32, clock_skips: &[(String, bool, u32)]) -> PropertyOptions {
    let mut opts = PropertyOptions::default();
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

/// Bind the properties given by command in the top module's scope and
/// compile them into checks named as asked. Each comes back under its name,
/// with why not when it does not bind or uses an operator svlang does not
/// compile yet.
fn command_properties(
    comp: &svlang::Compilation,
    nl: &mut Netlist,
    properties: &[(String, String)],
    opts: &PropertyOptions,
) -> Vec<RtlProperty> {
    if properties.is_empty() {
        return Vec::new();
    }
    let elab = comp.elaborate();
    let top = nl.top.clone();
    let mut arenas = Vec::new();
    let mut bound = Vec::new();
    for (_, text) in properties {
        let mut arena = svlang::ExprArena::new();
        bound.push(
            comp.bind_property_in_scope(&elab.design, &top, text, &mut arena)
                .map_err(|ds| {
                    ds.iter()
                        .map(|d| d.message.clone())
                        .collect::<Vec<_>>()
                        .join("; ")
                }),
        );
        arenas.push(arena);
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
    let mut compiled = nl
        .compile_external_properties(&elab.design, &external, opts)
        .into_iter();
    properties
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
            let liveness = matches!(made, Some(Ok(svlang::netlist::Compiled::Liveness(_))));
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
        .collect()
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

fn rtl_properties(
    nl: &mut Netlist,
    skip_cycles: u32,
    clock_skips: &[(String, bool, u32)],
) -> Vec<RtlProperty> {
    let opts = property_options(skip_cycles, clock_skips);
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

/// Elaborate and lower the design to a Btor2 model: every labelled
/// immediate assertion becomes a `bad` line named by its label, and
/// immediate assumptions become constraints. A concurrent property written
/// in the RTL becomes a `bad` line under its full hierarchical name (a
/// cover with [`Request::cover_targets`]) or a constraint; the ones svlang
/// cannot compile yet are listed in [`Lowered::properties`] with the
/// reason, and an assumption among them is an error. Never falls back:
/// anything not modelled is an error (`fallback` is always false).
pub fn lower(req: &Request) -> Lowered {
    let fail = |errors: Vec<String>| Lowered {
        ok: false,
        fallback: false,
        errors,
        ..Default::default()
    };
    let comp = match driver(req).compile() {
        Ok(c) => c,
        Err(e) => return fail(vec![format!("svlang: {e}")]),
    };
    let config = LowerConfig {
        top: req.top.clone(),
        blast_memories_up_to: Some(BLAST_WORDS),
        cut_points: req.cut_points.clone(),
        black_box_modules: req.black_box_modules.clone(),
        black_box_instances: req.black_box_instances.clone(),
        ..LowerConfig::default()
    };
    let mut nl = match comp.lower_netlist(&config) {
        LowerOutcome::Lowered(nl) => nl,
        LowerOutcome::Unsupported(ds) => {
            return fail(
                ds.iter()
                    .map(|d| format!("not supported yet: {}", show(d)))
                    .collect(),
            )
        }
        LowerOutcome::Error(ds) => return fail(ds.iter().map(show).collect()),
    };
    let mut properties = rtl_properties(&mut nl, req.skip_cycles, &req.clock_skips);
    properties.extend(command_properties(
        &comp,
        &mut nl,
        &req.properties,
        &property_options(req.skip_cycles, &req.clock_skips),
    ));
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
        return fail(lost);
    }
    let mut options = btor2::Options::default();
    options.trace_outputs = req.trace_outputs;
    options.trace_marker = informal_slang::TRACE_MARKER.into();
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
    match btor2::write(&nl, &options) {
        Ok(m) => {
            // The per-property notes below say more than the writer's count.
            let mut notes: Vec<String> = m
                .notes
                .into_iter()
                .filter(|n| !n.contains("compile_properties"))
                .collect();
            for p in &properties {
                if let Some(e) = &p.error {
                    notes.push(format!("{} {} is not checked: {e}", p.kind, p.name));
                }
                if let Some(why) = &p.unsupported {
                    notes.push(format!(
                        "{} {} is not checked: {why} is not supported yet",
                        p.kind, p.name
                    ));
                }
            }
            Lowered {
                ok: true,
                fallback: false,
                errors: Vec::new(),
                notes,
                btor2: m.btor2,
                properties,
                half_cycle: m.steps_per_cycle > 1,
            }
        }
        Err(e) => fail(vec![e.to_string()]),
    }
}

/// Compile and elaborate: diagnostics, top modules, the instance tree with
/// each scope's ports and signals (the app's Design browser), every
/// signal's path and width (property names are checked against them), and
/// the concurrent properties written in the RTL.
pub fn compile(req: &Request) -> Design {
    let comp = match driver(req).compile() {
        Ok(c) => c,
        Err(e) => {
            return Design {
                ok: false,
                diagnostics: vec![Diagnostic {
                    severity: "error".into(),
                    message: format!("svlang: {e}"),
                    file: None,
                    line: None,
                    column: None,
                }],
                ..Default::default()
            }
        }
    };
    let (diags, elab) = comp.check();
    let diagnostics: Vec<Diagnostic> = diags
        .iter()
        .map(|d| Diagnostic {
            severity: match d.severity {
                Severity::Error => "error",
                _ => "warning",
            }
            .into(),
            message: d.message.clone(),
            file: (!d.file.is_empty()).then(|| d.file.clone()),
            line: (d.line > 0).then_some(d.line),
            column: (d.column > 0).then_some(d.column),
        })
        .collect();
    let ok = !diagnostics.iter().any(|d| d.severity == "error");
    // A design that does not lower lists none here; prove reports why.
    let properties = match ok {
        true => {
            let config = LowerConfig {
                top: req.top.clone(),
                ..LowerConfig::default()
            };
            match comp.lower_netlist(&config) {
                LowerOutcome::Lowered(mut nl) => rtl_properties(&mut nl, 0, &[]),
                _ => Vec::new(),
            }
        }
        false => Vec::new(),
    };
    let e = &elab.design;
    Design {
        ok,
        diagnostics,
        tops: e.tops.iter().map(|&t| e.instance(t).name.clone()).collect(),
        instances: e
            .instances
            .iter()
            .filter(|i| !i.is_interface || i.parent.is_some())
            .map(|i| Instance {
                path: i.path.clone(),
                module: i.definition.clone(),
            })
            .collect(),
        signals: signals(e),
        hierarchy: e.scope_tree().iter().map(scope).collect(),
        properties,
    }
}

fn signals(e: &ElaboratedDesign) -> Vec<Signal> {
    let mut out: Vec<Signal> = e
        .symbols
        .iter()
        .map(|s| Signal {
            path: s.full_path.clone(),
            width: s.ty.bit_width().unwrap_or(0) as u64,
            constant: matches!(s.kind, SymbolKind::Param { .. } | SymbolKind::Genvar),
        })
        .collect();
    for i in &e.instances {
        for (name, _, _, ty) in &i.enum_consts {
            out.push(Signal {
                path: format!("{}.{name}", i.path),
                width: ty.bit_width().unwrap_or(0) as u64,
                constant: true,
            });
        }
    }
    out
}

/// One instance of the Design browser. Generate scopes are folded into the
/// instance that holds them, their signals named `g[0].x` (as slang shows
/// them); child instances inside them stay children.
fn scope(n: &ScopeNode) -> Scope {
    let mut out = Scope {
        path: n.path.clone(),
        name: n.name.clone(),
        module: n.module.clone(),
        signals: Vec::new(),
        children: Vec::new(),
    };
    fold(n, "", &mut out);
    // Ports first, in declaration order, then the rest as declared.
    out.signals.sort_by_key(|s| s.kind != "port");
    out
}

fn fold(n: &ScopeNode, prefix: &str, out: &mut Scope) {
    for s in &n.signals {
        let kind = match s.kind {
            "port" => "port",
            "net" => "net",
            "param" | "genvar" => "parameter",
            _ => "variable",
        };
        out.signals.push(ScopeSignal {
            name: format!("{prefix}{}", s.name),
            width: s.width as u64,
            kind: kind.into(),
            dir: s.dir.map(|d| d.as_str().to_string()),
        });
    }
    for c in &n.children {
        if c.module == "$generate" {
            fold(c, &format!("{prefix}{}.", c.name), out);
        } else {
            out.children.push(scope(c));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::property_text;

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
