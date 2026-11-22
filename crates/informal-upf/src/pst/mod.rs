//! Symbolic PST / `add_power_state` SAT solver built on `informal-sat`
//! (`Cadical` / `Mini`), supporting exact `i64` microvolts, empty-supply-set
//! guards, one-way implications, deterministic IEEE 1800-2023 §11.6 bit-blasted
//! `-logic_expr`, `-illegal` blocking clauses, UPF 1.0 Tseitin PST rows, and
//! `set_equivalent` coherence checks.

pub mod cnf;
pub mod encode;
pub mod queries;
pub mod supply_expr;

use std::collections::{BTreeMap, BTreeSet};

use informal_sat::Lit;
use serde::{Deserialize, Serialize};

use crate::expr_bv::{LogicConstraintAst, NullWidthProvider, SignalWidthProvider};
use crate::ir::{SimState, SupplySetId, UpfDiag, VoltageSpec};
use crate::resolve::ResolvedPowerIntent;

use cnf::CnfBuilder;

/// Returns the active SAT solver backend name (`"cadical"` or `"mini"`).
pub fn sat_backend_name() -> &'static str {
    informal_sat::best().name()
}

/// Concrete satisfying assignment witness for a PST / supply-state query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PstWitness {
    /// Chosen state name per `SupplySet` name.
    pub supply_states: BTreeMap<String, String>,
    /// Representative boolean valuations for `-logic_expr` signals.
    pub logic_signals: BTreeMap<String, bool>,
}

/// Level-shifter voltage requirement between two supply sets across legal states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LsRequirement {
    None,
    HighToLow {
        witness: PstWitness,
    },
    LowToHigh {
        witness: PstWitness,
    },
    Both {
        hl_witness: PstWitness,
        lh_witness: PstWitness,
    },
    Undetermined {
        reason: String,
    },
}

impl LsRequirement {
    /// Returns a coarse tag for comparing two `LsRequirement` values regardless of witness details.
    pub fn kind_tag(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::HighToLow { .. } => "high_to_low",
            Self::LowToHigh { .. } => "low_to_high",
            Self::Both { .. } => "both",
            Self::Undetermined { .. } => "undetermined",
        }
    }
}

/// Exported legal/illegal supply-state constraint over RTL `-logic_expr` signals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupplyLogicConstraint {
    pub supply_set: String,
    pub state_name: String,
    pub is_illegal: bool,
    pub simstate: Option<SimState>,
    pub logic_expr: Option<LogicConstraintAst>,
}

#[derive(Debug, Clone)]
pub(crate) struct StateEntry {
    pub state_name: String,
    pub var: Lit,
    pub simstate: SimState,
    pub power_v: VoltageSpec,
    pub power_vs: Vec<(VoltageSpec, Option<Lit>)>,
    pub ground_vs: Vec<(VoltageSpec, Option<Lit>)>,
}

/// Symbolic PST & supply-state SAT solver backed by `informal-sat`.
pub struct PstSat {
    pub(crate) cnf: CnfBuilder,
    pub(crate) ss_names: Vec<String>,
    pub(crate) ss_entries: Vec<Vec<StateEntry>>,
    pub(crate) lit_normal: Vec<Lit>,
    pub(crate) lit_corrupt_comb: Vec<Lit>,
    pub(crate) unconstrained: BTreeSet<SupplySetId>,
    pub(crate) equiv_class: Vec<SupplySetId>,
    pub(crate) func_equiv_class: Vec<SupplySetId>,
    pub(crate) signal_bits: BTreeMap<String, Vec<Lit>>,
    pub(crate) exported_constraints: Vec<SupplyLogicConstraint>,
    pub(crate) diagnostics: Vec<UpfDiag>,
    pub(crate) tol_uv: i64,
    pub(crate) consistent: bool,
}

impl PstSat {
    /// Builds and validates the CNF encoding of `resolved` in standalone UPF-only mode
    /// (`NullWidthProvider`) with voltage tolerance `tol_uv`.
    pub fn build(resolved: &ResolvedPowerIntent, tol_uv: i64) -> Self {
        Self::build_with_width_provider(resolved, tol_uv, &NullWidthProvider)
    }

    /// Builds and validates the CNF encoding of `resolved` using explicit signal widths
    /// from `width_provider` (falling back to deterministic IEEE 1800-2023 §11.6 standalone
    /// widths for any signal where `width_provider.width_of(sig)` returns `None`).
    pub fn build_with_width_provider(
        resolved: &ResolvedPowerIntent,
        tol_uv: i64,
        width_provider: &dyn SignalWidthProvider,
    ) -> Self {
        encode::build_pst_sat(resolved, tol_uv, width_provider)
    }

    pub fn diagnostics(&self) -> &[UpfDiag] {
        &self.diagnostics
    }

    pub fn is_state_space_consistent(&mut self) -> bool {
        self.consistent
    }

    /// Exports parsed legal & `-illegal` constraints as pure ASTs over RTL `-logic_expr` signals.
    pub fn export_logic_constraints(&self) -> Vec<SupplyLogicConstraint> {
        self.exported_constraints.clone()
    }

    /// Returns `(cnf_vars, cnf_clauses)` emitted into the underlying SAT solver.
    pub fn cnf_stats(&self) -> (usize, usize) {
        (
            self.cnf.next_var.saturating_sub(1) as usize,
            self.cnf.base_clauses.len(),
        )
    }
}
