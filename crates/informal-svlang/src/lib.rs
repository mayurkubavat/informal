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
fn rtl_properties(nl: &mut Netlist, skip_cycles: u32) -> Vec<RtlProperty> {
    let mut opts = PropertyOptions::default();
    opts.skip_cycles = skip_cycles;
    let outcomes = nl.compile_properties(&opts);
    nl.properties
        .iter()
        .zip(&nl.property_names)
        .zip(outcomes)
        .map(|((p, name), outcome)| RtlProperty {
            name: name.clone(),
            kind: match p.kind {
                PropKind::Assert => "assert",
                PropKind::Assume | PropKind::Restrict => "assume",
                PropKind::Cover => "cover",
            }
            .into(),
            unsupported: match outcome {
                PropertyOutcome::Compiled { .. } => None,
                PropertyOutcome::Unsupported(why) => Some(why),
            },
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
    let properties = rtl_properties(&mut nl, req.skip_cycles);
    // Leaving an assumption out would make the model weaker than the design says.
    let lost: Vec<String> = properties
        .iter()
        .filter(|p| p.kind == "assume")
        .filter_map(|p| {
            let why = p.unsupported.as_ref()?;
            Some(format!(
                "not supported yet: assumption {} uses {why}",
                p.name
            ))
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
    match btor2::write(&nl, &options) {
        Ok(m) => {
            // The per-property notes below say more than the writer's count.
            let mut notes: Vec<String> = m
                .notes
                .into_iter()
                .filter(|n| !n.contains("compile_properties"))
                .collect();
            for p in &properties {
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
                LowerOutcome::Lowered(mut nl) => rtl_properties(&mut nl, 0),
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
