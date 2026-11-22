//! Self-contained SystemVerilog boolean / bit-vector expression lexer, AST,
//! deterministic 2-pass IEEE 1800-2023 §11.6 width typer, and Tseitin CNF bitblaster.

pub mod ast;
pub mod bitblast;
pub mod lexer;
pub mod width_ieee1800;

use std::collections::BTreeMap;

pub use ast::{
    collect_logic_expr_signal_refs, collect_logic_expr_signals, parse_logic_expr,
    parse_logic_expr_in_scope, CmpOp, LogicConstraintAst, MAX_LITERAL_WIDTH,
};
pub use bitblast::{compile_logic_ast, compile_typed_bv, compile_typed_logic_ast, CnfEmitter};
pub(crate) use lexer::normalize_logic_expr_sep;
pub use lexer::{qualify_logic_expr_in_scope, qualify_logic_expr_to_root};
pub use width_ieee1800::{
    derive_standalone_signal_widths, typecheck_ast, typecheck_asts, TypedExpr, TypedExprKind,
};

/// External RTL signal width provider for IEEE 1800-2023 §11.6 expression typing.
///
/// In Stages 2–4 and Formal LPV, implementations query elaborated RTL widths.
/// In standalone UPF-only Stage 1, [`NullWidthProvider`] returns `None`, causing
/// [`derive_standalone_signal_widths`] to compute deterministic standalone widths.
pub trait SignalWidthProvider {
    fn width_of(&self, sig: &str) -> Option<u32>;
}

/// Standalone UPF-only width provider returning `None` for all signals.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullWidthProvider;

impl SignalWidthProvider for NullWidthProvider {
    fn width_of(&self, _sig: &str) -> Option<u32> {
        None
    }
}

/// Map-backed [`SignalWidthProvider`] for explicit RTL signal widths and tests.
#[derive(Debug, Clone, Default)]
pub struct MapWidthProvider(pub BTreeMap<String, u32>);

impl MapWidthProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_map(map: BTreeMap<String, u32>) -> Self {
        Self(map)
    }

    pub fn insert(&mut self, sig: impl Into<String>, width: u32) {
        self.0.insert(sig.into(), width);
    }
}

impl SignalWidthProvider for MapWidthProvider {
    fn width_of(&self, sig: &str) -> Option<u32> {
        self.0
            .get(sig)
            .or_else(|| self.0.get(sig.trim_start_matches('/')))
            .copied()
    }
}

pub(crate) fn query_provider_width(provider: &dyn SignalWidthProvider, name: &str) -> Option<u32> {
    let stripped = name.trim_start_matches('/');
    provider
        .width_of(name)
        .or_else(|| {
            if stripped != name {
                provider.width_of(stripped)
            } else {
                None
            }
        })
        .or_else(|| {
            if !name.starts_with('/') {
                provider.width_of(&format!("/{name}"))
            } else {
                None
            }
        })
        .map(|w| w.clamp(1, MAX_LITERAL_WIDTH))
}
