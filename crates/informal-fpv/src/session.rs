//! An FPV session: the design setup, clocks, reset and properties that
//! JasperGold and VC Formal scripts build up command by command, and the
//! prove step that turns them into engine runs.
//!
//! Elaboration runs svlang (the pure-Rust SystemVerilog front end, crate
//! `informal-svlang`) over the design: SystemVerilog errors come back with
//! file and line, the top module is taken from it, and property expressions
//! are checked against the names the top module declares. Properties are
//! lowered by [`crate::sva`] and injected as immediate assertions at the end
//! of the top module, so they see every signal in its scope; svlang then
//! lowers the whole design to one Btor2 model (decision D14). Each assertion
//! or cover gets its own copy with only its `bad` line and all assumptions,
//! and runs through the engine portfolio on its own. `INFORMAL_FRONTEND=slang`
//! uses the older slang lowering instead, as a cross-check.

use crate::progress::{ProveSink, Tracker};
use crate::sva;
use anyhow::{anyhow, bail, Context};
use informal_engine::portfolio::EngineRun;
use informal_engine::{portfolio, CheckRequest, Engine, ModelFormat, Progress, Verdict};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use crate::trace;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Assert,
    Assume,
    Cover,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Assert => "assert",
            Kind::Assume => "assume",
            Kind::Cover => "cover",
        }
    }
}

/// Result of proving one property, in the vendor tools' vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Status {
    Unprocessed,
    Proven {
        engine: String,
        ms: u128,
    },
    Cex {
        engine: String,
        depth: Option<u32>,
        ms: u128,
    },
    Covered {
        engine: String,
        depth: Option<u32>,
        ms: u128,
    },
    Unreachable {
        engine: String,
        ms: u128,
    },
    Undetermined {
        reason: String,
    },
    Error {
        message: String,
    },
}

impl Status {
    /// The short word JasperGold prints and `-include {status ...}` filters on.
    pub fn word(&self) -> &'static str {
        match self {
            Status::Unprocessed => "unprocessed",
            Status::Proven { .. } => "proven",
            Status::Cex { .. } => "cex",
            Status::Covered { .. } => "covered",
            Status::Unreachable { .. } => "unreachable",
            Status::Undetermined { .. } => "undetermined",
            Status::Error { .. } => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Property {
    pub name: String,
    pub kind: Kind,
    pub expr: String,
    pub enabled: bool,
    pub status: Status,
    /// The VCD written for a CEX or cover (`<workdir>/<name>.vcd`, relative
    /// to the working directory when inside it); `None` otherwise or when
    /// no witness could be replayed. See docs/ui/trace-format.md.
    pub trace: Option<String>,
    /// Where the property was written: the script line of the Tcl command
    /// that created it, when that ran from a sourced file. `None` for
    /// properties typed at the console.
    pub source: Option<SourceLoc>,
    /// The last prove's per-engine record: each portfolio engine's outcome
    /// (won/lost/timeout/killed/error), time and depth reached. Empty
    /// before a prove, or when no engine ran (elaboration, errors).
    pub engines: Vec<EngineRun>,
}

/// A file and 1-based line (absolute path).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceLoc {
    pub file: String,
    pub line: u32,
}

impl SourceLoc {
    /// Absolute form of a path as Tcl saw it (relative to the process's
    /// working directory).
    pub fn new(file: &str, line: u32) -> Self {
        let file = std::path::absolute(file)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| file.to_string());
        SourceLoc { file, line }
    }

    /// Fallback when Tcl has no line for the command (it was built with
    /// `eval` or `uplevel` from a string): the first line of `file` that
    /// mentions the property name as a whole word.
    pub fn search(file: &str, name: &str) -> Option<Self> {
        let text = std::fs::read_to_string(file).ok()?;
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let line = text.lines().position(|l| {
            l.match_indices(name).any(|(i, _)| {
                !l[..i].chars().next_back().is_some_and(is_word)
                    && !l[i + name.len()..].chars().next().is_some_and(is_word)
            })
        })?;
        Some(SourceLoc::new(file, line as u32 + 1))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Clock {
    pub signal: String,
    pub edge: &'static str,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Design {
    pub files: Vec<PathBuf>,
    pub defines: Vec<(String, Option<String>)>,
    pub incdirs: Vec<PathBuf>,
    pub top: Option<String>,
    pub params: Vec<(String, String)>,
    /// Modules to black-box (accepted for compatibility; not applied yet).
    pub bbox: Vec<String>,
    /// Cut points (`stopat`), as paths from the top module (`u_core.sig`):
    /// each is cut from its drivers and free on every step.
    pub stopats: Vec<String>,
    /// slang's scope tree after elaboration: instances with their ports and
    /// named signals (the app's Design browser). Also written to
    /// `<workdir>/design.hierarchy.json`. Empty without slang (the app then
    /// scans the top file's port list).
    pub hierarchy: Vec<informal_slang::Scope>,
}

#[derive(Serialize)]
pub struct Session {
    pub design: Design,
    pub elaborated: bool,
    pub clocks: Vec<Clock>,
    /// Active-high "in reset" condition, held for the first cycle only.
    pub reset: Option<String>,
    pub props: Vec<Property>,
    pub time_limit: Duration,
    /// Engine names to use; empty means every loaded engine.
    pub engine_filter: Vec<String>,
    pub workdir: PathBuf,
    #[serde(skip)]
    pub engines: Vec<Arc<dyn Engine>>,
    /// slang's view of the design, after elaboration (when linked).
    #[serde(skip)]
    pub slang: Option<informal_slang::Design>,
    /// Instance tree from slang, for the UI.
    pub hierarchy: Vec<informal_slang::Instance>,
    /// Front end that built the last prove's models (`svlang` or `slang`).
    pub frontend: String,
    /// Set from another thread (the app's Stop) to end a running prove:
    /// checked between properties and forwarded to the engine portfolio,
    /// whose external engines then kill their process groups. Cleared when
    /// a prove starts.
    #[serde(skip)]
    pub stop: Arc<AtomicBool>,
    /// Script check mode (`informal shell --check`): commands are parsed and
    /// recorded, but nothing is elaborated or proved.
    #[serde(skip)]
    pub dry_run: bool,
    /// Receives live progress while a prove runs (the desktop app).
    #[serde(skip)]
    pub sink: Option<Arc<dyn ProveSink>>,
    /// BMC re-runs made only to get a trace (the winner gave no usable
    /// witness). Tests and diagnostics read it.
    #[serde(skip)]
    pub trace_reruns: Arc<std::sync::atomic::AtomicUsize>,
}

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
            time_limit: Duration::from_secs(3600),
            engine_filter: Vec::new(),
            workdir,
            engines,
            slang: None,
            hierarchy: Vec::new(),
            frontend: String::new(),
            stop: Arc::default(),
            dry_run: false,
            sink: None,
            trace_reruns: Arc::default(),
        }
    }

    /// `clear -all`: forget the design and properties, keep engines (and
    /// the front end's stop flag and progress sink).
    pub fn clear(&mut self) {
        let engines = std::mem::take(&mut self.engines);
        let workdir = std::mem::take(&mut self.workdir);
        let (stop, sink, dry_run) = (self.stop.clone(), self.sink.take(), self.dry_run);
        *self = Session::new(engines, workdir);
        self.dry_run = dry_run;
        self.stop = stop;
        self.sink = sink;
    }

    pub fn add_property(
        &mut self,
        kind: Kind,
        name: Option<String>,
        expr: &str,
    ) -> Result<String, String> {
        sva::parse(expr)?; // reject what the lowering can't handle up front
        self.check_names(expr)?;
        let name = name.unwrap_or_else(|| {
            let n = self.props.iter().filter(|p| p.kind == kind).count();
            format!("{}_{}", kind.label(), n)
        });
        if self.props.iter().any(|p| p.name == name) {
            return Err(format!("property `{name}` already exists"));
        }
        self.props.push(Property {
            name: name.clone(),
            kind,
            expr: expr.to_string(),
            enabled: true,
            status: Status::Unprocessed,
            trace: None,
            source: None,
            engines: Vec::new(),
        });
        Ok(name)
    }

    /// With slang's view of the design, reject names the top module does
    /// not declare (typos would otherwise become implicit nets that make a
    /// property vacuous).
    fn check_names(&self, expr: &str) -> Result<(), String> {
        if let (Some(d), Some(top)) = (&self.slang, &self.design.top) {
            let unknown = d.unknown_names(top, expr);
            if !unknown.is_empty() {
                return Err(format!(
                    "unknown name(s) in module {top}: {}",
                    unknown.join(", ")
                ));
            }
        }
        Ok(())
    }

    /// With slang's view of the design, reject a reset expression or clock
    /// that names a signal the top module does not declare. (Unchecked, a
    /// typo would leave the real reset free and give a wrong verdict.)
    pub(crate) fn check_reset_and_clocks(&self) -> Result<(), String> {
        if let Some(r) = &self.reset {
            self.check_names(r)
                .map_err(|e| format!("reset {{{r}}}: {e}"))?;
        }
        for c in &self.clocks {
            self.check_names(&c.signal)
                .map_err(|e| format!("clock {}: {e}", c.signal))?;
        }
        Ok(())
    }

    /// `stopat`: the path of a signal to cut, from the top module (a
    /// leading `<top>.` is accepted). After elaboration the name must be a
    /// signal of the design; before it, the check happens at prove.
    pub fn cut_point(&self, name: &str) -> Result<String, String> {
        let top = self.design.top.as_deref().unwrap_or("");
        let rel = name
            .strip_prefix(top)
            .and_then(|r| r.strip_prefix('.'))
            .filter(|_| !top.is_empty())
            .unwrap_or(name);
        if let (Some(d), false) = (&self.slang, top.is_empty()) {
            let full = format!("{top}.{rel}");
            // A whole signal, or a part of one (`pkt.valid`, `mem[3]`).
            let known = d.signals.iter().filter(|s| !s.constant).any(|s| {
                full.strip_prefix(s.path.as_str())
                    .is_some_and(|r| r.is_empty() || r.starts_with(['.', '[']))
            });
            if !known {
                return Err(format!("`{name}` is not a signal of {top}"));
            }
        }
        Ok(rel.to_string())
    }

    /// The cut points as the front end wants them: full paths.
    fn cut_points(&self) -> Vec<String> {
        let top = self.design.top.as_deref().unwrap_or("");
        self.design
            .stopats
            .iter()
            .map(|s| format!("{top}.{s}"))
            .collect()
    }

    /// Names matching a Tcl-style glob (`*`, `?`).
    pub fn matching(&self, pattern: &str) -> Vec<usize> {
        self.props
            .iter()
            .enumerate()
            .filter(|(_, p)| glob(pattern, &p.name))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn elaborate(&mut self, top: Option<String>) -> anyhow::Result<()> {
        if let Some(t) = top {
            self.design.top = Some(t);
        }
        if self.dry_run {
            // Check mode: the design is not read (its files may not exist here).
            self.elaborated = true;
            return Ok(());
        }
        if self.design.files.is_empty() {
            bail!("no design files: run analyze / read_file first");
        }
        let req = informal_slang::Request {
            files: self.design.files.clone(),
            defines: self.design.defines.clone(),
            incdirs: self.design.incdirs.clone(),
            top: self.design.top.clone(),
            params: self.design.params.clone(),
            trace_outputs: false,
            cut_points: Vec::new(),
        };
        self.design.hierarchy.clear();
        let compiled = match frontend() {
            "svlang" => Some(informal_svlang::compile(&req)),
            _ => informal_slang::compile(&req),
        };
        if let Some(d) = compiled {
            let errors: Vec<String> = d.errors().take(20).map(|e| e.to_string()).collect();
            if !errors.is_empty() {
                bail!("SystemVerilog errors:\n{}", errors.join("\n"));
            }
            if self.design.top.is_none() {
                match d.tops.as_slice() {
                    [t] => self.design.top = Some(t.clone()),
                    tops => bail!("cannot tell the top module (candidates: {tops:?}); use -top"),
                }
            }
            self.hierarchy = d.instances.clone();
            self.design.hierarchy = d.hierarchy.clone();
            self.slang = Some(d);
        } else if self.design.top.is_none() {
            self.design.top = Some(guess_top(&self.design.files)?);
        }
        std::fs::create_dir_all(&self.workdir)?;
        let tree = self.workdir.join("design.hierarchy.json");
        if self.design.hierarchy.is_empty() {
            let _ = std::fs::remove_file(&tree);
        } else {
            std::fs::write(&tree, serde_json::to_string_pretty(&self.design.hierarchy)?)?;
        }
        self.elaborated = true;
        Ok(())
    }

    fn clock_for(&self, l: &sva::Lowered) -> anyhow::Result<(String, &'static str)> {
        if let Some(c) = &l.clock {
            return Ok((c.signal.clone(), c.edge));
        }
        let c = self
            .clocks
            .first()
            .ok_or_else(|| anyhow!("no clock: declare one with `clock` / `create_clock`"))?;
        Ok((c.signal.clone(), c.edge))
    }

    /// The generated checker text injected into the top module.
    fn harness(&self) -> anyhow::Result<String> {
        let (clk, edge) = match self.clocks.first() {
            Some(c) => (c.signal.clone(), c.edge),
            None => {
                // A property with its own clocking event can supply it.
                let l = self
                    .props
                    .iter()
                    .find_map(|p| sva::parse(&p.expr).ok().and_then(|l| l.clock))
                    .ok_or_else(|| {
                        anyhow!("no clock: declare one with `clock` / `create_clock`")
                    })?;
                (l.signal, l.edge)
            }
        };
        let first = u32::from(self.reset.is_some());
        let mut v = String::from("\n// ---- informal formal harness (generated) ----\n");
        v += "reg [15:0] informal_cyc = 16'd0;\n";
        v += &format!(
            "always @({edge} {clk}) if (informal_cyc != 16'hffff) informal_cyc <= informal_cyc + 16'd1;\n"
        );
        if let Some(r) = &self.reset {
            v += &format!(
                "always @* if (informal_cyc == 16'd0) assume ({r}); else assume (!({r}));\n"
            );
        }
        for (i, p) in self.props.iter().enumerate() {
            if !p.enabled {
                continue;
            }
            let l = sva::parse(&p.expr).map_err(|e| anyhow!("{}: {e}", p.name))?;
            let (pclk, pedge) = self.clock_for(&l)?;
            // After the reset cycle, and once the history it reads is real.
            let warm = (first + l.span).max(l.history());
            let mut guard = format!("informal_cyc >= 16'd{warm}");
            if let Some(d) = l.not_disabled() {
                guard += &format!(" && {d}");
            }
            let check = match p.kind {
                Kind::Assert | Kind::Assume => format!("!({}) || ({})", l.enable(), l.body()),
                // A cover is checked as "never matches"; a counterexample covers it.
                Kind::Cover => format!("!({} && {})", l.enable(), l.body()),
            };
            let verb = if p.kind == Kind::Assume {
                "assume"
            } else {
                "assert"
            };
            let label = format!("informal_p{i}");
            let line = format!(
                "always @({pedge} {pclk}) if ({guard}) {label}: {verb} ({check}); // {} {}\n",
                p.kind.label(),
                p.name
            );
            if p.kind == Kind::Assume {
                v += &line;
            } else {
                v += &format!("`ifdef INFORMAL_P{i}\n{line}`endif\n");
            }
        }
        v += "// ---- end informal formal harness ----\n";
        Ok(v)
    }

    /// Copy the file holding the top module with the harness inserted
    /// before its `endmodule`; other files are used as they are.
    fn injected_files(&self) -> anyhow::Result<Vec<PathBuf>> {
        let top = self
            .design
            .top
            .as_deref()
            .ok_or_else(|| anyhow!("no top module"))?;
        let harness = self.harness()?;
        let re = regex::Regex::new(&format!(r"(?m)\bmodule\s+{}\b", regex::escape(top)))?;
        let end = regex::Regex::new(r"\bendmodule\b")?;
        let mut out = Vec::new();
        let mut found = false;
        for f in &self.design.files {
            let text =
                std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?;
            match re.find(&text) {
                Some(m) if !found => {
                    let e = end
                        .find_at(&text, m.end())
                        .ok_or_else(|| anyhow!("no endmodule for {top} in {}", f.display()))?;
                    let name = f.file_name().unwrap_or_default().to_string_lossy();
                    let copy = self.workdir.join(format!("informal_{name}"));
                    let mut s = String::with_capacity(text.len() + harness.len());
                    s += &text[..e.start()];
                    s += &harness;
                    s += &text[e.start()..];
                    std::fs::write(&copy, s)?;
                    out.push(copy);
                    found = true;
                }
                _ => out.push(f.clone()),
            }
        }
        if !found {
            bail!("top module `{top}` not found in the analyzed files");
        }
        Ok(out)
    }

    fn engines(&self) -> Vec<Arc<dyn Engine>> {
        let picked: Vec<_> = self
            .engines
            .iter()
            .filter(|e| {
                self.engine_filter.is_empty() || self.engine_filter.iter().any(|n| n == e.name())
            })
            .filter(|e| e.capabilities().formats.contains(&ModelFormat::Btor2))
            .cloned()
            .collect();
        if picked.is_empty() {
            // set_engines rejects unknown names, so this is a filter whose
            // engines cannot read Btor2: say so rather than switch quietly.
            if !self.engine_filter.is_empty() {
                self.log(&format!(
                    "none of the selected engines ({}) reads Btor2; using all engines",
                    self.engine_filter.join(", ")
                ));
            }
            return self.engines.clone();
        }
        picked
    }

    /// Names of the engines this session can run.
    pub fn engine_names(&self) -> Vec<String> {
        self.engines.iter().map(|e| e.name().to_string()).collect()
    }

    /// Prove the selected assertions and covers (all enabled ones when
    /// `which` is empty). Returns their indices.
    ///
    /// Stop (`self.stop`) ends the run: the running property's portfolio is
    /// cancelled, and it and every property not reached yet become
    /// undetermined with reason "stopped". Elaboration and model building
    /// (svlang) are not interrupted; a Stop during them takes effect
    /// at the first engine run.
    pub fn prove(&mut self, which: &[usize]) -> anyhow::Result<Vec<usize>> {
        if self.dry_run {
            return Ok(Vec::new());
        }
        if !self.elaborated {
            self.elaborate(None)?;
        }
        self.check_reset_and_clocks().map_err(|e| anyhow!(e))?;
        let targets: Vec<usize> = if which.is_empty() {
            (0..self.props.len())
                .filter(|&i| self.props[i].enabled && self.props[i].kind != Kind::Assume)
                .collect()
        } else {
            which
                .iter()
                .copied()
                .filter(|&i| self.props[i].kind != Kind::Assume)
                .collect()
        };
        let tracker = self.sink.clone().map(|sink| {
            Tracker::start(
                sink,
                targets
                    .iter()
                    .map(|&i| (self.props[i].name.clone(), self.props[i].kind.label())),
            )
        });
        let files = match self.injected_files() {
            Ok(f) => f,
            Err(e) => {
                if let Some(t) = &tracker {
                    t.end(false);
                }
                return Err(e);
            }
        };
        let engines = self.engines();
        let fe = frontend();
        let lowered = self.lower(fe, &files, &targets);
        self.frontend = fe.into();

        // Heartbeat for the UI while the loop runs, so elapsed times move
        // between property events.
        let looping = AtomicBool::new(true);
        let reached = std::thread::scope(|sc| {
            if let Some(t) = &tracker {
                let looping = &looping;
                sc.spawn(move || {
                    let mut n = 0u32;
                    while looping.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(100));
                        n += 1;
                        if n.is_multiple_of(5) && looping.load(Ordering::SeqCst) {
                            t.tick();
                        }
                    }
                });
            }
            let r = self.prove_targets(
                &targets,
                lowered.as_deref().map_err(String::as_str),
                &engines,
                tracker.as_ref(),
            );
            looping.store(false, Ordering::SeqCst);
            r
        });
        // A Stop that came while the last property finished still counts.
        let stopped = reached < targets.len() || self.stop.load(Ordering::SeqCst);
        for &i in &targets[reached..] {
            self.props[i].status = Status::Undetermined {
                reason: "stopped".into(),
            };
            self.props[i].trace = None;
            self.props[i].engines.clear();
        }
        if let Some(t) = &tracker {
            t.end(stopped);
        }
        Ok(targets)
    }

    /// The per-property loop of [`Self::prove`]. Returns how many targets
    /// it got through; fewer than all means a Stop ended it.
    fn prove_targets(
        &mut self,
        targets: &[usize],
        lowered: Result<&str, &str>,
        engines: &[Arc<dyn Engine>],
        tracker: Option<&Tracker>,
    ) -> usize {
        for (slot, &i) in targets.iter().enumerate() {
            if self.stop.load(Ordering::SeqCst) {
                return slot;
            }
            if let Some(t) = tracker {
                t.running(slot);
            }
            // Properties added before elaboration are checked now.
            if let Err(e) = self.check_names(&self.props[i].expr) {
                let status = Status::Error { message: e };
                if let Some(t) = tracker {
                    t.finished(slot, &status, &[]);
                }
                self.props[i].status = status;
                self.props[i].engines.clear();
                continue;
            }
            let out = self.workdir.join(format!("prop_{i}.btor2"));
            let built = match lowered {
                Err(e) => Err(anyhow!("{e}")),
                Ok(btor) => {
                    let keep = format!("informal_p{i}");
                    let (model, section) = informal_slang::split_trace(btor);
                    let mut r = std::fs::write(&out, only_bad(model, &keep));
                    // The same model plus `output` lines naming the
                    // combinational nets, for the VCD (trace_model).
                    if r.is_ok() && !section.is_empty() {
                        r = std::fs::write(trace_model(&out), only_bad(btor, &keep));
                    }
                    r.map_err(anyhow::Error::from)
                }
            };
            self.props[i].trace = None;
            let (mut status, trace, runs) = match built {
                Err(e) => (
                    Status::Error {
                        message: format!("{e:#}"),
                    },
                    None,
                    Vec::new(),
                ),
                Ok(()) => self.check_one(engines, i, &out, tracker.map(|t| (t, slot))),
            };
            // Stopped mid-run: the portfolio came back without a verdict.
            // A verdict that landed just before the Stop is kept.
            let halt = self.stop.load(Ordering::SeqCst);
            if halt && matches!(status, Status::Undetermined { .. }) {
                status = Status::Undetermined {
                    reason: "stopped".into(),
                };
            }
            if let Some(t) = tracker {
                t.finished(slot, &status, &runs);
            }
            self.props[i].status = status;
            self.props[i].trace = trace;
            self.props[i].engines = runs;
            if halt {
                return slot + 1;
            }
        }
        targets.len()
    }

    /// One Btor2 model with every target's assertion. `Err` rejects the
    /// design: a compile error (an unknown name in a property or the reset,
    /// say) or a construct the front end does not model. There is no
    /// fallback: a guess there would turn a typo into a free signal (D13).
    fn lower(
        &self,
        fe: &'static str,
        files: &[PathBuf],
        targets: &[usize],
    ) -> Result<String, String> {
        let mut defines = self.design.defines.clone();
        defines.extend(targets.iter().map(|i| (format!("INFORMAL_P{i}"), None)));
        let req = informal_slang::Request {
            files: files.to_vec(),
            defines,
            incdirs: self.design.incdirs.clone(),
            top: self.design.top.clone(),
            params: self.design.params.clone(),
            // Appends named outputs for combinational nets after
            // informal_slang::TRACE_MARKER: engines get the text before it
            // (unchanged), the trace replay all of it.
            trace_outputs: true,
            cut_points: self.cut_points(),
        };
        if !req.cut_points.is_empty() {
            self.log(&format!(
                "{} cut point(s) (stopat) are free in this model: {}. A proof holds for the design; a counterexample may use values the design cannot produce there",
                self.design.stopats.len(),
                self.design.stopats.join(", ")
            ));
        }
        let l = if fe == "slang" {
            informal_slang::lower(&req).ok_or("slang: this informal was built without slang")?
        } else {
            informal_svlang::lower(&req)
        };
        for n in &l.notes {
            self.log(&format!("{fe}: {n}"));
        }
        if l.ok {
            Ok(l.btor2)
        } else {
            Err(format!("{fe}: {}", l.errors.join("; ")))
        }
    }

    /// Run the portfolio on one property's model. With a tracker, engine
    /// progress (k-bound) is forwarded to it for `slot`. A CEX or cover
    /// also gets its trace written (the path is returned with the status),
    /// and every engine's run is returned for the proof profiler.
    fn check_one(
        &self,
        engines: &[Arc<dyn Engine>],
        i: usize,
        model: &Path,
        tracker: Option<(&Tracker, usize)>,
    ) -> (Status, Option<String>, Vec<EngineRun>) {
        let kind = self.props[i].kind;
        let bads = std::fs::read_to_string(model)
            .map(|t| {
                t.lines()
                    .filter(|l| l.split_whitespace().nth(1) == Some("bad"))
                    .count()
            })
            .unwrap_or(0);
        if bads == 0 {
            // The front end folded the check to a constant true.
            let status = match kind {
                Kind::Cover => Status::Unreachable {
                    engine: "elaboration".into(),
                    ms: 0,
                },
                _ => Status::Proven {
                    engine: "elaboration".into(),
                    ms: 0,
                },
            };
            return (status, None, Vec::new());
        }
        let req = CheckRequest {
            model: model.to_path_buf(),
            format: ModelFormat::Btor2,
            timeout: self.time_limit,
        };
        let stop = Some(self.stop.as_ref());
        let o = match tracker {
            None => portfolio::run_until(engines, &req, None, stop),
            Some((t, slot)) => std::thread::scope(|sc| {
                let (tx, rx) = mpsc::channel::<Progress>();
                // Ends when the portfolio returns and drops the last sender.
                sc.spawn(move || {
                    for p in rx {
                        match p.output {
                            Some((text, err)) => t.engine_line(slot, &p.engine, text, err),
                            None => t.bound(slot, &p.engine, p.bound),
                        }
                    }
                });
                portfolio::run_until(engines, &req, Some(tx), stop)
            }),
        };
        let engine = o
            .winner
            .as_ref()
            .map(|w| w.engine.clone())
            .unwrap_or_default();
        let ms = o.wall_ms;
        let winner = engine.clone();
        let status = match (portfolio::verdict(&o), kind) {
            (Verdict::Safe, Kind::Cover) => Status::Unreachable { engine, ms },
            (Verdict::Safe, _) => Status::Proven { engine, ms },
            (Verdict::Unsafe { depth }, Kind::Cover) => Status::Covered { engine, depth, ms },
            (Verdict::Unsafe { depth }, _) => Status::Cex { engine, depth, ms },
            (Verdict::Unknown { reason }, _) => Status::Undetermined { reason },
        };
        let mut status = status;
        let mut trace = None;
        let mut withdrawn = None;
        if let Status::Cex { depth, .. } | Status::Covered { depth, .. } = &mut status {
            if !self.stop.load(Ordering::SeqCst) {
                let witness = o.winner.as_ref().and_then(|w| w.witness.as_deref());
                match self.write_trace(i, model, witness, *depth) {
                    Ok((path, d)) => {
                        // IC3 and external engines often report no depth; the
                        // replayed trace has one.
                        depth.get_or_insert(d as u32);
                        trace = Some(path);
                    }
                    Err(reason) => withdrawn = reason,
                }
            }
        }
        // A counterexample nothing here can reproduce is not reported: the
        // replay is the only check an UNSAFE verdict gets.
        if let Some(why) = withdrawn {
            self.log(&format!(
                "{}: {winner} reported a counterexample that does not replay ({why}); verdict withdrawn",
                self.props[i].name
            ));
            status = Status::Undetermined {
                reason: format!("counterexample did not replay ({winner})"),
            };
        }
        // The profiler's winner row gets the same depth when it had none.
        let mut runs = o.profile;
        if let Status::Cex {
            depth: Some(d),
            engine,
            ..
        }
        | Status::Covered {
            depth: Some(d),
            engine,
            ..
        } = &status
        {
            for r in runs
                .iter_mut()
                .filter(|r| &r.engine == engine && r.depth.is_none())
            {
                r.depth = Some(*d);
            }
        }
        (status, trace, runs)
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
    fn write_trace(
        &self,
        i: usize,
        model: &Path,
        witness: Option<&str>,
        depth: Option<u32>,
    ) -> Result<(String, usize), Option<String>> {
        let name = &self.props[i].name;
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
        let req = CheckRequest {
            model: model.to_path_buf(),
            format: ModelFormat::Btor2,
            timeout: self.time_limit.min(Duration::from_secs(30)),
        };
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
    fn note(&self, line: &str) {
        if let Some(s) = &self.sink {
            s.line(line, false);
        }
    }

    /// A line for the console: the app's sink when there is one, else stderr.
    fn log(&self, line: &str) {
        match &self.sink {
            Some(s) => s.line(line, true),
            None => eprintln!("informal: {line}"),
        }
    }

    /// A JasperGold-style summary table.
    pub fn report(&self) -> String {
        let mut s = format!(
            "{:<28} {:<7} {:<13} {:<10} {}\n",
            "property", "type", "status", "engine", "detail"
        );
        for p in &self.props {
            let (engine, detail) = match &p.status {
                Status::Proven { engine, ms } | Status::Unreachable { engine, ms } => {
                    (engine.clone(), format!("{:.2} s", *ms as f64 / 1000.0))
                }
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
            s += &format!(
                "{:<28} {:<7} {:<13} {:<10} {}\n",
                p.name,
                p.kind.label(),
                status,
                engine,
                detail
            );
        }
        s
    }
}

/// The Btor2 text with every `bad` line dropped except the one named `keep`.
/// `prop_N.trace.btor2` next to `prop_N.btor2`: the property's model with
/// the slang lowering's named-output section, used only to replay traces.
fn trace_model(model: &Path) -> PathBuf {
    model.with_extension("trace.btor2")
}

fn only_bad(btor: &str, keep: &str) -> String {
    btor.lines()
        .filter(|l| {
            let w: Vec<&str> = l.split_whitespace().collect();
            w.get(1) != Some(&"bad") || w.get(3) == Some(&keep)
        })
        .flat_map(|l| [l, "\n"])
        .collect()
}

/// The front end that elaborates and lowers designs: `svlang` (the
/// default), or `slang` through `INFORMAL_FRONTEND=slang` (a cross-check).
pub fn frontend() -> &'static str {
    match std::env::var("INFORMAL_FRONTEND").as_deref() {
        Ok("slang") => "slang",
        _ => "svlang",
    }
}

/// The single module never instantiated by another, when there is one.
fn guess_top(files: &[PathBuf]) -> anyhow::Result<String> {
    let decl = regex::Regex::new(r"(?m)^\s*module\s+([A-Za-z_][A-Za-z0-9_$]*)")?;
    let mut text = String::new();
    for f in files {
        text += &std::fs::read_to_string(f)?;
    }
    let names: Vec<String> = decl
        .captures_iter(&text)
        .map(|c| c[1].to_string())
        .collect();
    let roots: Vec<&String> = names
        .iter()
        .filter(|n| {
            let inst = regex::Regex::new(&format!(
                r"(?m)^\s*{}\s+(#\s*\(|[A-Za-z_])",
                regex::escape(n)
            ))
            .unwrap();
            !inst.is_match(&text)
        })
        .collect();
    match roots.as_slice() {
        [one] => Ok((*one).clone()),
        _ => bail!("cannot tell the top module (candidates: {roots:?}); use -top"),
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

/// `1h`, `30m`, `90s`, `2H`, or plain seconds.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, unit) = s.split_at(
        s.find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(s.len()),
    );
    let n: f64 = num.parse().map_err(|_| format!("bad time `{s}`"))?;
    let secs = match unit.to_ascii_lowercase().as_str() {
        "" | "s" => n,
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
