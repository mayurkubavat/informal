//! Symbolic UPF-to-UPF equivalence checker (`compare_upf`).

pub mod bipartite;
mod cross_format;
pub mod domains_pst;
pub mod strategies;

use serde::{Deserialize, Serialize};

use crate::ir::{UpfLoc, DEFAULT_VOLTAGE_TOL_UV};
use crate::pst::PstSat;
use crate::resolve::ResolvedPowerIntent;
use crate::upf::ObjectFinder;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UpfDiffKind {
    MissingDomain,
    ExtraDomain,
    DomainMembershipMismatch,
    SupplyEquivalenceMismatch,
    PstConsistencyMismatch,
    CrossingIsoRequirementMismatch,
    CrossingLsRequirementMismatch,
    IsolationStrategyMismatch,
    LevelShifterStrategyMismatch,
    RetentionStrategyMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpfDiffItem {
    pub kind: UpfDiffKind,
    pub subject: String,
    pub golden_detail: String,
    pub revised_detail: String,
    pub golden_loc: Option<UpfLoc>,
    pub revised_loc: Option<UpfLoc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpfCompareReport {
    pub equivalent: bool,
    pub diffs: Vec<UpfDiffItem>,
}

/// Configuration for [`compare_upf_with_config`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareConfig {
    pub tol_uv: i64,
}

impl Default for CompareConfig {
    fn default() -> Self {
        Self {
            tol_uv: DEFAULT_VOLTAGE_TOL_UV,
        }
    }
}

/// Compares a `golden` and `revised` [`ResolvedPowerIntent`] for domain partition,
/// supply equivalence, symbolic PST crossing-matrix, and strategy compatibility.
pub fn compare_upf(
    golden: &ResolvedPowerIntent,
    revised: &ResolvedPowerIntent,
    finder: &dyn ObjectFinder,
    tol_uv: i64,
) -> UpfCompareReport {
    compare_upf_with_config(golden, revised, finder, &CompareConfig { tol_uv })
}

/// Compares a `golden` and `revised` [`ResolvedPowerIntent`] using [`CompareConfig`].
pub fn compare_upf_with_config(
    golden: &ResolvedPowerIntent,
    revised: &ResolvedPowerIntent,
    finder: &dyn ObjectFinder,
    config: &CompareConfig,
) -> UpfCompareReport {
    let mut diffs = Vec::new();
    let tol_uv = config.tol_uv;

    let matched_domains =
        domains_pst::compare_domain_partitions(golden, revised, finder, &mut diffs);
    domains_pst::compare_supply_equivalence(golden, revised, &matched_domains, &mut diffs);

    let mut g_sat = PstSat::build(golden, tol_uv);
    let mut r_sat = PstSat::build(revised, tol_uv);
    domains_pst::compare_pst_crossing_matrix(
        golden,
        revised,
        &matched_domains,
        &mut g_sat,
        &mut r_sat,
        tol_uv,
        &mut diffs,
    );

    let cross_format = golden.intent.upf_version.is_some_and(|v| v.is_cpf())
        != revised.intent.upf_version.is_some_and(|v| v.is_cpf());
    strategies::compare_isolation_strategies(golden, revised, &mut g_sat, &mut r_sat, &mut diffs);
    strategies::compare_level_shifters(
        golden,
        revised,
        &matched_domains,
        &mut g_sat,
        tol_uv,
        cross_format,
        &mut diffs,
    );
    strategies::compare_retention_strategies(
        golden,
        revised,
        &mut g_sat,
        &mut r_sat,
        cross_format,
        &mut diffs,
    );

    UpfCompareReport {
        equivalent: diffs.is_empty(),
        diffs,
    }
}
