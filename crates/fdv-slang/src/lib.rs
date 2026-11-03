//! SystemVerilog front end on slang.
//!
//! Phase 1 step one: slang checks the design and reports real
//! SystemVerilog diagnostics with file and line, names the top modules,
//! and lists the instance tree and every signal with its width. fdv uses
//! that to pick the top, to reject typos in property expressions before
//! elaboration, and (in the app) to show the hierarchy. Lowering slang's
//! AST to the model IR, which retires the Yosys path, comes next.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: String,
    pub message: String,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub column: Option<u32>,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.file, self.line) {
            (Some(file), Some(line)) => write!(
                f,
                "{file}:{line}:{}: {}: {}",
                self.column.unwrap_or(0),
                self.severity,
                self.message
            ),
            _ => write!(f, "{}: {}", self.severity, self.message),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Instance {
    pub path: String,
    pub module: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Signal {
    pub path: String,
    pub width: u64,
    /// Parameters and enum values.
    #[serde(default)]
    pub constant: bool,
}

/// What slang found in a design.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Design {
    pub ok: bool,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(default)]
    pub tops: Vec<String>,
    #[serde(default)]
    pub instances: Vec<Instance>,
    #[serde(default)]
    pub signals: Vec<Signal>,
}

impl Design {
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics.iter().filter(|d| d.severity == "error")
    }

    /// Names a property expression in the top module's scope uses that the
    /// design does not declare. Dotted names are checked by their first
    /// part (struct members, interface signals).
    pub fn unknown_names(&self, top: &str, expr: &str) -> Vec<String> {
        const SKIP: &[&str] = &[
            "posedge", "negedge", "edge", "disable", "iff", "or", "and", "not", "if", "else",
            "inside", "dist",
        ];
        let prefix = format!("{top}.");
        // Signals, plus instance names (the start of a hierarchical name).
        let mut known: std::collections::HashSet<&str> =
            self.top_signals(top).into_iter().map(|(n, _)| n).collect();
        known.extend(
            self.instances
                .iter()
                .filter_map(|i| i.path.strip_prefix(&prefix))
                .filter(|n| !n.contains('.')),
        );
        let b = expr.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            if c.is_ascii_alphabetic() || c == b'_' {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$')
                {
                    i += 1;
                }
                let prev = if start > 0 { b[start - 1] } else { b' ' };
                let word = &expr[start..i];
                // `$past`, `4'd9`, `.member`, and digits glued to a base.
                let skip = prev == b'$'
                    || prev == b'\''
                    || prev == b'.'
                    || prev.is_ascii_digit()
                    || SKIP.contains(&word);
                if !skip && !known.contains(word) && !out.iter().any(|w| w == word) {
                    out.push(word.to_string());
                }
            } else {
                i += 1;
            }
        }
        out
    }

    /// Signals of the top instance, by their local name.
    pub fn top_signals(&self, top: &str) -> Vec<(&str, u64)> {
        let prefix = format!("{top}.");
        self.signals
            .iter()
            .filter_map(|s| {
                s.path
                    .strip_prefix(&prefix)
                    .filter(|n| !n.contains('.'))
                    .map(|n| (n, s.width))
            })
            .collect()
    }
}

/// Inputs to a compile, in slang command-line terms.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub files: Vec<PathBuf>,
    pub defines: Vec<(String, Option<String>)>,
    pub incdirs: Vec<PathBuf>,
    pub top: Option<String>,
    pub params: Vec<(String, String)>,
}

impl Request {
    fn words(&self) -> Vec<String> {
        let mut w = Vec::new();
        for (k, v) in &self.defines {
            w.push(match v {
                Some(v) => format!("-D{k}={v}"),
                None => format!("-D{k}"),
            });
        }
        for i in &self.incdirs {
            w.push(format!("-I{}", i.display()));
        }
        if let Some(t) = &self.top {
            w.push(format!("--top={t}"));
        }
        for (k, v) in &self.params {
            w.push(format!("-G{k}={v}"));
        }
        // Formal designs commonly leave nets undriven and use SVA freely.
        w.push("--ignore-unknown-modules".into());
        for f in &self.files {
            w.push(f.display().to_string());
        }
        w
    }
}

#[cfg(fdv_slang)]
mod ffi {
    use std::ffi::c_char;
    extern "C" {
        pub fn fdv_slang_lower(args: *const c_char) -> *mut c_char;
        pub fn fdv_slang_compile(args: *const c_char) -> *mut c_char;
        pub fn fdv_slang_free(p: *mut c_char);
    }
}

/// A design lowered to Btor2 by slang (see `cpp/lower.cpp`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Lowered {
    pub ok: bool,
    pub errors: Vec<String>,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub btor2: String,
}

#[cfg(fdv_slang)]
fn call(
    f: unsafe extern "C" fn(*const std::ffi::c_char) -> *mut std::ffi::c_char,
    req: &Request,
) -> String {
    let mut buf = Vec::new();
    for w in req.words() {
        buf.extend(w.replace('\0', "").into_bytes());
        buf.push(0);
    }
    buf.push(0);
    unsafe {
        let p = f(buf.as_ptr() as *const std::ffi::c_char);
        let s = std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
        ffi::fdv_slang_free(p);
        s
    }
}

/// Elaborate and lower the design to a Btor2 model: every labelled
/// immediate assertion becomes a `bad` line named by its label, and
/// immediate assumptions become constraints. `None` without slang.
pub fn lower(req: &Request) -> Option<Lowered> {
    #[cfg(fdv_slang)]
    {
        let json = call(ffi::fdv_slang_lower, req);
        Some(serde_json::from_str(&json).unwrap_or_else(|e| Lowered {
            ok: false,
            errors: vec![format!("bad reply from the slang shim: {e}")],
            ..Default::default()
        }))
    }
    #[cfg(not(fdv_slang))]
    {
        let _ = req;
        None
    }
}

/// True when this build links slang.
pub fn available() -> bool {
    cfg!(fdv_slang)
}

/// Compile with slang. `None` when slang is not linked.
pub fn compile(req: &Request) -> Option<Design> {
    #[cfg(fdv_slang)]
    {
        let json = call(ffi::fdv_slang_compile, req);
        Some(serde_json::from_str(&json).unwrap_or_else(|e| Design {
            ok: false,
            diagnostics: vec![Diagnostic {
                severity: "error".into(),
                message: format!("bad reply from the slang shim: {e}"),
                file: None,
                line: None,
                column: None,
            }],
            ..Default::default()
        }))
    }
    #[cfg(not(fdv_slang))]
    {
        let _ = req.words();
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(name: &str, text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fdv-slang-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn reports_tops_hierarchy_signals_and_errors() {
        if !available() {
            eprintln!("skipping: built without slang");
            return;
        }
        let f = write(
            "h.sv",
            "module leaf(input logic [3:0] a, output logic y); assign y = ^a; endmodule\n\
             module top(input logic clk, input logic [3:0] d, output logic p);\n  logic [7:0] cnt;\n  leaf u_leaf(.a(d), .y(p));\nendmodule\n",
        );
        let d = compile(&Request {
            files: vec![f.clone()],
            ..Default::default()
        })
        .unwrap();
        assert!(d.ok, "{:?}", d.diagnostics);
        assert_eq!(d.tops, ["top"]);
        assert!(d
            .instances
            .iter()
            .any(|i| i.path == "top.u_leaf" && i.module == "leaf"));
        assert_eq!(
            d.unknown_names(
                "top",
                "@(posedge clk) $past(cnt) == 8'hff |-> ##1 d[0] && typo"
            ),
            ["typo"]
        );
        let sigs = d.top_signals("top");
        assert!(
            sigs.contains(&("cnt", 8)) && sigs.contains(&("d", 4)),
            "{sigs:?}"
        );

        let bad = write(
            "bad.sv",
            "module m(input logic a);\n  logic y; assign y = a & nosuch;\nendmodule\n",
        );
        let d = compile(&Request {
            files: vec![bad],
            ..Default::default()
        })
        .unwrap();
        assert!(!d.ok);
        let e = d.errors().next().unwrap();
        assert_eq!(e.line, Some(2));
        assert!(e.message.contains("nosuch"), "{e}");
    }
}
