//! Supply ports, nets, sets, and equivalence declarations.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::diag::UpfLoc;
use super::state::VoltageSpec;
use super::{SupplyNetId, SupplyPortId, SupplySetId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SupplyDirection {
    In,
    Out,
    InOut,
}

impl SupplyDirection {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "in" => Ok(Self::In),
            "out" => Ok(Self::Out),
            "inout" => Ok(Self::InOut),
            _ => Err(format!("invalid supply direction `{s}`")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupplyPort {
    pub id: SupplyPortId,
    pub name: String,
    pub short_name: String,
    pub scope: String,
    pub direction: SupplyDirection,
    pub domain: Option<String>,
    pub resolve: Option<String>,
    pub loc: Option<UpfLoc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortStateDef {
    pub state_name: String,
    pub voltage: VoltageSpec,
    pub is_off: bool,
    pub loc: Option<UpfLoc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupplyNet {
    pub id: SupplyNetId,
    pub name: String,
    pub short_name: String,
    pub scope: String,
    pub domain: Option<String>,
    pub resolve: Option<String>,
    pub reuse: bool,
    pub connected_ports: Vec<String>,
    pub connected_pins: Vec<String>,
    pub port_states: Vec<PortStateDef>,
    pub nominal_voltage: Option<VoltageSpec>,
    pub is_ground: bool,
    pub is_internal: bool,
    pub loc: Option<UpfLoc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupplySet {
    pub id: SupplySetId,
    pub name: String,
    pub scope: String,
    pub power: Option<String>,
    pub ground: Option<String>,
    pub pwell: Option<String>,
    pub nwell: Option<String>,
    pub deeppwell: Option<String>,
    pub deepnwell: Option<String>,
    pub extra_functions: BTreeMap<String, String>,
    pub is_implicit: bool,
    pub reference_gnd: Option<String>,
    pub loc: Option<UpfLoc>,
}

impl SupplySet {
    pub fn set_function(&mut self, func: &str, net: String) -> Result<(), String> {
        match func.trim().to_ascii_lowercase().as_str() {
            "power" => self.power = Some(net),
            "ground" => self.ground = Some(net),
            "pwell" => self.pwell = Some(net),
            "nwell" => self.nwell = Some(net),
            "deeppwell" => self.deeppwell = Some(net),
            "deepnwell" => self.deepnwell = Some(net),
            other if !other.is_empty() => {
                self.extra_functions.insert(other.to_string(), net);
            }
            _ => return Err(format!("empty supply function `{func}`")),
        }
        Ok(())
    }

    pub fn get_function(&self, func: &str) -> Option<&str> {
        match func.trim().to_ascii_lowercase().as_str() {
            "power" => self.power.as_deref(),
            "ground" => self.ground.as_deref(),
            "pwell" => self.pwell.as_deref(),
            "nwell" => self.nwell.as_deref(),
            "deeppwell" => self.deeppwell.as_deref(),
            "deepnwell" => self.deepnwell.as_deref(),
            other => self.extra_functions.get(other).map(String::as_str),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupplyEquivalence {
    pub sets: Vec<String>,
    pub nets: Vec<String>,
    pub function_only: bool,
    pub scope: String,
    pub loc: Option<UpfLoc>,
}
