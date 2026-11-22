//! Source locations and diagnostic records for UPF/CPF parsing and resolution.

use serde::{Deserialize, Serialize};

/// Source location captured from Tcl `info frame -1` via `informal_tcl::caller_location()`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UpfLoc {
    pub file: String,
    pub line: u32,
}

impl UpfLoc {
    /// Captures the current Tcl caller file and line, if running inside a sourced script.
    pub fn from_tcl() -> Option<Self> {
        informal_tcl::caller_location().map(|(file, line)| Self { file, line })
    }

    /// Formats an optional `UpfLoc` as `file:line` or `<inline>:0`.
    pub fn display_opt(loc: Option<&Self>) -> String {
        match loc {
            Some(l) => format!("{}:{}", l.file, l.line),
            None => "<inline>:0".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagSeverity {
    Error,
    Warning,
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpfDiag {
    pub rule_id: String,
    pub severity: DiagSeverity,
    pub message: String,
    pub loc: Option<UpfLoc>,
}
