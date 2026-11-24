//! Liberty (`.lib`) low-power cell, pin, and PG-pin data model.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusType {
    pub name: String,
    pub bit_width: u32,
    pub bit_from: i32,
    pub bit_to: i32,
    pub downto: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PgType {
    PrimaryPower,
    PrimaryGround,
    BackupPower,
    BackupGround,
    InternalPower,
    InternalGround,
    Pwell,
    Nwell,
    DeepNwell,
    DeepPwell,
    Other(String),
}

impl PgType {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "primary_power" => Self::PrimaryPower,
            "primary_ground" => Self::PrimaryGround,
            "backup_power" => Self::BackupPower,
            "backup_ground" => Self::BackupGround,
            "internal_power" => Self::InternalPower,
            "internal_ground" => Self::InternalGround,
            "pwell" => Self::Pwell,
            "nwell" => Self::Nwell,
            "deepnwell" | "deep_nwell" => Self::DeepNwell,
            "deeppwell" | "deep_pwell" => Self::DeepPwell,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn is_power(&self) -> bool {
        matches!(
            self,
            Self::PrimaryPower | Self::BackupPower | Self::InternalPower
        )
    }

    pub fn is_ground(&self) -> bool {
        matches!(
            self,
            Self::PrimaryGround | Self::BackupGround | Self::InternalGround
        )
    }

    pub fn is_bias(&self) -> bool {
        matches!(
            self,
            Self::Pwell | Self::Nwell | Self::DeepNwell | Self::DeepPwell
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinDir {
    Input,
    Output,
    Inout,
    Internal,
}

impl PinDir {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "input" => Self::Input,
            "output" => Self::Output,
            "inout" => Self::Inout,
            _ => Self::Internal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LevelShifterType {
    Hl,
    Lh,
    HlLh,
}

impl LevelShifterType {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "HL" | "HIGH_TO_LOW" => Some(Self::Hl),
            "LH" | "LOW_TO_HIGH" => Some(Self::Lh),
            "HL_LH" | "BOTH" => Some(Self::HlLh),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsoClampKind {
    Zero,
    One,
    Latch,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionPinKind {
    Save,
    Restore,
    SaveRestore,
    Other(String),
}

impl RetentionPinKind {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "save" => Self::Save,
            "restore" => Self::Restore,
            "save_restore" | "both" => Self::SaveRestore,
            other => Self::Other(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PgPinDef {
    pub name: String,
    pub pg_type: PgType,
    pub voltage_name: Option<String>,
    pub nominal_voltage: Option<f64>,
    pub switch_function: Option<String>,
    pub pg_function: Option<String>,
    pub std_cell_main_rail: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalPinDef {
    pub name: String,
    pub is_bus: bool,
    pub width: u32,
    pub direction: PinDir,
    pub function: Option<String>,
    pub related_power_pin: Option<String>,
    pub related_ground_pin: Option<String>,
    pub related_bias_pin: Option<String>,
    pub power_down_function: Option<String>,
    pub is_clock: bool,
    pub is_always_on: bool,
    pub is_iso_data_pin: bool,
    pub is_iso_enable_pin: bool,
    pub is_ls_data_pin: bool,
    pub is_ls_enable_pin: bool,
    pub is_switch_pin: bool,
    pub retention_pin: Option<(RetentionPinKind, Option<String>)>,
}

impl Default for SignalPinDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            is_bus: false,
            width: 1,
            direction: PinDir::Input,
            function: None,
            related_power_pin: None,
            related_ground_pin: None,
            related_bias_pin: None,
            power_down_function: None,
            is_clock: false,
            is_always_on: false,
            is_iso_data_pin: false,
            is_iso_enable_pin: false,
            is_ls_data_pin: false,
            is_ls_enable_pin: false,
            is_switch_pin: false,
            retention_pin: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibertyCell {
    pub name: String,
    pub library: String,
    pub is_isolation_cell: bool,
    pub isolation_enable_condition: Option<String>,
    pub is_level_shifter: bool,
    pub level_shifter_type: Option<LevelShifterType>,
    pub valid_location: Option<String>,
    pub input_voltage_range: Option<(f64, f64)>,
    pub output_voltage_range: Option<(f64, f64)>,
    pub is_always_on: bool,
    pub retention_cell: Option<String>,
    pub switch_cell_type: Option<String>,
    pub is_macro_cell: bool,
    pub has_ff: bool,
    pub has_latch: bool,
    pub pg_pins: Vec<PgPinDef>,
    pub pins: Vec<SignalPinDef>,
}

impl LibertyCell {
    pub fn is_retention(&self) -> bool {
        self.retention_cell.is_some() || self.pins.iter().any(|p| p.retention_pin.is_some())
    }

    pub fn is_power_switch(&self) -> bool {
        self.switch_cell_type.is_some()
            || self.pins.iter().any(|p| p.is_switch_pin)
            || self.pg_pins.iter().any(|p| p.switch_function.is_some())
    }

    pub fn is_sequential(&self) -> bool {
        self.has_ff || self.has_latch || self.is_retention()
    }

    pub fn is_els(&self) -> bool {
        self.is_isolation_cell && self.is_level_shifter
    }

    pub fn pin(&self, name: &str) -> Option<&SignalPinDef> {
        self.pins.iter().find(|p| p.name == name)
    }

    pub fn pg_pin(&self, name: &str) -> Option<&PgPinDef> {
        self.pg_pins.iter().find(|p| p.name == name)
    }

    pub fn iso_enable_pin(&self) -> Option<&SignalPinDef> {
        self.pins.iter().find(|p| p.is_iso_enable_pin)
    }

    pub fn iso_data_pin(&self) -> Option<&SignalPinDef> {
        self.pins.iter().find(|p| p.is_iso_data_pin)
    }

    pub fn ls_data_pin(&self) -> Option<&SignalPinDef> {
        self.pins.iter().find(|p| p.is_ls_data_pin)
    }

    /// Returns `Some(true)` if isolation enable is active-high, `Some(false)` if active-low.
    pub fn iso_enable_active_high(&self) -> Option<bool> {
        if let Some(cond) = self.isolation_enable_condition.as_deref() {
            let trimmed = cond.trim();
            if trimmed.starts_with('!') || trimmed.starts_with('~') {
                return Some(false);
            }
            if !trimmed.is_empty() {
                return Some(true);
            }
        }
        if self.is_isolation_cell {
            return Some(true);
        }
        None
    }

    /// Infers the clamp value (`0`, `1`, or `latch`) of an isolation cell from its
    /// output pin `function` expression and `isolation_enable_condition`.
    pub fn iso_clamp_kind(&self) -> Option<IsoClampKind> {
        if !self.is_isolation_cell {
            return None;
        }
        if self.has_latch {
            return Some(IsoClampKind::Latch);
        }
        let active_high = self.iso_enable_active_high().unwrap_or(true);
        let en_name = self.iso_enable_pin().map(|p| p.name.as_str()).or_else(|| {
            self.isolation_enable_condition
                .as_deref()
                .map(|c| c.trim().trim_start_matches(['!', '~']).trim())
        })?;
        let data_name = self
            .iso_data_pin()
            .map(|p| p.name.as_str())
            .or_else(|| {
                self.pins
                    .iter()
                    .find(|p| p.direction == PinDir::Input && p.name != en_name)
                    .map(|p| p.name.as_str())
            })
            .unwrap_or("A");
        let out_fn = self
            .pins
            .iter()
            .find(|p| p.direction == PinDir::Output)
            .and_then(|p| p.function.as_deref())?;

        let mut env0 = HashMap::new();
        env0.insert(en_name, active_high);
        env0.insert(data_name, false);
        let mut env1 = HashMap::new();
        env1.insert(en_name, active_high);
        env1.insert(data_name, true);

        let v0 = eval_lib_bool(out_fn, &env0)?;
        let v1 = eval_lib_bool(out_fn, &env1)?;
        match (v0, v1) {
            (false, false) => Some(IsoClampKind::Zero),
            (true, true) => Some(IsoClampKind::One),
            _ => None,
        }
    }
}

/// Evaluates a simple Liberty boolean `function` / `switch_function` expression
/// supporting `!`/`~`/`'` (NOT), `&`/`*`/juxtaposition (AND), `|`/`+` (OR), `^` (XOR),
/// parentheses, `0`/`1`, and pin identifiers.
pub fn eval_lib_bool(expr: &str, env: &HashMap<&str, bool>) -> Option<bool> {
    let mut p = BoolEvalParser::new(expr, env);
    let val = p.parse_or()?;
    p.skip_ws();
    if p.pos == p.bytes.len() {
        Some(val)
    } else {
        None
    }
}

struct BoolEvalParser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    env: &'a HashMap<&'a str, bool>,
}

impl<'a> BoolEvalParser<'a> {
    fn new(src: &'a str, env: &'a HashMap<&'a str, bool>) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            env,
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn parse_or(&mut self) -> Option<bool> {
        let mut lhs = self.parse_xor()?;
        loop {
            self.skip_ws();
            if self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b'|' | b'+') {
                self.pos += 1;
                if self.pos < self.bytes.len() && self.bytes[self.pos] == b'|' {
                    self.pos += 1;
                }
                let rhs = self.parse_xor()?;
                lhs = lhs || rhs;
            } else {
                break;
            }
        }
        Some(lhs)
    }

    fn parse_xor(&mut self) -> Option<bool> {
        let mut lhs = self.parse_and()?;
        loop {
            self.skip_ws();
            if self.pos < self.bytes.len() && self.bytes[self.pos] == b'^' {
                self.pos += 1;
                let rhs = self.parse_and()?;
                lhs ^= rhs;
            } else {
                break;
            }
        }
        Some(lhs)
    }

    fn parse_and(&mut self) -> Option<bool> {
        let mut lhs = self.parse_unary()?;
        loop {
            self.skip_ws();
            if self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b'&' | b'*') {
                self.pos += 1;
                if self.pos < self.bytes.len() && self.bytes[self.pos] == b'&' {
                    self.pos += 1;
                }
                let rhs = self.parse_unary()?;
                lhs = lhs && rhs;
            } else if self.pos < self.bytes.len()
                && (self.bytes[self.pos].is_ascii_alphanumeric()
                    || matches!(self.bytes[self.pos], b'_' | b'!' | b'~' | b'('))
            {
                // Implicit space-separated AND in Liberty (`A B`)
                let rhs = self.parse_unary()?;
                lhs = lhs && rhs;
            } else {
                break;
            }
        }
        Some(lhs)
    }

    fn parse_unary(&mut self) -> Option<bool> {
        self.skip_ws();
        if self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b'!' | b'~') {
            self.pos += 1;
            return Some(!self.parse_unary()?);
        }
        let mut val = self.parse_atom()?;
        self.skip_ws();
        while self.pos < self.bytes.len() && self.bytes[self.pos] == b'\'' {
            self.pos += 1;
            val = !val;
            self.skip_ws();
        }
        Some(val)
    }

    fn parse_atom(&mut self) -> Option<bool> {
        self.skip_ws();
        if self.pos >= self.bytes.len() {
            return None;
        }
        if self.bytes[self.pos] == b'(' {
            self.pos += 1;
            let v = self.parse_or()?;
            self.skip_ws();
            if self.pos < self.bytes.len() && self.bytes[self.pos] == b')' {
                self.pos += 1;
                return Some(v);
            }
            return None;
        }
        let start = self.pos;
        while self.pos < self.bytes.len()
            && (self.bytes[self.pos].is_ascii_alphanumeric()
                || matches!(self.bytes[self.pos], b'_' | b'[' | b']'))
        {
            self.pos += 1;
        }
        if start == self.pos {
            return None;
        }
        let tok = &self.src[start..self.pos];
        match tok {
            "0" | "1'b0" => Some(false),
            "1" | "1'b1" => Some(true),
            _ => self.env.get(tok).copied(),
        }
    }
}

/// Merged Liberty cell database loaded across one or more `.lib` files.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LibertyDb {
    pub libraries: Vec<String>,
    pub voltage_map: HashMap<String, f64>,
    pub bus_types: HashMap<String, BusType>,
    pub cells: HashMap<String, LibertyCell>,
}

impl LibertyDb {
    pub fn cell(&self, name: &str) -> Option<&LibertyCell> {
        self.cells.get(name)
    }

    pub fn contains_cell(&self, name: &str) -> bool {
        self.cells.contains_key(name)
    }

    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }
}
