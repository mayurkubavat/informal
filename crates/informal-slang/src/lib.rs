//! SystemVerilog front end on slang, kept as a cross-check
//! (`INFORMAL_FRONTEND=slang`); svlang (`informal-svlang`) is the front end
//! (decision D14). This crate also defines the request and design types
//! both front ends share.
//!
//! slang checks the design and reports SystemVerilog diagnostics with file
//! and line, names the top modules, lists the instance tree and every signal
//! with its width, and lowers the design to Btor2 (`cpp/lower.cpp`).

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

/// One instance in the design browser's tree: where it is, what it
/// instantiates, its ports and named signals, and its child instances.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Scope {
    /// Hierarchical path, e.g. `fifo.u_wp`.
    pub path: String,
    /// Instance name (the last part of the path; the module name for a top).
    pub name: String,
    pub module: String,
    /// Ports first (declaration order), then variables, nets and
    /// parameters as slang visits them.
    pub signals: Vec<ScopeSignal>,
    pub children: Vec<Scope>,
}

/// A port or named signal of a [`Scope`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopeSignal {
    /// Name relative to the scope (`g[0].x` inside a generate block).
    pub name: String,
    pub width: u64,
    /// `port`, `variable`, `net` or `parameter`.
    pub kind: String,
    /// Ports only: `input`, `output`, `inout` or `ref`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

impl Scope {
    /// This scope and every scope below it, depth first.
    pub fn walk(&self) -> Vec<&Scope> {
        let mut out = vec![self];
        for c in &self.children {
            out.extend(c.walk());
        }
        out
    }
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
    /// The instance tree with each scope's ports and signals (the app's
    /// Design browser). One root per top instance.
    #[serde(default)]
    pub hierarchy: Vec<Scope>,
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
    /// [`lower`] only: append the named-output section for traces (see
    /// [`TRACE_MARKER`]). Not a slang option; the shim strips it.
    pub trace_outputs: bool,
    /// [`lower`] only: cut points (`stopat`), as full hierarchical paths
    /// from the top module (`top.u_core.sig`). Each named signal is cut
    /// from its drivers and is a free value on every step. The svlang
    /// front end applies them; the slang lowering rejects a non-empty list.
    pub cut_points: Vec<String>,
    /// [`lower`] only: black boxes, by module name and by instance path
    /// from the top module (`top.u_core.u_mem`). A boxed instance keeps its
    /// ports; nothing inside it is lowered and its outputs are free. svlang
    /// only, like [`Self::cut_points`].
    pub black_box_modules: Vec<String>,
    pub black_box_instances: Vec<String>,
}

/// The comment line that starts [`lower`]'s trace section: `output` lines
/// naming the combinational nets. Text before it is exactly the model
/// without [`Request::trace_outputs`], which is what engines get.
pub const TRACE_MARKER: &str = "; informal-trace-outputs";

/// Split a lowered model at [`TRACE_MARKER`]: (engine model, trace section).
pub fn split_trace(btor2: &str) -> (&str, &str) {
    match btor2.find(TRACE_MARKER) {
        Some(i) => btor2.split_at(i),
        None => (btor2, ""),
    }
}

impl Request {
    fn words(&self) -> Vec<String> {
        let mut w = Vec::new();
        if self.trace_outputs {
            w.push("--informal-trace-outputs".into());
        }
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

#[cfg(informal_slang)]
mod ffi {
    use std::ffi::c_char;
    extern "C" {
        pub fn informal_slang_lower(args: *const c_char) -> *mut c_char;
        pub fn informal_slang_compile(args: *const c_char) -> *mut c_char;
        pub fn informal_slang_free(p: *mut c_char);
    }
}

/// A design lowered to Btor2 by slang (see `cpp/lower.cpp`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Lowered {
    pub ok: bool,
    /// When not `ok`: true if the only error is a construct the lowering
    /// does not handle yet, false for compile errors and constructs it does
    /// not model. The session treats both as errors (no fallback, D14).
    #[serde(default)]
    pub fallback: bool,
    pub errors: Vec<String>,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub btor2: String,
}

#[cfg(informal_slang)]
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
        ffi::informal_slang_free(p);
        s
    }
}

/// Elaborate and lower the design to a Btor2 model: every labelled
/// immediate assertion becomes a `bad` line named by its label, and
/// immediate assumptions become constraints. `None` without slang.
pub fn lower(req: &Request) -> Option<Lowered> {
    #[cfg(informal_slang)]
    {
        if !(req.cut_points.is_empty()
            && req.black_box_modules.is_empty()
            && req.black_box_instances.is_empty())
        {
            return Some(Lowered {
                ok: false,
                errors: vec![
                    "not modelled: cut points and black boxes with the slang lowering".into(),
                ],
                ..Default::default()
            });
        }
        let json = call(ffi::informal_slang_lower, req);
        Some(serde_json::from_str(&json).unwrap_or_else(|e| Lowered {
            ok: false,
            errors: vec![format!("bad reply from the slang shim: {e}")],
            ..Default::default()
        }))
    }
    #[cfg(not(informal_slang))]
    {
        let _ = req;
        None
    }
}

/// True when this build links slang.
pub fn available() -> bool {
    cfg!(informal_slang)
}

/// Compile with slang. `None` when slang is not linked.
pub fn compile(req: &Request) -> Option<Design> {
    #[cfg(informal_slang)]
    {
        let json = call(ffi::informal_slang_compile, req);
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
    #[cfg(not(informal_slang))]
    {
        let _ = req.words();
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(name: &str, text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("informal-slang-{}", std::process::id()));
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

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn sig<'a>(s: &'a Scope, name: &str) -> &'a ScopeSignal {
        s.signals
            .iter()
            .find(|x| x.name == name)
            .unwrap_or_else(|| panic!("{} has no {name}: {:?}", s.path, s.signals))
    }

    #[test]
    fn hierarchy_of_the_counter() {
        if !available() {
            eprintln!("skipping: built without slang");
            return;
        }
        let d = compile(&Request {
            files: vec![root().join("tests/fpv/counter/counter.sv")],
            ..Default::default()
        })
        .unwrap();
        assert!(d.ok, "{:?}", d.diagnostics);
        let [top] = d.hierarchy.as_slice() else {
            panic!("{:?}", d.hierarchy)
        };
        assert_eq!(
            (top.path.as_str(), top.name.as_str(), top.module.as_str()),
            ("counter", "counter", "counter")
        );
        assert!(top.children.is_empty());
        let ports: Vec<_> = top
            .signals
            .iter()
            .filter(|s| s.kind == "port")
            .map(|s| (s.name.as_str(), s.dir.as_deref().unwrap(), s.width))
            .collect();
        assert_eq!(
            ports,
            [
                ("clk", "input", 1),
                ("rst_n", "input", 1),
                ("en", "input", 1),
                ("q", "output", 4),
                ("done", "output", 1)
            ]
        );
        // Internal signals follow the ports, each listed once.
        assert_eq!(sig(top, "nxt").width, 4);
        assert_eq!(sig(top, "nxt").dir, None);
        assert_eq!(top.signals.iter().filter(|s| s.name == "q").count(), 1);
        let json = serde_json::to_value(top).unwrap();
        assert_eq!(
            json["signals"][0],
            serde_json::json!({"name": "clk", "width": 1, "kind": "port", "dir": "input"})
        );
    }

    #[test]
    fn hierarchy_of_the_fifo() {
        if !available() {
            eprintln!("skipping: built without slang");
            return;
        }
        let d = compile(&Request {
            files: vec![root().join("tests/fpv/fifo/fifo.sv")],
            top: Some("fifo".into()),
            ..Default::default()
        })
        .unwrap();
        assert!(d.ok, "{:?}", d.diagnostics);
        let [top] = d.hierarchy.as_slice() else {
            panic!("{:?}", d.hierarchy)
        };
        assert_eq!(top.module, "fifo");
        let kids: Vec<_> = top
            .children
            .iter()
            .map(|c| (c.path.as_str(), c.name.as_str(), c.module.as_str()))
            .collect();
        assert_eq!(
            kids,
            [("fifo.u_wp", "u_wp", "ptr"), ("fifo.u_rp", "u_rp", "ptr")]
        );
        assert_eq!(top.walk().len(), 3);
        // Parameterised widths are elaborated: DEPTH_LOG2 = 2, DW = 8.
        assert_eq!(
            (sig(top, "din").width, sig(top, "din").dir.as_deref()),
            (8, Some("input"))
        );
        assert_eq!(
            (sig(top, "status").width, sig(top, "status").dir.as_deref()),
            (2, Some("output"))
        );
        assert_eq!(
            (sig(top, "count").width, sig(top, "count").kind.as_str()),
            (3, "variable")
        );
        assert_eq!(sig(top, "mem").width, 32);
        assert_eq!(sig(top, "DEPTH").kind, "parameter");
        let wp = &top.children[0];
        assert_eq!(
            (sig(wp, "p").width, sig(wp, "p").dir.as_deref()),
            (2, Some("output"))
        );
        assert_eq!(sig(wp, "W").kind, "parameter");
    }

    #[test]
    fn trace_outputs_only_append_named_nets() {
        if !available() {
            eprintln!("skipping: built without slang");
            return;
        }
        let f = write(
            "t.sv",
            "module t(input logic clk, input logic en, output logic done);\n\
             logic [1:0] q; logic [1:0] nxt; logic unused;\n\
             assign nxt = q + 2'd1;\n  assign done = q == 2'd3;\n  assign unused = en ^ q[0];\n\
             always_ff @(posedge clk) if (en && !done) q <= nxt;\n\
             always_comb begin a_ok: assert (q <= 2'd3); end\nendmodule\n",
        );
        let mut req = Request {
            files: vec![f],
            ..Default::default()
        };
        let plain = lower(&req).unwrap();
        assert!(plain.ok, "{:?}", plain.errors);
        assert!(!plain.btor2.contains(TRACE_MARKER) && !plain.btor2.contains(" output "));
        req.trace_outputs = true;
        let traced = lower(&req).unwrap();
        assert!(traced.ok, "{:?}", traced.errors);
        let (model, section) = split_trace(&traced.btor2);
        assert_eq!(model, plain.btor2, "the engine part must not change");
        let outs: Vec<&str> = section
            .lines()
            .filter(|l| l.split_whitespace().nth(1) == Some("output"))
            .filter_map(|l| l.split_whitespace().nth(3))
            .collect();
        // Sorted; `unused` is read by nothing and lowered for the trace.
        assert_eq!(outs, ["t.done", "t.nxt", "t.unused"]);
        assert!(section.starts_with(TRACE_MARKER));
    }
}
