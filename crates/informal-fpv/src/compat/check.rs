//! Script check mode (`informal shell --check`): what a JasperGold or VC
//! Formal script would get from informal, without elaborating or proving.
//!
//! While a check runs, the shell's commands record here instead of
//! failing: a rejected command or option is an `error` finding, an option
//! that is accepted and dropped an `ignored` one, a command informal does
//! not have an `unknown` one, and any other Tcl error (an unset variable, a
//! missing file) a `tcl` one. Each finding carries the script line of the
//! top-level command it came from.

use serde::Serialize;
use std::cell::RefCell;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// informal has the command but rejects this use (an unsupported
    /// option or construct): the script would stop here.
    Error,
    /// The command name is not one informal implements.
    Unknown,
    /// Accepted, with no effect.
    Ignored,
    /// A Tcl error outside informal's commands.
    Tcl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub kind: Kind,
    pub file: String,
    pub line: u32,
    /// The command it is about (a vendor command name).
    pub command: String,
    pub message: String,
}

/// How often the script used one of informal's commands.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Uses {
    pub honoured: u32,
    pub rejected: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub commands: BTreeMap<String, Uses>,
}

impl Report {
    pub fn count(&self, kind: Kind) -> usize {
        self.findings.iter().filter(|f| f.kind == kind).count()
    }

    /// True when the script would run to its end (ignored options aside).
    pub fn clean(&self) -> bool {
        self.findings.iter().all(|f| f.kind == Kind::Ignored)
    }

    /// One line per finding (`file:line: kind: message`), then a summary.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for f in &self.findings {
            let kind = match f.kind {
                Kind::Error => "error",
                Kind::Unknown => "unknown command",
                Kind::Ignored => "ignored",
                Kind::Tcl => "tcl error",
            };
            out += &format!("{}:{}: {kind}: {}\n", f.file, f.line, f.message);
        }
        let (honoured, rejected) = self
            .commands
            .values()
            .fold((0, 0), |(h, r), u| (h + u.honoured, r + u.rejected));
        out += &format!(
            "{} command(s) honoured, {} rejected, {} unknown, {} ignored option(s) or command(s), {} Tcl error(s)\n",
            honoured,
            rejected,
            self.count(Kind::Unknown),
            self.count(Kind::Ignored),
            self.count(Kind::Tcl)
        );
        out
    }
}

#[derive(Default)]
struct State {
    file: String,
    line: u32,
    /// First word of the top-level command being evaluated.
    word: String,
    report: Report,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

pub fn active() -> bool {
    STATE.with(|s| s.borrow().is_some())
}

pub(super) fn start() {
    STATE.with(|s| *s.borrow_mut() = Some(State::default()));
}

pub(super) fn finish() -> Report {
    STATE.with(|s| s.borrow_mut().take().map(|s| s.report).unwrap_or_default())
}

/// The top-level command about to be evaluated.
pub(super) fn at(file: &str, line: u32, command: &str) {
    STATE.with(|s| {
        if let Some(st) = s.borrow_mut().as_mut() {
            st.file = file.to_string();
            st.line = line;
            st.word = command.split_whitespace().next().unwrap_or("").to_string();
        }
    });
}

pub(super) fn used(command: &str, ok: bool) {
    STATE.with(|s| {
        if let Some(st) = s.borrow_mut().as_mut() {
            let u = st.report.commands.entry(command.to_string()).or_default();
            if ok {
                u.honoured += 1;
            } else {
                u.rejected += 1;
            }
        }
    });
}

pub(super) fn finding(kind: Kind, command: &str, message: &str) {
    STATE.with(|s| {
        if let Some(st) = s.borrow_mut().as_mut() {
            let f = Finding {
                kind,
                file: st.file.clone(),
                line: st.line,
                command: command.to_string(),
                message: message.to_string(),
            };
            st.report.findings.push(f);
        }
    });
}

/// A Tcl error that ended a top-level command.
pub(super) fn tcl_error(message: &str) {
    let first = message.lines().next().unwrap_or("");
    match first.strip_prefix("invalid command name ") {
        Some(name) => {
            let name = name.trim_matches('"');
            finding(
                Kind::Unknown,
                name,
                &format!("{name}: not an informal command"),
            );
        }
        None => {
            let word = STATE.with(|s| s.borrow().as_ref().map(|s| s.word.clone()));
            finding(Kind::Tcl, &word.unwrap_or_default(), first);
        }
    }
}
