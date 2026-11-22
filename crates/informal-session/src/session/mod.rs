//! An FPV session: the design setup, clocks, reset and properties that
//! JasperGold and VC Formal scripts build up command by command, and the
//! prove step that turns them into engine runs.
//!
//! Elaboration runs svlang (the pure-Rust SystemVerilog front end, crate
//! `informal-svlang`) over the design: SystemVerilog errors come back with
//! file and line, the top module is taken from it, and property expressions
//! are checked against the names the top module declares. svlang lowers the
//! whole design to one Btor2 model (decision D14), binding each property
//! given by command in the top module's scope, so it sees every signal
//! there, and compiling it with the properties written in the RTL; the
//! reset is part of the model too. Each assertion or cover gets its own
//! copy with only its target line and all assumptions, and runs through
//! the engine portfolio on its own.

mod elaborate;
mod orchestrate;
mod prove;
mod tasks;
mod types;
mod vacuity_env;

pub use types::{
    Clock, Design, Effort, Environment, Kind, Orchestration, Property, Session, SourceLoc, Status,
    Task, Vacuity, EMBEDDED,
};

use crate::trace;
use informal_engine::{CheckRequest, Engine, ModelFormat, Verdict};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

impl Session {
    pub fn new(engines: Vec<Arc<dyn Engine>>, workdir: PathBuf) -> Self {
        // Absolute, so that paths written into it stay valid.
        let workdir = std::path::absolute(&workdir).unwrap_or(workdir);
        Session {
            design: Design::default(),
            elaborated: false,
            clocks: Vec::new(),
            reset: None,
            props: Vec::new(),
            tasks: vec![Task::new(EMBEDDED)],
            current_task: EMBEDDED.to_string(),
            time_limit: Duration::from_secs(3600),
            total_time_limit: None,
            effort: Effort::Standard,
            orchestration: Orchestration::Auto,
            max_trace_length: None,
            engine_filter: Vec::new(),
            workdir,
            engines,
            elab: None,
            compiled: None,
            hierarchy: Vec::new(),
            frontend: String::new(),
            stop: Arc::default(),
            dry_run: false,
            sink: None,
            trace_reruns: Arc::default(),
            certificates: None,
            environment_depth: 20,
            environment: None,
            preconditions: true,
            witnesses: false,
            prove_cache: false,
            prove_cache_dir: None,
            prove_cache_mode: crate::cache::CacheMode::default(),
            cov: crate::cov::CovState::default(),
            xprop: crate::xprop::XpropState::default(),
            spv: Vec::new(),
            sec: crate::sec::SecState::default(),
            lp: crate::lp::LpState::default(),
            assumed_from_assert: Vec::new(),
            counter_abstractions: Vec::new(),
            prove_bound: None,
            with_proof_core: false,
        }
    }

    /// `clear -all`: forget the design and properties, keep engines (and
    /// the front end's stop flag and progress sink).
    pub fn clear(&mut self) {
        let engines = std::mem::take(&mut self.engines);
        let workdir = std::mem::take(&mut self.workdir);
        let (stop, sink, dry_run) = (self.stop.clone(), self.sink.take(), self.dry_run);
        let certificates = self.certificates.take();
        let (depth, pre, wit) = (self.environment_depth, self.preconditions, self.witnesses);
        let (pc, pc_dir, pc_mode) = (
            self.prove_cache,
            self.prove_cache_dir.take(),
            self.prove_cache_mode,
        );
        *self = Session::new(engines, workdir);
        self.certificates = certificates;
        self.environment_depth = depth;
        self.preconditions = pre;
        self.witnesses = wit;
        self.prove_cache = pc;
        self.prove_cache_dir = pc_dir;
        self.prove_cache_mode = pc_mode;
        self.dry_run = dry_run;
        self.stop = stop;
        self.sink = sink;
    }

    /// Write property `i`'s trace as `<workdir>/<name>.vcd` from the
    /// winner's named witness (informal's own engines, and external engines
    /// whose counterexample informal converts: rIC3 `--cex`, ABC `write_cex`) or,
    /// when the winner gave none or it does not replay, from a BMC re-run to
    /// the reported depth that only serves to produce one. Returns the path
    /// and the trace's depth. `Err(Some(why))` when no counterexample could
    /// be replayed on the model (the caller withdraws the verdict);
    /// `Err(None)` when one was but its file could not be written (logged;
    /// the verdict stands).
    pub(crate) fn write_trace(
        &self,
        name: &str,
        model: &Path,
        witness: Option<&str>,
        depth: Option<u32>,
    ) -> Result<(String, usize), Option<String>> {
        let clock = self.clocks.first().map_or("clk", |c| c.signal.as_str());
        // Replay on the copy with named combinational nets when there is one.
        let with_nets = trace_model(model);
        let replay = if with_nets.is_file() {
            with_nets.as_path()
        } else {
            model
        };
        let from_winner = witness.and_then(trace::parse_witness).and_then(|w| {
            match trace::vcd_from_witness(replay, &w, name, clock) {
                Ok(v) => Some((v, w.depth)),
                Err(e) => {
                    self.log(&format!("{name}: the engine's witness does not replay ({e:#}); re-running BMC for the trace"));
                    None
                }
            }
        });
        let (vcd, trace_depth) = match from_winner {
            Some(t) => t,
            None => {
                self.trace_reruns.fetch_add(1, Ordering::SeqCst);
                if witness.is_none() {
                    self.note(&format!(
                        "{name}: the winning engine gave no witness; tracing with a BMC re-run"
                    ));
                }
                let w = match self.trace_bmc(model, depth) {
                    Ok(w) => w,
                    Err(e) => return Err(Some(format!("{e:#}"))),
                };
                match trace::vcd_from_witness(replay, &w, name, clock) {
                    Ok(v) => (v, w.depth),
                    Err(e) => return Err(Some(format!("{e:#}"))),
                }
            }
        };
        let path = self.workdir.join(trace::file_name(name));
        if let Err(e) = std::fs::write(&path, vcd) {
            self.log(&format!("{name}: writing {}: {e}", path.display()));
            return Err(None);
        }
        let shown = std::env::current_dir()
            .ok()
            .and_then(|d| path.strip_prefix(d).ok().map(Path::to_path_buf))
            .unwrap_or(path);
        Ok((shown.display().to_string(), trace_depth))
    }

    /// A witness from informal-bmc on `model`, searching up to `depth` (64
    /// frames when the winner did not say), within 30 s or the prove time
    /// limit if shorter.
    fn trace_bmc(&self, model: &Path, depth: Option<u32>) -> anyhow::Result<trace::Witness> {
        let bmc = informal_bmc::Bmc {
            max_depth: Some(depth.unwrap_or(64) as usize),
            step: 1,
        };
        let req = CheckRequest::new(
            model.to_path_buf(),
            ModelFormat::Btor2,
            self.time_limit.min(Duration::from_secs(30)),
        );
        let r = bmc.check(&req, &informal_engine::RunContext::new(req.timeout))?;
        match (
            &r.verdict,
            r.witness.as_deref().and_then(trace::parse_witness),
        ) {
            (Verdict::Unsafe { .. }, Some(w)) => Ok(w),
            (Verdict::Unsafe { .. }, None) => anyhow::bail!("informal-bmc gave no named witness"),
            (v, _) => anyhow::bail!("informal-bmc could not reproduce it ({})", v.label()),
        }
    }

    /// An informational console line (the app's sink only).
    pub(super) fn note(&self, line: &str) {
        if let Some(s) = &self.sink {
            s.line(line, false);
        }
    }

    /// A line for the console: the app's sink when there is one, else stderr.
    #[doc(hidden)]
    pub fn log(&self, line: &str) {
        match &self.sink {
            Some(s) => s.line(line, true),
            None => eprintln!("informal: {line}"),
        }
    }

    /// One row per property of the table `report` prints: name, type,
    /// status, engine and detail.
    pub fn report_rows(&self) -> Vec<[String; 5]> {
        self.report_rows_of(None)
    }

    /// [`Self::report_rows`] of one task's properties, or of all.
    pub fn report_rows_of(&self, task: Option<&str>) -> Vec<[String; 5]> {
        let mut rows = Vec::new();
        for p in self
            .props
            .iter()
            .filter(|p| task.is_none_or(|t| p.task == t))
        {
            let (engine, detail) = match &p.status {
                Status::Proven { engine, ms } | Status::Unreachable { engine, ms } => {
                    (engine.clone(), format!("{:.2} s", *ms as f64 / 1000.0))
                }
                Status::BoundedProven { engine, bound, ms } => (
                    engine.clone(),
                    format!("bound {bound} in {:.2} s", *ms as f64 / 1000.0),
                ),
                Status::Cex { engine, depth, ms } | Status::Covered { engine, depth, ms } => (
                    engine.clone(),
                    format!(
                        "depth {} in {:.2} s",
                        depth.map_or("?".into(), |d| d.to_string()),
                        *ms as f64 / 1000.0
                    ),
                ),
                Status::Undetermined { reason } => (String::new(), reason.clone()),
                Status::Error { message } => {
                    (String::new(), message.lines().next().unwrap_or("").into())
                }
                Status::Unprocessed => (String::new(), String::new()),
            };
            let status = if p.enabled {
                p.status.word()
            } else {
                "disabled"
            };
            // Said in the table: what an AI added, and which results
            // stand on an AI-added assumption or cut point.
            let detail = match p.vacuity {
                Some(v) => format!("{detail} [{}]", v.word()),
                None => detail,
            };
            let detail = match (p.ai, p.under_ai.is_empty()) {
                (true, _) => format!("{detail} [AI-added]").trim_start().to_string(),
                (false, false) => format!("{detail} [under AI-added: {}]", p.under_ai.join(", ")),
                (false, true) => detail,
            };
            rows.push([
                p.name.clone(),
                p.kind.label().to_string(),
                status.to_string(),
                engine,
                detail,
            ]);
        }
        rows
    }

    /// A JasperGold-style summary table.
    pub fn report(&self) -> String {
        self.report_of(None)
    }

    /// [`Self::report`] of one task's properties, or of all.
    pub fn report_of(&self, task: Option<&str>) -> String {
        let mut s = format!(
            "{:<28} {:<7} {:<13} {:<10} {}\n",
            "property", "type", "status", "engine", "detail"
        );
        for [name, kind, status, engine, detail] in self.report_rows_of(task) {
            s += &format!("{name:<28} {kind:<7} {status:<13} {engine:<10} {detail}\n");
        }
        s
    }

    /// The same table as comma-separated values (`report -csv`).
    pub fn report_csv(&self) -> String {
        self.report_csv_of(None)
    }

    /// [`Self::report_csv`] of one task's properties, or of all.
    pub fn report_csv_of(&self, task: Option<&str>) -> String {
        let field = |f: &str| {
            if f.contains([',', '"', '\n']) {
                format!("\"{}\"", f.replace('"', "\"\""))
            } else {
                f.to_string()
            }
        };
        let mut s = String::from("property,type,status,engine,detail\n");
        for row in self.report_rows_of(task) {
            s += &row.iter().map(|f| field(f)).collect::<Vec<_>>().join(",");
            s.push('\n');
        }
        s
    }
}

/// The Btor2 text with every `bad` line dropped except the one named `keep`.
/// `prop_N.trace.btor2` next to `prop_N.btor2`: the property's model with
/// the slang lowering's named-output section, used only to replay traces.
pub(super) fn trace_model(model: &Path) -> PathBuf {
    model.with_extension("trace.btor2")
}

/// The name of the property a model line is the target of: a `bad` line
/// (`<id> bad <node> <name>`) or a `justice` line (`<id> justice <n>
/// <nodes> <name>`, a liveness property). `None` for any other line, and
/// `Some("")` for a target with no name.
pub(super) fn target_of(line: &str) -> Option<(&str, &str)> {
    let w: Vec<&str> = line.split_whitespace().collect();
    match w.get(1).copied() {
        Some("bad") => Some(("bad", w.get(3).copied().unwrap_or(""))),
        Some("justice") => {
            let n: usize = w.get(2)?.parse().ok()?;
            Some(("justice", w.get(3 + n).copied().unwrap_or("")))
        }
        _ => None,
    }
}

/// True when the model has a target (see [`target_of`]) named `name`.
pub(super) fn has_target(btor: &str, name: &str) -> bool {
    btor.lines()
        .any(|l| target_of(l).is_some_and(|(_, n)| n == name))
}

/// Drop named `constraint` and `fair` lines whose symbol is in `drop_syms`.
pub(crate) fn drop_assumptions(btor: &str, drop_syms: &[String]) -> String {
    btor.lines()
        .filter(|l| {
            let mut w = l.split_whitespace();
            let _id = w.next();
            match w.next() {
                Some("constraint" | "fair") => {
                    let _arg = w.next();
                    match w.next() {
                        Some(sym) => !drop_syms.iter().any(|d| d == sym),
                        None => true,
                    }
                }
                _ => true,
            }
        })
        .flat_map(|l| [l, "\n"])
        .collect()
}

/// The model with `keep` as its only target. Fairness lines only matter to
/// a liveness target, so they go with the others when `keep` is a `bad`
/// (an external engine may not read them).
pub(crate) fn only_target(btor: &str, keep: &str) -> String {
    let liveness = btor
        .lines()
        .any(|l| target_of(l) == Some(("justice", keep)));
    btor.lines()
        .filter(|l| match target_of(l) {
            Some((_, name)) => name == keep,
            None => liveness || l.split_whitespace().nth(1) != Some("fair"),
        })
        .flat_map(|l| [l, "\n"])
        .collect()
}

/// A property's full name in `task`: `<task>::<name>`, or the name alone
/// in the embedded task.
pub fn qualify(task: &str, name: &str) -> String {
    match task {
        EMBEDDED => name.to_string(),
        t => format!("{t}::{name}"),
    }
}

/// A property's name as its target line in the model has it: a Btor2
/// symbol has no white space.
pub(crate) fn target_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect()
}

/// Operators the front end's property compiler does not take, checked
/// when a property is given so that the command fails and not the prove.
/// (It takes bounded and unbounded sequences, goto `[->]` and
/// non-consecutive `[=]` repetition, `strong`, `not`, `#-#` / `#=#`,
/// `accept_on` / `reject_on` / `sync_accept_on` / `sync_reject_on`, the
/// `until` family, `s_eventually`, and bounded `nexttime`, `eventually`
/// and `always`.)
const NOT_COMPILED: &[&str] = &[];

/// `Err` for a property text that is empty or uses one of [`NOT_COMPILED`].
pub(super) fn check_operators(expr: &str) -> Result<(), String> {
    if expr.trim().is_empty() {
        return Err("empty property".into());
    }
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    // The operator as a whole word where it is one (`until` in `s_until`
    // or in a signal name is not it).
    let used = |word: &str| {
        expr.match_indices(word).any(|(i, _)| {
            let before = expr[..i].chars().next_back();
            let after = expr[i + word.len()..].chars().next();
            let (first, last) = (word.chars().next(), word.chars().next_back());
            (!first.is_some_and(ident) || !before.is_some_and(ident))
                && (!last.is_some_and(ident) || !after.is_some_and(ident))
        })
    };
    match NOT_COMPILED.iter().find(|w| used(w)) {
        Some(w) => Err(format!("`{}` is not supported yet in a property", w.trim())),
        None => Ok(()),
    }
}

/// Tcl `string match` style glob.
pub fn glob(pat: &str, s: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some(b'*'), _) => go(&p[1..], s) || (!s.is_empty() && go(p, &s[1..])),
            (Some(b'?'), Some(_)) => go(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &s[1..]),
            _ => false,
        }
    }
    go(pat.as_bytes(), s.as_bytes())
}

/// `1h`, `30m`, `90s`, `500ms`, `2H`, or plain seconds.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, unit) = s.split_at(
        s.find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(s.len()),
    );
    let n: f64 = num.parse().map_err(|_| format!("bad time `{s}`"))?;
    let secs = match unit.to_ascii_lowercase().as_str() {
        "" | "s" => n,
        "ms" => n / 1000.0,
        "m" => n * 60.0,
        "h" => n * 3600.0,
        "d" => n * 86400.0,
        _ => return Err(format!("bad time unit in `{s}`")),
    };
    Ok(Duration::from_secs_f64(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_and_durations() {
        assert!(glob("a_*", "a_le9"));
        assert!(!glob("a_?", "a_le9"));
        assert_eq!(parse_duration("1h").unwrap().as_secs(), 3600);
        assert_eq!(parse_duration("30M").unwrap().as_secs(), 1800);
        assert_eq!(parse_duration("45").unwrap().as_secs(), 45);
        assert!(parse_duration("3x").is_err());
    }
}
