//! SystemVerilog front end on svlang (github.com/mayurkubavat/svlang), the
//! pure-Rust IEEE 1800 compiler: svlang elaborates the design and lowers it
//! to a word-level netlist, compiles the concurrent properties written in the
//! RTL into checks, and writes the netlist as Btor2 (`svlang::btor2`).
//!
//! svlang is the only front end (decision D14): the properties given by
//! command and the reset are bound and compiled here too, so no source file
//! is copied or edited.

pub mod assist;
pub mod cache;
mod compile_lower;
pub mod gate_fast;
pub mod ral;
pub mod struct_graph;
mod types;

pub use cache::{
    cache_stats, compile_cached, design_from_compiled, lower_from_compiled, CacheStats,
    CompileFingerprint, CompiledDesign, FileDigest, LowerKey,
};
pub use gate_fast::{
    elaborate_fast_cached, should_use_fast_path, try_elaborate_fast_cached, FastGateDesign,
};
pub(crate) use compile_lower::{
    design_stats, driver, hint, immediate_checks, model, rtl_properties, show,
};
pub use struct_graph::{
    BitRange, CellId, CellNode, CrossingEdge, CrossingGroup, CtrlCone, CtrlRoot, DriverEdge,
    DriverOrigin, EdgeId, ExprOpSummary, FeedthroughKind, GroupId, InlineClampInfo, LoadEdge,
    NetSegId, NetSegKind, NetSegment, PinConnBinding, PinId, PinNode, Polarity, SeqElement,
    StructuralGraph,
};
pub use svlang::semantic::PortDir;
use svlang::{ElaboratedDesign, ScopeNode, SymbolKind};
pub use types::{
    split_trace, ClockInfo, Design, DesignStats, Diagnostic, Instance, Lowered, MemoryInfo,
    Request, ResetInfo, RtlProperty, Scope, ScopeSignal, Signal, TickReset, TRACE_MARKER,
};

/// Memories up to this many words become one register per word; larger
/// ones stay Btor2 arrays (which only some engines take).
pub const BLAST_WORDS: usize = 4096;

/// Elaborate and lower the design to a Btor2 model. Every labelled
/// immediate assertion becomes a `bad` line named by its label, and
/// immediate assumptions become constraints. A concurrent property, written
/// in the RTL or given in [`Request::properties`], becomes a `bad` line
/// under its name (a cover with [`Request::cover_targets`], a liveness
/// property a `justice` line) or a constraint; the ones svlang cannot
/// compile are listed in [`Lowered::properties`] with the reason, and an
/// assumption among them is an error. The reset of [`Request::reset`] is
/// part of the model. Nothing is guessed: anything not modelled is an
/// error.
pub fn lower(req: &Request) -> Lowered {
    match cache::compile_cached(None, req) {
        Ok(compiled) => cache::lower_from_compiled(&compiled, req),
        Err(e) => Lowered {
            ok: false,
            errors: vec![e],
            ..Default::default()
        },
    }
}

/// Compile and elaborate: diagnostics, top modules, the instance tree with
/// each scope's ports and signals (the app's Design browser), every
/// signal's path and width (property names are checked against them), and
/// the concurrent properties written in the RTL.
pub fn compile(req: &Request) -> Design {
    match cache::compile_cached(None, req) {
        Ok(compiled) => cache::design_from_compiled(&compiled, req),
        Err(e) => Design {
            ok: false,
            diagnostics: vec![Diagnostic {
                severity: "error".into(),
                message: e,
                file: None,
                line: None,
                column: None,
            }],
            ..Default::default()
        },
    }
}

pub(crate) fn signals(e: &ElaboratedDesign) -> Vec<Signal> {
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
        for (_, name, _, ty) in &i.enum_consts {
            out.push(Signal {
                path: format!("{}.{name}", i.path),
                width: ty.bit_width().unwrap_or(0) as u64,
                constant: true,
            });
        }
    }
    out
}

fn item_decl_name(t: &svlang::Tree, node: svlang::NodeId) -> Option<String> {
    let n = t.child(node, svlang::SyntaxKind::Name)?;
    let last = *t.token_range(n).last()?;
    let raw = &*t.tokens[last as usize].text;
    Some(raw.strip_prefix('\\').unwrap_or(raw).to_string())
}

fn pkg_exported_names<'a>(
    e: &'a ElaboratedDesign,
    pkg_name: &'a str,
    visited: &mut std::collections::HashSet<&'a str>,
    out: &mut Vec<String>,
) {
    if !visited.insert(pkg_name) {
        return;
    }
    let Some(p) = (pkg_name == "$unit")
        .then_some(&e.unit_scope)
        .or_else(|| e.packages.get(pkg_name))
    else {
        return;
    };
    for (n, _, _, _) in &p.params {
        out.push(n.clone());
    }
    for (n, _, _) in &p.enum_consts {
        out.push(n.clone());
    }
    for (n, _) in &p.typedefs {
        out.push(n.clone());
    }
    for (n, _, _, _, _) in &p.subroutines {
        out.push(n.clone());
    }
    for (n, _, _, _) in &p.array_params {
        out.push(n.clone());
    }
    for (imp_pkg, imp_item) in &p.imports {
        let mut sub = Vec::new();
        pkg_exported_names(e, imp_pkg, visited, &mut sub);
        for n in sub {
            if imp_item.as_deref().is_none_or(|x| x == n) {
                out.push(n);
            }
        }
    }
}

pub(crate) fn collect_scope_names(trees: &[svlang::Tree], e: &ElaboratedDesign) -> Vec<String> {
    use svlang::ElabItemKind;
    let mut out = Vec::new();
    for pkg_name in e.packages.keys() {
        out.push(format!("$pkg:{pkg_name}"));
        let mut names = Vec::new();
        let mut vis = std::collections::HashSet::new();
        pkg_exported_names(e, pkg_name, &mut vis, &mut names);
        for n in names {
            out.push(format!("$pkg:{pkg_name}::{n}"));
        }
    }
    let mut unit_names = Vec::new();
    {
        let mut vis = std::collections::HashSet::new();
        pkg_exported_names(e, "$unit", &mut vis, &mut unit_names);
    }
    for n in &unit_names {
        out.push(format!("$pkg:$unit::{n}"));
    }
    for i in &e.instances {
        for (n, _) in &i.params {
            out.push(format!("{}.{n}", i.path));
        }
        for (n, _) in &i.type_params {
            out.push(format!("{}.{n}", i.path));
        }
        for (_, n, _) in &i.typedefs {
            out.push(format!("{}.{n}", i.path));
        }
        for (_, n, _, _) in &i.enum_consts {
            out.push(format!("{}.{n}", i.path));
        }
        for item in &i.items {
            if matches!(
                item.kind,
                ElabItemKind::FunctionDecl
                    | ElabItemKind::TaskDecl
                    | ElabItemKind::LetDecl
                    | ElabItemKind::SequenceDecl
                    | ElabItemKind::PropertyDecl
                    | ElabItemKind::ClockingDecl
            ) {
                if let Some(t) = trees.get(item.tree) {
                    if let Some(n) = item_decl_name(t, item.node) {
                        out.push(format!("{}.{n}", i.path));
                    }
                }
            }
        }
        for (_, imp_pkg, imp_item) in &i.imports {
            let mut names = Vec::new();
            let mut vis = std::collections::HashSet::new();
            pkg_exported_names(e, imp_pkg, &mut vis, &mut names);
            for n in names {
                if imp_item.as_deref().is_none_or(|x| x == n) {
                    out.push(format!("{}.{n}", i.path));
                }
            }
        }
        for n in &unit_names {
            out.push(format!("{}.{n}", i.path));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// One instance of the Design browser. Generate scopes are folded into the
/// instance that holds them, their signals named `g[0].x` (as slang shows
/// them); child instances inside them stay children.
pub(crate) fn scope(n: &ScopeNode) -> Scope {
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
