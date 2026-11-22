//! Power domains, port/design attributes, interface cell rules, and deferred resolution ops.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::diag::UpfLoc;
use super::strategy::{AppliesTo, ClampValue, Sense, StrategyLocation};
use super::supply::PortStateDef;
use super::PowerDomainId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerDomain {
    pub id: PowerDomainId,
    pub name: String,
    pub short_name: String,
    pub scope: String,
    pub include_scope: bool,
    pub elements: Vec<String>,
    pub exclude_elements: Vec<String>,
    pub transitive: bool,
    pub atomic: bool,
    pub primary_supply_set: Option<String>,
    pub supply_handles: BTreeMap<String, String>,
    pub legacy_primary_power_net: Option<String>,
    pub legacy_primary_ground_net: Option<String>,
    pub is_top: bool,
    pub update: bool,
    pub loc: Option<UpfLoc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortAttribute {
    pub ports: Vec<String>,
    pub elements: Vec<String>,
    pub exclude_elements: Vec<String>,
    pub domains: Vec<String>,
    pub applies_to: Option<AppliesTo>,
    pub scope: String,
    pub driver_supply: Option<String>,
    pub receiver_supply: Option<String>,
    pub related_power_port: Option<String>,
    pub related_ground_port: Option<String>,
    pub related_bias_ports: Vec<String>,
    pub clamp_value: Option<ClampValue>,
    pub repeater_supply: Option<String>,
    pub is_isolated: Option<bool>,
    pub is_hard_macro: bool,
    pub model: Option<String>,
    pub attributes: BTreeMap<String, String>,
    pub loc: Option<UpfLoc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignAttribute {
    pub elements: Vec<String>,
    pub exclude_elements: Vec<String>,
    pub models: Vec<String>,
    pub scope: String,
    pub is_hard_macro: bool,
    pub attributes: BTreeMap<String, String>,
    pub loc: Option<UpfLoc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceCellRule {
    pub name: String,
    pub scope: String,
    pub domain: String,
    pub strategy: Option<String>,
    pub elements: Vec<String>,
    pub lib_cells: Vec<String>,
    pub map_supply_set: Vec<(String, String)>,
    pub update: bool,
    pub loc: Option<UpfLoc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeferredOp {
    AddPortState {
        target: String,
        scope: String,
        states: Vec<PortStateDef>,
        loc: Option<UpfLoc>,
    },
    ConnectSupplyNet {
        net: String,
        scope: String,
        ports: Vec<String>,
        pins: Vec<String>,
        domain: Option<String>,
        elements: Vec<String>,
        transitive: bool,
        pg_type: Option<String>,
        loc: Option<UpfLoc>,
    },
    AssociateSupplySet {
        supply_set: String,
        handle: String,
        scope: String,
        loc: Option<UpfLoc>,
    },
    SetDomainSupplyNet {
        domain: String,
        scope: String,
        primary_power_net: Option<String>,
        primary_ground_net: Option<String>,
        loc: Option<UpfLoc>,
    },
    SetIsolationControl {
        name: String,
        scope: String,
        domain: String,
        isolation_signal: Option<String>,
        isolation_sense: Option<Sense>,
        location: Option<StrategyLocation>,
        loc: Option<UpfLoc>,
    },
    MapIsolationCell {
        name: String,
        scope: String,
        domain: String,
        lib_cells: Vec<String>,
        lib_cell_type: Option<String>,
        lib_model_name: Option<String>,
        elements: Vec<String>,
        loc: Option<UpfLoc>,
    },
    MapLevelShifterCell {
        name: String,
        scope: String,
        domain: String,
        lib_cells: Vec<String>,
        elements: Vec<String>,
        loc: Option<UpfLoc>,
    },
    SetRetentionControl {
        name: String,
        scope: String,
        domain: String,
        save_signal: Option<(String, Sense)>,
        restore_signal: Option<(String, Sense)>,
        save_condition: Option<String>,
        restore_condition: Option<String>,
        assert_r_mutex: Option<String>,
        assert_s_mutex: Option<String>,
        assert_rs_mutex: Option<String>,
        loc: Option<UpfLoc>,
    },
    SetRetentionElements {
        name: String,
        scope: String,
        domain: String,
        elements: Vec<String>,
        exclude_elements: Vec<String>,
        loc: Option<UpfLoc>,
    },
    MapRetentionCell {
        name: String,
        scope: String,
        domain: String,
        lib_cells: Vec<String>,
        lib_cell_type: Option<String>,
        lib_model_name: Option<String>,
        elements: Vec<String>,
        loc: Option<UpfLoc>,
    },
    MapPowerSwitch {
        switches: Vec<String>,
        scope: String,
        domain: Option<String>,
        lib_cells: Vec<String>,
        port_map: Vec<(String, String)>,
        loc: Option<UpfLoc>,
    },
    GlobalConnection {
        net: String,
        pins: Vec<String>,
        domain: Option<String>,
        instances: Vec<String>,
        scope: String,
        loc: Option<UpfLoc>,
    },
    UpdateCpfIso {
        names: Vec<String>,
        scope: String,
        #[serde(default = "default_hier_sep")]
        sep: char,
        location: Option<StrategyLocation>,
        lib_cells: Vec<String>,
        loc: Option<UpfLoc>,
    },
    UpdateCpfLs {
        names: Vec<String>,
        scope: String,
        #[serde(default = "default_hier_sep")]
        sep: char,
        location: Option<StrategyLocation>,
        lib_cells: Vec<String>,
        loc: Option<UpfLoc>,
    },
    UpdateCpfRet {
        names: Vec<String>,
        scope: String,
        #[serde(default = "default_hier_sep")]
        sep: char,
        lib_cells: Vec<String>,
        use_secondary_for_output: bool,
        loc: Option<UpfLoc>,
    },
    CpfEquivalentNets {
        domain: String,
        scope: String,
        is_ground: bool,
        nets: Vec<String>,
        function_only: bool,
        loc: Option<UpfLoc>,
    },
}

fn default_hier_sep() -> char {
    '/'
}
