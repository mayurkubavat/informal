use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const SPEC_SCHEMA: &str = "informal.spec/1";
pub const AUDIT_SCHEMA: &str = "informal.spec_audit/1";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DesignIntent {
    #[serde(default)]
    pub top: Option<String>,
    #[serde(default)]
    pub clocks: Vec<String>,
    #[serde(default)]
    pub reset: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BoundarySpec {
    /// Legal primary inputs (or globs) that environment `assume` properties may
    /// constrain. Empty means all elaborated primary inputs are legal.
    #[serde(default)]
    pub allowed_assume_inputs: Vec<String>,
    /// Signals (or globs) explicitly forbidden inside environment `assume`s.
    #[serde(default)]
    pub forbidden_assume_signals: Vec<String>,
    /// Internal DUT signals explicitly waived for `assume` (default empty:
    /// internal signals require `assert` + `assume -from_assert`).
    #[serde(default)]
    pub allow_internal_assumes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementSpec {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// Target safety/liveness assertion names or globs (`*`, `?`).
    #[serde(default)]
    pub assertions: Vec<String>,
    /// Mandatory anti-vacuity / reachability witness cover names or globs.
    #[serde(default)]
    pub covers: Vec<String>,
    /// Optional environment assumption names associated with this requirement.
    #[serde(default)]
    pub assumptions: Vec<String>,
}

fn default_proof_target() -> String {
    "proven".into()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignoffGates {
    /// `"proven"` (unbounded, default) or `"bounded_proven"`.
    #[serde(default = "default_proof_target")]
    pub proof_target: String,
    /// Minimum BMC bound k when `proof_target == "bounded_proven"`.
    #[serde(default)]
    pub min_bound: u32,
    /// Maximum tolerated boundary-violating assumptions (default 0).
    #[serde(default)]
    pub max_overconstraints: usize,
    /// Maximum tolerated 2QBF dead-end trap states (default 0).
    #[serde(default)]
    pub max_dead_ends: usize,
    /// Whether `spec_audit` requires `check_dead_end=true` to pass signoff.
    #[serde(default)]
    pub require_dead_end_check: bool,
    /// Reject proven assertions whose vacuity status is `vacuous` (default true).
    #[serde(default = "default_true")]
    pub forbid_vacuous: bool,
    /// Require every requirement to declare at least one witness cover.
    #[serde(default)]
    pub require_covers: bool,
    /// Block signoff while proofs still stand on AI-added assumptions/cutpoints.
    #[serde(default)]
    pub forbid_under_ai: bool,
    /// Optional COI formal coverage floor percentage (`0.0..=100.0`).
    #[serde(default)]
    pub coi_coverage_floor: Option<f64>,
    /// Optional mutant-kill score floor percentage (`0.0..=100.0`).
    #[serde(default)]
    pub mutation_score_floor: Option<f64>,
}

impl Default for SignoffGates {
    fn default() -> Self {
        Self {
            proof_target: default_proof_target(),
            min_bound: 0,
            max_overconstraints: 0,
            max_dead_ends: 0,
            require_dead_end_check: false,
            forbid_vacuous: true,
            require_covers: false,
            forbid_under_ai: false,
            coi_coverage_floor: None,
            mutation_score_floor: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LowPowerGates {
    /// Require at least one IEEE 1801 UPF or CPF file to be loaded (`load_upf`).
    #[serde(default)]
    pub require_upf: bool,
    /// Maximum allowed open/CEX low-power error violations (e.g. `0`).
    #[serde(default)]
    pub max_open_lp_errors: Option<usize>,
    /// Require formal LPV (`check_lp -stage formal` / `prove_corruption`) with zero failing `LowPower` CEXs.
    #[serde(default)]
    pub require_formal_lpv: bool,
    /// Allow manual `set_lp_waiver` waivers (default `true`; when `false`, only `formally_waived` certified waivers pass).
    #[serde(default = "default_true")]
    pub allow_manual_lp_waivers: bool,
}

impl Default for LowPowerGates {
    fn default() -> Self {
        Self {
            require_upf: false,
            max_open_lp_errors: None,
            require_formal_lpv: false,
            allow_manual_lp_waivers: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FvpSpec {
    pub schema: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub design: DesignIntent,
    #[serde(default)]
    pub boundary: BoundarySpec,
    #[serde(default)]
    pub requirements: Vec<RequirementSpec>,
    #[serde(default)]
    pub signoff: SignoffGates,
    #[serde(default)]
    pub low_power: Option<LowPowerGates>,
}

impl FvpSpec {
    pub fn parse_value(v: &Value) -> Result<Self, String> {
        let spec: Self =
            serde_json::from_value(v.clone()).map_err(|e| format!("invalid fvp.spec JSON: {e}"))?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn parse_str(text: &str) -> Result<Self, String> {
        let v: Value =
            serde_json::from_str(text).map_err(|e| format!("malformed fvp.spec JSON: {e}"))?;
        Self::parse_value(&v)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SPEC_SCHEMA {
            return Err(format!(
                "unsupported spec `schema`: got `{}`, expected `{SPEC_SCHEMA}`",
                self.schema
            ));
        }
        if !matches!(
            self.signoff.proof_target.as_str(),
            "proven" | "bounded_proven"
        ) {
            return Err(format!(
                "`signoff.proof_target` must be `proven` or `bounded_proven` (got `{}`)",
                self.signoff.proof_target
            ));
        }
        for (label, opt) in [
            (
                "signoff.coi_coverage_floor",
                self.signoff.coi_coverage_floor,
            ),
            (
                "signoff.mutation_score_floor",
                self.signoff.mutation_score_floor,
            ),
        ] {
            if let Some(pct) = opt {
                if !(0.0..=100.0).contains(&pct) || pct.is_nan() {
                    return Err(format!(
                        "`{label}` must be between 0.0 and 100.0 (got {pct})"
                    ));
                }
            }
        }
        let mut seen = BTreeSet::new();
        for req in &self.requirements {
            let id = req.id.trim();
            if id.is_empty() {
                return Err(
                    "every requirement in `requirements` must have a non-empty `id`".into(),
                );
            }
            if !seen.insert(id.to_string()) {
                return Err(format!("duplicate requirement id `{id}` in spec"));
            }
        }
        Ok(())
    }
}

/// Optional live solver checks passed into [`audit_session`].
#[derive(Debug, Clone, Default)]
pub struct AuditRuntime {
    pub spec_source: String,
    pub task: String,
    pub env_conflict: bool,
    pub conflict_set: Vec<String>,
    pub dead_end_checked: bool,
    pub dead_end_found: bool,
    pub dead_end_depth: Option<u32>,
    pub mutation_score: Option<f64>,
}
