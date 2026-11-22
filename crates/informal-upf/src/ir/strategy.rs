//! Isolation, level-shifter, retention, power-switch, and repeater strategies.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::diag::UpfLoc;
use super::{IsoId, LsId, PowerSwitchId, RetId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AppliesTo {
    Inputs,
    Outputs,
    Both,
}

impl AppliesTo {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "inputs" | "input" => Ok(Self::Inputs),
            "outputs" | "output" => Ok(Self::Outputs),
            "both" => Ok(Self::Both),
            _ => Err(format!("invalid -applies_to `{s}`")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClampValue {
    Zero,
    One,
    Latch,
    Any,
    Z,
}

impl ClampValue {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "0" | "zero" | "low" => Ok(Self::Zero),
            "1" | "one" | "high" => Ok(Self::One),
            "latch" | "hold" => Ok(Self::Latch),
            "any" => Ok(Self::Any),
            "z" | "tristate" => Ok(Self::Z),
            _ => Err(format!("invalid clamp_value `{s}`")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Sense {
    High,
    Low,
    Posedge,
    Negedge,
}

impl Sense {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "high" | "1" | "active_high" => Ok(Self::High),
            "low" | "0" | "active_low" => Ok(Self::Low),
            "posedge" => Ok(Self::Posedge),
            "negedge" => Ok(Self::Negedge),
            _ => Err(format!("invalid sense `{s}`")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StrategyLocation {
    Self_,
    Parent,
    Sibling,
    Fanout,
    Automatic,
    Other,
}

impl StrategyLocation {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "self" => Ok(Self::Self_),
            "parent" => Ok(Self::Parent),
            "sibling" => Ok(Self::Sibling),
            "fanout" => Ok(Self::Fanout),
            "automatic" => Ok(Self::Automatic),
            "other" => Ok(Self::Other),
            _ => Err(format!("invalid -location `{s}`")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LsRule {
    LowToHigh,
    HighToLow,
    Both,
}

impl LsRule {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low_to_high" => Ok(Self::LowToHigh),
            "high_to_low" => Ok(Self::HighToLow),
            "both" => Ok(Self::Both),
            _ => Err(format!("invalid level-shifter -rule `{s}`")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationStrategy {
    pub id: IsoId,
    pub name: String,
    pub scope: String,
    pub domain: String,
    pub applies_to: Option<AppliesTo>,
    pub applies_to_clamp: Option<ClampValue>,
    pub elements: Vec<String>,
    pub exclude_elements: Vec<String>,
    pub source_filter: Option<String>,
    pub sink_filter: Option<String>,
    pub diff_supply_only: bool,
    pub no_isolation: bool,
    pub force_isolation: bool,
    pub clamp_value: Option<ClampValue>,
    pub isolation_supply_set: Option<String>,
    pub isolation_power_net: Option<String>,
    pub isolation_ground_net: Option<String>,
    pub isolation_signal: Option<String>,
    pub isolation_sense: Option<Sense>,
    #[serde(default)]
    pub ctrl_scope: Option<String>,
    pub location: Option<StrategyLocation>,
    pub mapped_cells: Vec<String>,
    pub mapped_lib_model: Option<String>,
    pub precedence_order: u32,
    pub update: bool,
    pub loc: Option<UpfLoc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LevelShifterStrategy {
    pub id: LsId,
    pub name: String,
    pub scope: String,
    pub domain: String,
    pub applies_to: Option<AppliesTo>,
    pub rule: Option<LsRule>,
    pub threshold_uv: Option<i64>,
    pub elements: Vec<String>,
    pub exclude_elements: Vec<String>,
    pub source_filter: Option<String>,
    pub sink_filter: Option<String>,
    pub no_shift: bool,
    pub force_shift: bool,
    pub location: Option<StrategyLocation>,
    pub input_supply_set: Option<String>,
    pub output_supply_set: Option<String>,
    pub internal_supply_set: Option<String>,
    pub mapped_cells: Vec<String>,
    pub precedence_order: u32,
    pub update: bool,
    pub loc: Option<UpfLoc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionStrategy {
    pub id: RetId,
    pub name: String,
    pub scope: String,
    pub domain: String,
    pub elements: Vec<String>,
    pub exclude_elements: Vec<String>,
    pub no_retention: bool,
    pub retention_supply_set: Option<String>,
    pub retention_power_net: Option<String>,
    pub retention_ground_net: Option<String>,
    pub save_signal: Option<String>,
    pub save_sense: Option<Sense>,
    pub save_condition: Option<String>,
    #[serde(default)]
    pub save_scope: Option<String>,
    #[serde(default)]
    pub save_cond_scope: Option<String>,
    pub restore_signal: Option<String>,
    pub restore_sense: Option<Sense>,
    pub restore_condition: Option<String>,
    #[serde(default)]
    pub restore_scope: Option<String>,
    #[serde(default)]
    pub restore_cond_scope: Option<String>,
    pub retention_condition: Option<String>,
    #[serde(default)]
    pub retention_cond_scope: Option<String>,
    pub use_retention_as_primary: bool,
    pub mapped_cells: Vec<String>,
    pub mapped_lib_model: Option<String>,
    pub precedence_order: u32,
    pub update: bool,
    pub loc: Option<UpfLoc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchStateExpr {
    pub state_name: String,
    pub supply_port: Option<String>,
    pub expr: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerSwitchDef {
    pub id: PowerSwitchId,
    pub name: String,
    pub scope: String,
    pub domain: String,
    pub supply_set: Option<String>,
    pub input_supply_ports: Vec<(String, String)>,
    pub output_supply_ports: Vec<(String, String)>,
    pub control_ports: Vec<(String, String)>,
    #[serde(default)]
    pub control_port_scopes: BTreeMap<String, String>,
    #[serde(default)]
    pub shutoff_synthesized_ctrl: bool,
    pub ack_ports: Vec<(String, String)>,
    pub on_states: Vec<SwitchStateExpr>,
    pub off_states: Vec<SwitchStateExpr>,
    pub error_states: Vec<SwitchStateExpr>,
    pub mapped_cells: Vec<String>,
    pub loc: Option<UpfLoc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepeaterStrategy {
    pub name: String,
    pub scope: String,
    pub domain: String,
    pub applies_to: Option<AppliesTo>,
    pub elements: Vec<String>,
    pub exclude_elements: Vec<String>,
    pub source_filter: Option<String>,
    pub sink_filter: Option<String>,
    pub repeater_supply: Option<String>,
    pub mapped_cells: Vec<String>,
    pub loc: Option<UpfLoc>,
}
