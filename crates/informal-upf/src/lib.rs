//! IEEE 1801 UPF (1.0–4.0) and Cadence CPF (1.1–2.0) front end, canonical
//! `PowerIntent` IR, two-phase resolver, symbolic PST SAT solver, and UPF
//! equivalence checker.

pub mod compare;
pub mod cpf;
pub mod expr_bv;
pub mod ir;
pub mod pst;
pub mod resolve;
pub mod upf;

pub mod pst_sat {
    pub use crate::expr_bv::*;
    pub use crate::pst::*;
}
pub use crate::cpf as tcl_cpf;
pub use crate::upf as tcl_upf;

use std::path::Path;
use std::sync::Arc;

pub use compare::{
    compare_upf, compare_upf_with_config, CompareConfig, UpfCompareReport, UpfDiffItem, UpfDiffKind,
};
pub use cpf::{load_cpf_file, parse_cpf_str};
pub use expr_bv::{
    collect_logic_expr_signal_refs, collect_logic_expr_signals, parse_logic_expr,
    parse_logic_expr_in_scope, CmpOp, LogicConstraintAst, MapWidthProvider, NullWidthProvider,
    SignalWidthProvider,
};
pub use ir::{
    escape_glob, escape_glob_brackets, escape_rtl_brackets, glob, is_glob_char_class,
    normalize_scope, parse_uv_scalar, qualify_name, qualify_ref, qualify_signal_ref,
    split_tcl_list, strip_outer_braces, AppliesTo, ClampValue, DeferredOp, DesignAttribute,
    DiagSeverity, IllegalStateCombination, InterfaceCellRule, IsoId, IsolationStrategy,
    LevelShifterStrategy, LsId, LsRule, PortAttribute, PortStateDef, PowerDomain, PowerDomainId,
    PowerIntent, PowerStateDef, PowerSwitchDef, PowerSwitchId, PstId, PstRow, PstTable,
    RepeaterStrategy, RetId, RetentionStrategy, Sense, SimState, StrategyLocation, SupplyDirection,
    SupplyEquivalence, SupplyNet, SupplyNetId, SupplyPort, SupplyPortId, SupplyRailState,
    SupplySet, SupplySetId, SwitchStateExpr, UpfDiag, UpfLoc, UpfVersion, VoltageSpec,
    DEFAULT_VOLTAGE_TOL_UV, UV_PER_VOLT,
};
pub use pst::{sat_backend_name, LsRequirement, PstSat, PstWitness, SupplyLogicConstraint};
pub use resolve::{
    endpoint_base_matches, endpoint_matches_patterns, matches_any_element_in_scope,
    parse_bit_slice_expr, resolve, sv_slice_to_phys_range, ResolvedPowerIntent, SignalEndpoint,
    StrategyMatch,
};
pub use upf::{
    load_upf_file, load_upf_files, parse_upf_str, NullObjectFinder, ObjectFinder, ObjectType,
    UpfError, UpfFileSpec,
};

/// Loads and resolves a UPF file on disk using `finder`.
///
/// ```
/// use informal_upf::{parse_upf_and_resolve, NullObjectFinder, DEFAULT_VOLTAGE_TOL_UV, PstSat};
/// use std::sync::Arc;
///
/// let upf = r#"
///     upf_version 2.1
///     create_supply_net VDD
///     create_supply_net VSS
///     create_supply_set SS_AON -function {power VDD} -function {ground VSS}
///     create_power_domain PD_TOP -include_scope -supply {primary SS_AON}
///     add_power_state SS_AON -state {ON -supply_expr {power == {FULL_ON 0.9} && ground == {FULL_ON 0.0}} -simstate NORMAL}
/// "#;
/// let resolved = parse_upf_and_resolve(upf, None, None, Arc::new(NullObjectFinder)).unwrap();
/// let mut sat = PstSat::build(&resolved, DEFAULT_VOLTAGE_TOL_UV);
/// assert!(sat.is_state_space_consistent());
/// ```
pub fn load_upf(
    path: &Path,
    scope: Option<&str>,
    version: Option<UpfVersion>,
    finder: Arc<dyn ObjectFinder>,
) -> Result<ResolvedPowerIntent, UpfError> {
    let mut intent = load_upf_file(path, scope, version, finder.clone())?;
    Ok(resolve(&mut intent, finder.as_ref()))
}

/// Loads and resolves a CPF file on disk using `finder`.
pub fn load_cpf(
    path: &Path,
    scope: Option<&str>,
    version: Option<UpfVersion>,
    finder: Arc<dyn ObjectFinder>,
) -> Result<ResolvedPowerIntent, UpfError> {
    let mut intent = load_cpf_file(path, scope, version, finder.clone())?;
    Ok(resolve(&mut intent, finder.as_ref()))
}

/// Parses and resolves an inline UPF Tcl script string using `finder`.
pub fn parse_upf_and_resolve(
    script: &str,
    scope: Option<&str>,
    version: Option<UpfVersion>,
    finder: Arc<dyn ObjectFinder>,
) -> Result<ResolvedPowerIntent, UpfError> {
    let mut intent = parse_upf_str(script, scope, version, finder.clone())?;
    Ok(resolve(&mut intent, finder.as_ref()))
}

/// Parses and resolves an inline CPF Tcl script string using `finder`.
pub fn parse_cpf_and_resolve(
    script: &str,
    scope: Option<&str>,
    version: Option<UpfVersion>,
    finder: Arc<dyn ObjectFinder>,
) -> Result<ResolvedPowerIntent, UpfError> {
    let mut intent = parse_cpf_str(script, scope, version, finder.clone())?;
    Ok(resolve(&mut intent, finder.as_ref()))
}
