//! What the session and the front end exchange: the request for a compile
//! or a model, the design as found (diagnostics, hierarchy, signals,
//! properties) and the lowered model.

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
    /// parameters as declared.
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

/// What the front end found in a design.
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
    /// The properties written in the RTL: the concurrent ones, and the
    /// immediate assertions that have a label of their own (empty when the
    /// design cannot be lowered: see `not_lowered`).
    #[serde(default)]
    pub properties: Vec<RtlProperty>,
    /// Why the design has no model, when it compiles and does not lower:
    /// one line for each construct the front end does not model.
    #[serde(default)]
    pub not_lowered: Vec<String>,
    /// Immediate assertions and covers that cannot be listed as properties,
    /// with the reason (no label, or a label used more than once). They are
    /// not checked; saying so is the caller's job.
    #[serde(default)]
    pub unlisted_checks: Vec<String>,
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
            "posedge",
            "negedge",
            "edge",
            "disable",
            "iff",
            "or",
            "and",
            "not",
            "if",
            "else",
            "inside",
            "dist",
            "throughout",
            "within",
            "intersect",
            "first_match",
            "until",
            "until_with",
            "s_until",
            "s_until_with",
            "s_eventually",
            "eventually",
            "nexttime",
            "s_nexttime",
            "always",
            "s_always",
            "strong",
            "weak",
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

/// What to compile, and for a model how.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub files: Vec<PathBuf>,
    pub defines: Vec<(String, Option<String>)>,
    pub incdirs: Vec<PathBuf>,
    pub top: Option<String>,
    pub params: Vec<(String, String)>,
    /// Model only: append the named-output section for traces (see
    /// [`TRACE_MARKER`]).
    pub trace_outputs: bool,
    /// Model only: cut points (`stopat`), as full hierarchical paths from
    /// the top module (`top.u_core.sig`). Each named signal is cut from its
    /// drivers and is a free value on every step.
    pub cut_points: Vec<String>,
    /// Model only: black boxes, by module name and by instance path from
    /// the top module (`top.u_core.u_mem`). A boxed instance keeps its
    /// ports; nothing inside it is lowered and its outputs are free.
    pub black_box_modules: Vec<String>,
    pub black_box_instances: Vec<String>,
    /// Model only: write each cover as a `bad` line that is reached when
    /// the cover is hit. The session does, and tells covers from assertions
    /// by [`Lowered::properties`].
    pub cover_targets: bool,
    /// Model only: the declared clocks, each a top-level input of the top
    /// module: its name, the model steps it stays at one level, and the
    /// steps its wave is moved later. A design on one edge of one clock
    /// ignores them.
    pub clocks: Vec<(String, u32, u32)>,
    /// Model only: properties given by command, which the front end binds
    /// in the top module's scope and compiles: the name of the check, and
    /// the whole directive (`assert property (@(posedge clk) req |->
    /// ##[1:3] gnt)`). Each comes back in [`Lowered::properties`] under its
    /// name.
    pub properties: Vec<(String, String)>,
    /// Model only: the "in reset" condition, an expression in the top
    /// module's scope (`!rst_n`). The model holds it true at the start of
    /// every trace and false after: for the first model step when one step
    /// is a clock cycle, and for [`Self::tick`]'s steps when the model
    /// drives the clocks. Attempts of a property that start in reset are
    /// not checked.
    pub reset: Option<String>,
    /// Model only: the reset's length for a model that drives the clocks
    /// (a design on both clock edges, several clocks or computed clocks).
    pub tick: TickReset,
    /// Leave out the immediate assertions in `initial` blocks that check a
    /// signal at time zero (OpenTitan's `ASSERT_INIT_NET`), which are
    /// otherwise not supported. Only those checks are not made; the
    /// design's behaviour is the same. Off unless the user asks.
    pub skip_initial_checks: bool,
    /// Lower `$isunknown(x)` as 0, since the model has no unknown value;
    /// it is otherwise not supported. A check such as `assert
    /// (!$isunknown(x))` then always holds and says nothing about the
    /// design. Off unless the user asks.
    pub isunknown_is_false: bool,
    /// Model only: also build a precondition-cover model (`<name>:precondition`)
    /// for assertions with antecedents (`|->`, `|=>`), returned in
    /// [`Lowered::preconditions_btor2`] and [`Lowered::preconditions`] so
    /// the primary [`Lowered::btor2`] stays free of extra cover logic.
    pub precondition_covers: bool,
}

/// How long the reset lasts in a model that drives the clocks, where one
/// model step is half a period of the fastest clock.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TickReset {
    /// Model steps the reset is held for.
    pub steps: u32,
    /// Clock cycles left out of the properties on a clock and edge with no
    /// entry in [`Self::by_clock`].
    pub cycles: u32,
    /// For a declared clock and edge (clock input, rising edge?): how many
    /// of its edges fall in the reset, so that a property on it skips
    /// exactly the attempts that start there.
    pub by_clock: Vec<(String, bool, u32)>,
}

/// A property in a model: one written in the RTL (`assert property`,
/// `assume property`, `cover property` inside a module), or one given by
/// command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RtlProperty {
    /// Full hierarchical name (`top.u_fifo.a_no_overflow`), also the name of
    /// its `bad` line in the model (assertions and covers).
    pub name: String,
    /// `assert`, `assume` or `cover`.
    pub kind: String,
    /// Why it is not in the model, when it uses an operator the front end
    /// does not compile yet.
    pub unsupported: Option<String>,
    /// Why it is not in the model, when it does not bind in the design (a
    /// property given by command that names an unknown signal, say).
    #[serde(default)]
    pub error: Option<String>,
    /// A liveness property (`req |-> s_eventually gnt`): its target in the
    /// model is a `justice` line, not a `bad`.
    #[serde(default)]
    pub liveness: bool,
    /// The property as written in the RTL: what stands between the
    /// parentheses of `assert property ( ... )`, on one line. `None` when
    /// it cannot be read from the source (code from a macro, say).
    #[serde(default)]
    pub text: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
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

/// A design lowered to Btor2.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Lowered {
    /// When false, `errors` says why: a compile error, or a construct the
    /// front end does not model (never a guess, decision D13).
    pub ok: bool,
    pub errors: Vec<String>,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub btor2: String,
    /// The concurrent properties written in the RTL, then those given in
    /// [`Request::properties`].
    #[serde(default)]
    pub properties: Vec<RtlProperty>,
    /// The design uses both edges of its clock, several clocks or computed
    /// clocks, so the model drives the clocks and one model step is half a
    /// period of the fastest one.
    #[serde(default)]
    pub half_cycle: bool,
    /// Precondition cover targets (`<name>:precondition`) emitted when
    /// [`Request::precondition_covers`] is true. A target listed here that
    /// has no `bad` line in [`Self::preconditions_btor2`] was folded to
    /// constant false by elaboration (unreachable at time zero).
    #[serde(default)]
    pub preconditions: Vec<String>,
    /// Btor2 model holding `<name>:precondition` `bad` targets alongside all
    /// active assumptions and reset, when [`Request::precondition_covers`]
    /// is true.
    #[serde(default)]
    pub preconditions_btor2: String,
}
