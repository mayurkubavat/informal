//! Canonical version-neutral Power Intent IR for IEEE 1801 UPF (1.0–4.0)
//! and Cadence CPF (1.1–2.0).

pub mod diag;
pub mod domain;
pub mod glob;
pub mod path;
pub mod state;
pub mod strategy;
pub mod supply;

pub use diag::{DiagSeverity, UpfDiag, UpfLoc};
pub use domain::{DeferredOp, DesignAttribute, InterfaceCellRule, PortAttribute, PowerDomain};
pub(crate) use glob::{
    closed_bracket_kind, escape_sv_escaped_backslashes_for_glob, has_unescaped_glob_wildcards,
    unescape_rtl_brackets,
};
pub use glob::{escape_glob, escape_glob_brackets, escape_rtl_brackets, glob, is_glob_char_class};
pub(crate) use path::{
    has_hier_sep, is_glob_escaped_bracket_at, last_hier_segment, push_norm_segments,
    scan_sv_escaped_ident_end, split_handle_dot, split_hier_segments, split_parent_leaf,
    strip_sv_escapes_in_glob, strip_sv_escapes_in_path, trim_hier_path,
};
pub use path::{normalize_scope, qualify_name, qualify_ref, qualify_signal_ref, split_tcl_list};
pub use state::{
    parse_uv_scalar, strip_outer_braces, IllegalStateCombination, PowerStateDef, PstRow, PstTable,
    SimState, SupplyRailState, UpfVersion, VoltageSpec,
};
pub use strategy::{
    AppliesTo, ClampValue, IsolationStrategy, LevelShifterStrategy, LsRule, PowerSwitchDef,
    RepeaterStrategy, RetentionStrategy, Sense, StrategyLocation, SwitchStateExpr,
};
pub use supply::{
    PortStateDef, SupplyDirection, SupplyEquivalence, SupplyNet, SupplyPort, SupplySet,
};

use serde::{Deserialize, Serialize};

/// Microvolts per Volt (`1 V = 1_000_000 uV`).
pub const UV_PER_VOLT: i64 = 1_000_000;

/// Default voltage comparison tolerance in microvolts (`1 mV = 1_000 uV`).
pub const DEFAULT_VOLTAGE_TOL_UV: i64 = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SupplyPortId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SupplyNetId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SupplySetId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PowerDomainId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PowerSwitchId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct IsoId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LsId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RetId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PstId(pub u32);

/// Version-neutral canonical Power Intent IR produced by `upf` and `cpf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerIntent {
    pub upf_version: Option<UpfVersion>,
    pub design_top: Option<String>,
    pub hierarchy_separator: char,
    pub create_implicit_supply_sets: bool,
    pub loaded_files: Vec<String>,
    pub supply_ports: Vec<SupplyPort>,
    pub supply_nets: Vec<SupplyNet>,
    pub supply_sets: Vec<SupplySet>,
    pub equivalences: Vec<SupplyEquivalence>,
    pub domains: Vec<PowerDomain>,
    pub power_states: Vec<PowerStateDef>,
    pub illegal_combinations: Vec<IllegalStateCombination>,
    pub pst_tables: Vec<PstTable>,
    pub isolation_strategies: Vec<IsolationStrategy>,
    pub level_shifter_strategies: Vec<LevelShifterStrategy>,
    pub retention_strategies: Vec<RetentionStrategy>,
    pub power_switches: Vec<PowerSwitchDef>,
    pub port_attributes: Vec<PortAttribute>,
    pub repeater_strategies: Vec<RepeaterStrategy>,
    pub design_attributes: Vec<DesignAttribute>,
    pub interface_cells: Vec<InterfaceCellRule>,
    pub deferred_ops: Vec<DeferredOp>,
    pub diagnostics: Vec<UpfDiag>,
}

impl Default for PowerIntent {
    fn default() -> Self {
        Self {
            upf_version: None,
            design_top: None,
            hierarchy_separator: '/',
            create_implicit_supply_sets: true,
            loaded_files: Vec::new(),
            supply_ports: Vec::new(),
            supply_nets: Vec::new(),
            supply_sets: Vec::new(),
            equivalences: Vec::new(),
            domains: Vec::new(),
            power_states: Vec::new(),
            illegal_combinations: Vec::new(),
            pst_tables: Vec::new(),
            isolation_strategies: Vec::new(),
            level_shifter_strategies: Vec::new(),
            retention_strategies: Vec::new(),
            power_switches: Vec::new(),
            port_attributes: Vec::new(),
            repeater_strategies: Vec::new(),
            design_attributes: Vec::new(),
            interface_cells: Vec::new(),
            deferred_ops: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
}
