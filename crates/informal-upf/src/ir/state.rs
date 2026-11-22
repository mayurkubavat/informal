//! Voltage specifications (integer microvolts), simulation states, UPF versions, and PST tables.

use serde::{Deserialize, Serialize};

use super::diag::UpfLoc;
use super::path::split_tcl_list;
use super::{PstId, UV_PER_VOLT};

/// Voltage specification stored in exact integer microvolts (`i64`) to avoid
/// IEEE-754 floating-point drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VoltageSpec {
    Off,
    /// Single nominal voltage in microvolts (`1 V = 1_000_000 uV`).
    Nominal(i64),
    /// Inclusive `[min_uv, max_uv]` voltage range in microvolts.
    Range(i64, i64),
    /// Declared with `PARTIAL_ON` without bounds or `UNDETERMINED`.
    Undetermined,
}

impl VoltageSpec {
    /// Returns inclusive `(min_uv, max_uv)` if energized with known voltage bounds.
    pub fn bounds_uv(self) -> Option<(i64, i64)> {
        match self {
            Self::Nominal(v) => Some((v, v)),
            Self::Range(lo, hi) => Some((lo.min(hi), lo.max(hi))),
            Self::Off | Self::Undetermined => None,
        }
    }

    pub fn is_off(self) -> bool {
        matches!(self, Self::Off)
    }

    pub fn is_on_known(self) -> bool {
        matches!(self, Self::Nominal(_) | Self::Range(_, _))
    }

    /// Converts an `f64` voltage in Volts to rounded microvolts.
    pub fn from_volts_f64(v: f64) -> Result<i64, String> {
        if !v.is_finite() {
            return Err(format!("non-finite voltage: {v}"));
        }
        Ok((v * (UV_PER_VOLT as f64)).round() as i64)
    }

    /// Exact decimal parser without `f64` precision drift.
    ///
    /// Accepts `"OFF"`, `"off"`, `"UNDETERMINED"`, `"0.9"`, `"0.90"`, `"0.9V"`,
    /// `"900mV"`, `"900000uV"`, `"{0.8 1.0}"`, `"{0.8, 1.0}"`, `"0.8:1.0"`,
    /// `"{FULL_ON 0.90}"`, `"{PARTIAL_ON 0.7 0.8}"`, etc.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = strip_outer_braces(raw.trim());
        if trimmed.is_empty() {
            return Err("empty voltage specification".to_string());
        }
        let upper = trimmed.to_ascii_uppercase();
        if upper == "OFF" {
            return Ok(Self::Off);
        }
        if upper == "UNDETERMINED" || upper == "PARTIAL_ON" {
            return Ok(Self::Undetermined);
        }

        // Handle FULL_ON / PARTIAL_ON prefix if present.
        let tokens = split_tcl_list(trimmed);
        if let Some(first) = tokens.first() {
            let kw = first.to_ascii_uppercase();
            if kw == "OFF" && tokens.len() == 1 {
                return Ok(Self::Off);
            }
            if kw == "UNDETERMINED" && tokens.len() == 1 {
                return Ok(Self::Undetermined);
            }
            if kw == "FULL_ON" {
                let scalars = collect_voltage_scalars(&tokens[1..]);
                return match scalars.len() {
                    1 => Ok(Self::Nominal(parse_uv_scalar(&scalars[0])?)),
                    2 => {
                        let a = parse_uv_scalar(&scalars[0])?;
                        let b = parse_uv_scalar(&scalars[1])?;
                        Ok(Self::Range(a.min(b), a.max(b)))
                    }
                    _ => Err(format!("invalid FULL_ON voltage spec `{raw}`")),
                };
            }
            if kw == "PARTIAL_ON" {
                let scalars = collect_voltage_scalars(&tokens[1..]);
                return match scalars.len() {
                    0 => Ok(Self::Undetermined),
                    1 => Ok(Self::Nominal(parse_uv_scalar(&scalars[0])?)),
                    2 => {
                        let a = parse_uv_scalar(&scalars[0])?;
                        let b = parse_uv_scalar(&scalars[1])?;
                        Ok(Self::Range(a.min(b), a.max(b)))
                    }
                    _ => Err(format!("invalid PARTIAL_ON voltage spec `{raw}`")),
                };
            }
        }

        // Check colon range `0.8:1.0`.
        if let Some((lhs, rhs)) = trimmed.split_once(':') {
            let a = parse_uv_scalar(lhs)?;
            let b = parse_uv_scalar(rhs)?;
            return Ok(Self::Range(a.min(b), a.max(b)));
        }

        // Check comma or space separated 2-element range `0.8, 1.0` or `0.8 1.0`.
        let cleaned = trimmed.replace(',', " ");
        let parts: Vec<&str> = cleaned.split_whitespace().collect();
        match parts.len() {
            1 => Ok(Self::Nominal(parse_uv_scalar(parts[0])?)),
            2 => {
                let a = parse_uv_scalar(parts[0])?;
                let b = parse_uv_scalar(parts[1])?;
                Ok(Self::Range(a.min(b), a.max(b)))
            }
            _ => Err(format!("invalid voltage specification `{raw}`")),
        }
    }
}

fn collect_voltage_scalars(tokens: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for tok in tokens {
        let inner = strip_outer_braces(tok);
        for sub in split_tcl_list(inner) {
            let cleaned = strip_outer_braces(&sub).replace(',', " ");
            for part in cleaned.split_whitespace() {
                if part.contains(':') {
                    for piece in part.split(':') {
                        out.push(piece.to_string());
                    }
                } else {
                    out.push(part.to_string());
                }
            }
        }
    }
    out
}

/// Strips a single balanced pair of outer `{...}` braces if they enclose the whole string.
pub fn strip_outer_braces(s: &str) -> &str {
    let mut cur = s.trim();
    while cur.starts_with('{') && cur.ends_with('}') && cur.len() >= 2 {
        let inner = &cur[1..cur.len() - 1];
        let mut depth = 0i32;
        let mut balanced = true;
        let mut escaped = false;
        for ch in inner.chars() {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth < 0 {
                        balanced = false;
                        break;
                    }
                }
                _ => {}
            }
        }
        if balanced && depth == 0 {
            cur = inner.trim();
        } else {
            break;
        }
    }
    cur
}

/// Parses a single scalar voltage string into exact integer microvolts (`i64`).
pub fn parse_uv_scalar(raw: &str) -> Result<i64, String> {
    let mut s = raw.trim();
    if s.is_empty() {
        return Err("empty voltage scalar".to_string());
    }

    let neg = if let Some(rest) = s.strip_prefix('-') {
        s = rest.trim_start();
        true
    } else if let Some(rest) = s.strip_prefix('+') {
        s = rest.trim_start();
        false
    } else {
        false
    };

    let lower = s.to_ascii_lowercase();
    let (num_str, dec_places, scale): (&str, usize, i128) = if lower.ends_with("uv") {
        (&s[..s.len() - 2], 0, 1)
    } else if lower.ends_with("mv") {
        (&s[..s.len() - 2], 3, 1_000)
    } else if lower.ends_with('v') {
        (&s[..s.len() - 1], 6, 1_000_000)
    } else {
        (s, 6, 1_000_000)
    };

    let num_str = num_str.trim();
    if num_str.is_empty() {
        return Err(format!("invalid voltage `{raw}`"));
    }

    let (int_part, frac_part) = match num_str.split_once('.') {
        Some((i, f)) => (i, f),
        None => (num_str, ""),
    };

    if (int_part.is_empty() && frac_part.is_empty())
        || !int_part.chars().all(|c| c.is_ascii_digit())
        || !frac_part.chars().all(|c| c.is_ascii_digit())
    {
        return Err(format!("invalid voltage `{raw}`"));
    }

    let int_val: i128 = if int_part.is_empty() {
        0
    } else {
        int_part
            .parse::<i128>()
            .map_err(|_| format!("voltage overflow `{raw}`"))?
    };

    let mut frac_uv: i128 = 0;
    if dec_places > 0 && !frac_part.is_empty() {
        let bytes = frac_part.as_bytes();
        let take = bytes.len().min(dec_places);
        for &b in &bytes[..take] {
            frac_uv = frac_uv * 10 + (b - b'0') as i128;
        }
        for _ in take..dec_places {
            frac_uv *= 10;
        }
        if bytes.len() > dec_places && bytes[dec_places] >= b'5' {
            frac_uv += 1;
        }
    } else if dec_places == 0 && !frac_part.is_empty() && frac_part.as_bytes()[0] >= b'5' {
        frac_uv = 1;
    }

    let mag = int_val
        .checked_mul(scale)
        .and_then(|v| v.checked_add(frac_uv))
        .ok_or_else(|| format!("voltage overflow `{raw}`"))?;
    let signed = if neg { -mag } else { mag };
    i64::try_from(signed).map_err(|_| format!("voltage out of i64 microvolt range `{raw}`"))
}

/// IEEE 1801-2018/2024 supply simulation state semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SimState {
    Normal,
    Corrupt,
    CorruptOnActivity,
    CorruptStateOnChange,
    CorruptStateOnActivity,
    NotNormal,
    Off,
}

impl SimState {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_uppercase().as_str() {
            "NORMAL" => Ok(Self::Normal),
            "CORRUPT" => Ok(Self::Corrupt),
            "CORRUPT_ON_ACTIVITY" => Ok(Self::CorruptOnActivity),
            "CORRUPT_STATE_ON_CHANGE" => Ok(Self::CorruptStateOnChange),
            "CORRUPT_STATE_ON_ACTIVITY" => Ok(Self::CorruptStateOnActivity),
            "NOT_NORMAL" => Ok(Self::NotNormal),
            "OFF" => Ok(Self::Off),
            _ => Err(format!("invalid UPF simstate `{s}`")),
        }
    }

    pub fn is_normal(self) -> bool {
        matches!(self, Self::Normal)
    }

    /// True if combinational logic / outputs driven by this supply corrupt.
    pub fn corrupts_combinational(self) -> bool {
        matches!(
            self,
            Self::Corrupt | Self::CorruptOnActivity | Self::NotNormal | Self::Off
        )
    }

    /// True if sequential elements powered by this supply can lose state.
    pub fn corrupts_sequential(self) -> bool {
        !self.is_normal()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UpfVersion {
    V1_0,
    V2_0,
    V2_1,
    V3_0,
    V3_1,
    V4_0,
    Cpf1_1,
    Cpf2_0,
}

impl UpfVersion {
    pub fn parse_upf(s: &str) -> Result<Self, String> {
        match s.trim() {
            "1.0" | "1801-2009_1.0" => Ok(Self::V1_0),
            "2.0" | "1801-2009" => Ok(Self::V2_0),
            "2.1" | "1801-2013" => Ok(Self::V2_1),
            "3.0" | "1801-2015" => Ok(Self::V3_0),
            "3.1" | "1801-2018" => Ok(Self::V3_1),
            "4.0" | "1801-2024" => Ok(Self::V4_0),
            _ => Err(format!("unsupported UPF version `{s}`")),
        }
    }

    pub fn parse_cpf(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "1.0" | "1.1" | "cpf1.0" | "cpf1.1" | "cpf-1.0" | "cpf-1.1" => Ok(Self::Cpf1_1),
            "2.0" | "cpf2.0" | "cpf-2.0" => Ok(Self::Cpf2_0),
            _ => Err(format!("unsupported CPF version `{s}`")),
        }
    }

    pub fn is_cpf(self) -> bool {
        matches!(self, Self::Cpf1_1 | Self::Cpf2_0)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::V1_0 => "1.0",
            Self::V2_0 => "2.0",
            Self::V2_1 => "2.1",
            Self::V3_0 => "3.0",
            Self::V3_1 => "3.1",
            Self::V4_0 => "4.0",
            Self::Cpf1_1 => "cpf-1.1",
            Self::Cpf2_0 => "cpf-2.0",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupplyRailState {
    pub function: String,
    pub is_off: bool,
    pub voltage: VoltageSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerStateDef {
    pub target: String,
    pub scope: String,
    pub state_name: String,
    pub simstate: Option<SimState>,
    pub supply_expr: Option<String>,
    pub logic_expr: Option<String>,
    pub rail_states: Vec<SupplyRailState>,
    pub illegal: bool,
    #[serde(default)]
    pub illegal_override: Option<bool>,
    pub complete: bool,
    pub update: bool,
    pub loc: Option<UpfLoc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IllegalStateCombination {
    pub mode_name: String,
    pub scope: String,
    #[serde(default)]
    pub source_scope: String,
    #[serde(default)]
    pub design_top: Option<String>,
    /// Pairs of `(supply_set_name, state_name)` that must not hold simultaneously.
    pub domain_states: Vec<(String, String)>,
    pub logic_expr: Option<String>,
    pub fallback_logic_expr: Option<String>,
    pub loc: Option<UpfLoc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PstRow {
    pub state_name: String,
    /// Aligned 1:1 with `PstTable::supplies`. `"*"` denotes wildcard (from heterogeneous CPF modes).
    pub states: Vec<String>,
    #[serde(default)]
    pub logic_expr: Option<String>,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub source_scope: Option<String>,
    pub loc: Option<UpfLoc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PstTable {
    pub id: PstId,
    pub name: String,
    pub scope: String,
    pub supplies: Vec<String>,
    pub rows: Vec<PstRow>,
    pub loc: Option<UpfLoc>,
}

#[cfg(test)]
mod tests {
    use super::UpfVersion;

    #[test]
    fn parse_cpf_accepts_prefixed_and_bare_versions() {
        for s in ["1.0", "1.1", "cpf1.0", "cpf1.1", "CPF1.1", "cpf-1.1"] {
            assert_eq!(UpfVersion::parse_cpf(s), Ok(UpfVersion::Cpf1_1), "{s}");
        }
        for s in ["2.0", "cpf2.0", "CPF2.0", "cpf-2.0"] {
            assert_eq!(UpfVersion::parse_cpf(s), Ok(UpfVersion::Cpf2_0), "{s}");
        }
        assert!(UpfVersion::parse_cpf("3.0").is_err());
    }
}
