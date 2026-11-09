//! External engine adapters.
//!
//! Phase 0 runs existing model checkers (rIC3, Pono, ABC, BtorMC, ...) as
//! child processes behind the [`fdv_engine::Engine`] API. Each engine is
//! described in a TOML file: how to call it, which formats it reads, and
//! how to read its verdict from the exit code or output.

use anyhow::{bail, Context};
use fdv_engine::{
    Capabilities, CheckRequest, Engine, EngineResult, ModelFormat, Progress, RunContext, Verdict,
};

/// Output lines per stream forwarded live to a progress listener per run.
const LOG_LINES: usize = 2000;
use regex::Regex;
use serde::Deserialize;
use std::io::{BufRead, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How to call one external engine. See `config/engines.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineSpec {
    pub name: String,
    pub program: String,
    /// Arguments; `{model}` is replaced with the model path.
    #[serde(default)]
    pub args: Vec<String>,
    pub formats: Vec<ModelFormat>,
    #[serde(default)]
    pub proves: bool,
    #[serde(default)]
    pub finds_bugs: bool,
    #[serde(default)]
    pub liveness: bool,
    #[serde(default)]
    pub exit_codes: ExitCodes,
    #[serde(default)]
    pub patterns: Patterns,
    /// Keep stdout as the witness when the verdict is unsafe.
    #[serde(default)]
    pub witness_on_stdout: bool,
    /// The engine prints its counterexample (rIC3 `--cex`, ABC `write_cex
    /// -a -t /dev/stdout`) in the witness format of the model it read:
    /// Btor2 witness for `.btor2`, AIGER witness for AIGER. On an unsafe
    /// verdict it is converted to fdv's named witness (fdv_engine::witness)
    /// using the model's names, so the trace needs no BMC re-run. Ignored
    /// for preprocessed models (their positions are not the model's).
    #[serde(default)]
    pub cex_witness: bool,
    /// Give the engine fdv's preprocessed model (binary AIGER, passed in
    /// memory through memfd on Linux) instead of the original file.
    #[serde(default)]
    pub preprocess: bool,
    /// Memory cap in MiB for the engine and every process it starts
    /// (default: `FDV_ENGINE_MEM_MB`, else none). On Linux this runs the
    /// engine in a systemd user scope with MemoryMax, so a runaway engine
    /// is killed on its own instead of the kernel picking a victim.
    #[serde(default)]
    pub mem_limit_mb: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExitCodes {
    #[serde(default)]
    pub safe: Vec<i32>,
    #[serde(default, rename = "unsafe")]
    pub unsafe_: Vec<i32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patterns {
    pub safe: Option<String>,
    #[serde(rename = "unsafe")]
    pub unsafe_: Option<String>,
    /// Regex whose first capture group is the counterexample depth.
    pub depth: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    #[serde(default)]
    engine: Vec<EngineSpec>,
}

/// An external engine ready to run.
pub struct ExternalEngine {
    spec: EngineSpec,
    safe_re: Option<Regex>,
    unsafe_re: Option<Regex>,
    depth_re: Option<Regex>,
}

impl ExternalEngine {
    pub fn new(spec: EngineSpec) -> anyhow::Result<Self> {
        let re = |p: &Option<String>, what: &str| -> anyhow::Result<Option<Regex>> {
            p.as_deref()
                .map(Regex::new)
                .transpose()
                .with_context(|| format!("engine '{}': bad {what} pattern", spec.name))
        };
        let safe_re = re(&spec.patterns.safe, "safe")?;
        let unsafe_re = re(&spec.patterns.unsafe_, "unsafe")?;
        let depth_re = re(&spec.patterns.depth, "depth")?;
        if spec.formats.is_empty() {
            bail!("engine '{}' lists no formats", spec.name);
        }
        Ok(ExternalEngine {
            spec,
            safe_re,
            unsafe_re,
            depth_re,
        })
    }

    pub fn spec(&self) -> &EngineSpec {
        &self.spec
    }

    /// Read the verdict from an exit code and the combined output.
    /// Exit codes win over patterns; `unsafe` is checked before `safe`.
    pub fn classify(&self, code: Option<i32>, output: &str) -> Verdict {
        let depth = || {
            self.depth_re
                .as_ref()
                .and_then(|r| r.captures(output))
                .and_then(|c| c.get(1))
                .and_then(|m| m.as_str().parse().ok())
        };
        if let Some(c) = code {
            if self.spec.exit_codes.unsafe_.contains(&c) {
                return Verdict::Unsafe { depth: depth() };
            }
            if self.spec.exit_codes.safe.contains(&c) {
                return Verdict::Safe;
            }
        }
        if self.unsafe_re.as_ref().is_some_and(|r| r.is_match(output)) {
            return Verdict::Unsafe { depth: depth() };
        }
        if self.safe_re.as_ref().is_some_and(|r| r.is_match(output)) {
            return Verdict::Safe;
        }
        Verdict::Unknown {
            reason: format!("no verdict in output (exit code {code:?})"),
        }
    }
}

/// Kill the engine's whole process group (the engine and its children).
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        if let Ok(pid) = i32::try_from(child.id()) {
            // SAFETY: plain syscall on a process group id we created.
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
}

/// True when `systemd-run --user --scope` works here (checked once).
fn scopes_available() -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| {
        cfg!(target_os = "linux")
            && Command::new("systemd-run")
                .args(["--user", "--scope", "--quiet", "--collect", "true"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
    })
}

/// A preprocessed model held in memory for a child process.
struct Staged {
    path: String,
    _file: std::fs::File,
}

/// Simplify the model and write it as binary AIGER to an anonymous
/// in-memory file the child can open by `/proc/self/fd/N` (it inherits the
/// descriptor). `None` (use the original file) when the model cannot be
/// loaded or has resets AIGER cannot express.
fn stage_preprocessed(model: &Path) -> Option<Staged> {
    let g = fdv_aig::load(model).ok()?;
    let (s, _) = fdv_aig::preprocess::simplify(&g);
    if s.latches
        .iter()
        .any(|l| matches!(l.init, fdv_aig::Init::Expr(_)))
    {
        return None;
    }
    let bytes = fdv_aig::aiger::write_binary(&s);
    stage_bytes(&bytes)
}

#[cfg(target_os = "linux")]
fn stage_bytes(bytes: &[u8]) -> Option<Staged> {
    use std::io::Write;
    use std::os::fd::FromRawFd;
    let name = std::ffi::CString::new("fdv-model").ok()?;
    // No MFD_CLOEXEC: the engine process must inherit the descriptor.
    let fd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
    if fd < 0 {
        return None;
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(bytes).ok()?;
    Some(Staged {
        path: format!("/proc/self/fd/{fd}"),
        _file: file,
    })
}

#[cfg(not(target_os = "linux"))]
fn stage_bytes(bytes: &[u8]) -> Option<Staged> {
    let path =
        std::env::temp_dir().join(format!("fdv-model-{}-{:p}.aig", std::process::id(), bytes));
    std::fs::write(&path, bytes).ok()?;
    let file = std::fs::File::open(&path).ok()?;
    Some(Staged {
        path: path.to_string_lossy().into_owned(),
        _file: file,
    })
}

/// Load every `[[engine]]` from a TOML file.
pub fn load(path: &Path) -> anyhow::Result<Vec<Arc<dyn Engine>>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse_config(&text).with_context(|| format!("in {}", path.display()))
}

pub fn parse_config(text: &str) -> anyhow::Result<Vec<Arc<dyn Engine>>> {
    let cfg: ConfigFile = toml::from_str(text)?;
    cfg.engine
        .into_iter()
        .map(|s| Ok(Arc::new(ExternalEngine::new(s)?) as Arc<dyn Engine>))
        .collect()
}

impl Engine for ExternalEngine {
    fn name(&self) -> &str {
        &self.spec.name
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            formats: self.spec.formats.clone(),
            proves: self.spec.proves,
            finds_bugs: self.spec.finds_bugs,
            liveness: self.spec.liveness,
            lemma_export: false,
            lemma_import: false,
        }
    }

    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let start = Instant::now();
        // Kept alive until the child exits: the engine reads it by path.
        let staged = if self.spec.preprocess {
            stage_preprocessed(&req.model)
        } else {
            None
        };
        let model = match &staged {
            Some(s) => s.path.clone(),
            None => req.model.to_string_lossy().into_owned(),
        };
        let args: Vec<String> = self
            .spec
            .args
            .iter()
            .map(|a| a.replace("{model}", &model))
            .collect();
        let limit = self.spec.mem_limit_mb.or_else(|| {
            std::env::var("FDV_ENGINE_MEM_MB")
                .ok()
                .and_then(|v| v.parse().ok())
        });
        let mut cmd = match limit.filter(|_| scopes_available()) {
            Some(mb) => {
                let mut c = Command::new("systemd-run");
                c.args(["--user", "--scope", "--quiet", "--collect"])
                    .arg(format!("-pMemoryMax={mb}M"))
                    .arg("-pMemorySwapMax=0")
                    .arg(&self.spec.program);
                c
            }
            None => Command::new(&self.spec.program),
        };
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Own process group, so a timeout kills the engine and every child it started.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let mut child = cmd.spawn().with_context(|| {
            format!(
                "starting '{}' for engine '{}'",
                self.spec.program, self.spec.name
            )
        })?;

        // Drain both pipes on threads so a chatty engine cannot block on a
        // full pipe. With a progress channel (the desktop app), each line is
        // also forwarded as it arrives, up to LOG_LINES per stream.
        let drain = |r: Box<dyn Read + Send>, stderr: bool| {
            let tx = ctx.progress.clone();
            let name = self.spec.name.clone();
            std::thread::spawn(move || {
                let mut r = std::io::BufReader::new(r);
                let mut all = Vec::new();
                let mut line = Vec::new();
                let mut sent = 0usize;
                loop {
                    line.clear();
                    match r.read_until(b'\n', &mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    all.extend_from_slice(&line);
                    if let Some(tx) = &tx {
                        if sent < LOG_LINES {
                            let text = String::from_utf8_lossy(&line)
                                .trim_end_matches(['\n', '\r'])
                                .to_string();
                            let _ = tx.send(Progress::line(&name, text, stderr));
                        } else if sent == LOG_LINES {
                            let note = format!(
                                "[{name}: further {} output not shown]",
                                if stderr { "stderr" } else { "stdout" }
                            );
                            let _ = tx.send(Progress::line(&name, note, stderr));
                        }
                        sent += 1;
                    }
                }
                String::from_utf8_lossy(&all).into_owned()
            })
        };
        let out_t = drain(Box::new(child.stdout.take().expect("piped")), false);
        let err_t = drain(Box::new(child.stderr.take().expect("piped")), true);

        let status = loop {
            if let Some(st) = child.try_wait()? {
                break Some(st);
            }
            if ctx.should_stop() {
                kill_tree(&mut child);
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        // Workers the engine forked may outlive it (rIC3's portfolio does
        // when its parent dies) and would hold the output pipes open
        // forever: kill whatever is left of the process group.
        #[cfg(unix)]
        unsafe {
            libc::killpg(child.id() as i32, libc::SIGKILL);
        }
        let stdout = out_t.join().unwrap_or_default();
        let stderr = err_t.join().unwrap_or_default();

        let verdict = match status {
            None if ctx.cancel.is_cancelled() => Verdict::Unknown {
                reason: "cancelled".into(),
            },
            None => Verdict::Unknown {
                reason: "timeout".into(),
            },
            Some(st) => self.classify(st.code(), &format!("{stdout}\n{stderr}")),
        };
        let witness = if !matches!(verdict, Verdict::Unsafe { .. }) {
            None
        } else if self.spec.cex_witness && staged.is_none() {
            // The depth is left as the engine reported it: the witness is
            // not checked here (fdv-fpv replays it and takes the depth from
            // the trace when it does).
            match named_witness(&stdout, &req.model, req.format) {
                Ok(w) => Some(w),
                Err(e) => {
                    ctx.report(Progress::line(
                        &self.spec.name,
                        format!("[fdv: witness not used: {e}]"),
                        true,
                    ));
                    None
                }
            }
        } else {
            self.spec.witness_on_stdout.then_some(stdout)
        };
        Ok(EngineResult {
            engine: self.spec.name.clone(),
            verdict,
            wall_ms: start.elapsed().as_millis(),
            witness,
        })
    }
}

/// The counterexample in an engine's stdout as a named witness: Btor2
/// witness for a Btor2 model, AIGER witness for AIGER.
fn named_witness(stdout: &str, model: &Path, format: ModelFormat) -> Result<String, String> {
    let btor2 = format == ModelFormat::Btor2;
    let raw =
        fdv_engine::witness::extract(stdout, btor2).ok_or("no counterexample in the output")?;
    let named = if btor2 {
        let text =
            std::fs::read_to_string(model).map_err(|e| format!("{}: {e}", model.display()))?;
        fdv_engine::witness::from_btor2(&raw, &text)?
    } else {
        let g = fdv_aig::load(model).map_err(|e| format!("{}: {e:#}", model.display()))?;
        let name = |l: u32| g.names.get(&fdv_aig::var(l)).cloned();
        let inputs: Vec<_> = g.inputs.iter().map(|&i| name(i)).collect();
        let latches: Vec<_> = g.latches.iter().map(|l| name(l.lit)).collect();
        fdv_engine::witness::from_aiger(&raw, &inputs, &latches)?
    };
    Ok(named)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh_engine(name: &str, script: &str, extra: &str) -> Arc<dyn Engine> {
        let toml = format!(
            "[[engine]]\nname = \"{name}\"\nprogram = \"sh\"\nargs = [\"-c\", '''{script}''', \"sh\", \"{{model}}\"]\nformats = [\"btor2\"]\n{extra}"
        );
        parse_config(&toml).unwrap().remove(0)
    }

    fn req(timeout_ms: u64) -> CheckRequest {
        CheckRequest {
            model: "x.btor2".into(),
            format: ModelFormat::Btor2,
            timeout: Duration::from_millis(timeout_ms),
        }
    }

    #[test]
    fn reads_verdict_from_output_patterns() {
        let pats = "[engine.patterns]\nsafe = '(?m)^unsat$'\nunsafe = '(?m)^sat$'\n";
        let e = sh_engine("p", "echo sat; echo b0", pats);
        let r = e
            .check(&req(5_000), &RunContext::new(Duration::from_secs(5)))
            .unwrap();
        assert_eq!(r.verdict, Verdict::Unsafe { depth: None });
        let e = sh_engine("p", "echo unsat", pats);
        assert_eq!(
            e.check(&req(5_000), &RunContext::new(Duration::from_secs(5)))
                .unwrap()
                .verdict,
            Verdict::Safe
        );
    }

    #[test]
    fn streams_output_lines_while_running() {
        let pats = "[engine.patterns]\nsafe = '(?m)^unsat$'\n";
        // The second line only comes after the first was forwarded.
        let e = sh_engine("s", "echo one; echo warn >&2; sleep 0.3; echo unsat", pats);
        let (tx, rx) = std::sync::mpsc::channel();
        let mut ctx = RunContext::new(Duration::from_secs(5));
        ctx.progress = Some(tx);
        let t = std::thread::spawn(move || {
            let first = rx.recv_timeout(Duration::from_millis(250)).ok();
            let rest: Vec<_> = rx.iter().collect();
            (first, rest)
        });
        let r = e.check(&req(5_000), &ctx).unwrap();
        drop(ctx);
        assert_eq!(r.verdict, Verdict::Safe);
        let (first, rest) = t.join().unwrap();
        let first = first.expect("a line before the engine exits");
        assert_eq!(first.engine, "s");
        let mut lines: Vec<_> = std::iter::once(first)
            .chain(rest)
            .filter_map(|p| p.output)
            .collect();
        lines.sort();
        assert_eq!(
            lines,
            [
                ("one".to_string(), false),
                ("unsat".to_string(), false),
                ("warn".to_string(), true)
            ]
        );
    }

    #[test]
    fn exit_codes_and_depth() {
        let extra = "witness_on_stdout = true\n[engine.exit_codes]\nsafe = [20]\nunsafe = [10]\n[engine.patterns]\ndepth = 'frame (\\d+)'\n";
        let e = sh_engine("x", "echo asserted in frame 7; exit 10", extra);
        let r = e
            .check(&req(5_000), &RunContext::new(Duration::from_secs(5)))
            .unwrap();
        assert_eq!(r.verdict, Verdict::Unsafe { depth: Some(7) });
        assert!(r.witness.unwrap().contains("frame 7"));
        let e = sh_engine("x", "exit 20", extra);
        assert_eq!(
            e.check(&req(5_000), &RunContext::new(Duration::from_secs(5)))
                .unwrap()
                .verdict,
            Verdict::Safe
        );
    }

    #[test]
    fn model_path_is_passed_through() {
        let e = sh_engine(
            "echo",
            "case \"$1\" in *x.btor2) echo unsat;; esac",
            "[engine.patterns]\nsafe = 'unsat'\n",
        );
        assert_eq!(
            e.check(&req(5_000), &RunContext::new(Duration::from_secs(5)))
                .unwrap()
                .verdict,
            Verdict::Safe
        );
    }

    #[test]
    fn kills_the_process_at_the_deadline() {
        let e = sh_engine("sleeper", "exec sleep 30", "");
        let t = Instant::now();
        let r = e
            .check(&req(200), &RunContext::new(Duration::from_millis(200)))
            .unwrap();
        assert_eq!(
            r.verdict,
            Verdict::Unknown {
                reason: "timeout".into()
            }
        );
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn kills_grandchildren_too() {
        // `sh` forks `sleep`; killing only `sh` would leave the pipe open.
        let e = sh_engine("forker", "sleep 30; echo done", "");
        let t = Instant::now();
        let r = e
            .check(&req(200), &RunContext::new(Duration::from_millis(200)))
            .unwrap();
        assert_eq!(
            r.verdict,
            Verdict::Unknown {
                reason: "timeout".into()
            }
        );
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "took {:?}",
            t.elapsed()
        );
    }

    #[test]
    fn reports_a_missing_binary() {
        let toml = "[[engine]]\nname = \"ghost\"\nprogram = \"/nonexistent/fdv-engine\"\nformats = [\"aiger\"]\n";
        let e = parse_config(toml).unwrap().remove(0);
        assert!(e
            .check(&req(100), &RunContext::new(Duration::from_millis(100)))
            .is_err());
    }

    #[test]
    fn shipped_config_parses() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/engines.toml");
        let engines = load(&path).unwrap();
        assert!(engines.len() >= 4);
    }

    fn models() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/models")
    }

    /// A fake engine that prints a hand-written AIGER witness for the 2-bit
    /// counter (en high for three cycles, q reaches 3): the result carries
    /// it as a named witness, by the model's symbol table.
    #[test]
    fn converts_an_aiger_counterexample_to_a_named_witness() {
        let toml = r#"
[[engine]]
name = "fake-aig"
program = "sh"
args = ["-c", "echo 'Output 0 was asserted in frame 3.'; printf '1\\nb0\\n00\\n1\\n1\\n1\\n0\\n.\\n'", "sh", "{model}"]
formats = ["aiger"]
finds_bugs = true
cex_witness = true
[engine.patterns]
unsafe = 'was asserted in frame \d+'
depth = 'was asserted in frame (\d+)'
"#;
        let e = parse_config(toml).unwrap().remove(0);
        let req = CheckRequest {
            model: models().join("counter2_unsafe.aag"),
            format: ModelFormat::Aiger,
            timeout: Duration::from_secs(5),
        };
        let r = e
            .check(&req, &RunContext::new(Duration::from_secs(5)))
            .unwrap();
        assert_eq!(r.verdict, Verdict::Unsafe { depth: Some(3) });
        assert_eq!(
            r.witness.as_deref(),
            Some(
                "fdvw 1\ndepth 3\ninit counter.q[0]=0 counter.q[1]=0\n\
                 @0 counter.en=1\n@1 counter.en=1\n@2 counter.en=1\n@3 counter.en=0\n"
            )
        );
    }

    /// The same for a Btor2 witness (rIC3 `--cex` on a .btor2 model).
    #[test]
    fn converts_a_btor2_counterexample_to_a_named_witness() {
        let frames: String = (1..=7).map(|k| format!("#{k}\\n@{k}\\n")).collect();
        let toml = format!(
            r#"
[[engine]]
name = "fake-btor"
program = "sh"
args = ["-c", "printf 'SAT\\nsat\\nb0\\n#0\\n0 000 cnt\\n@0\\n{frames}.\\n'", "sh", "{{model}}"]
formats = ["btor2"]
finds_bugs = true
cex_witness = true
[engine.patterns]
unsafe = '(?m)^SAT$'
"#
        );
        let e = parse_config(&toml).unwrap().remove(0);
        let req = CheckRequest {
            model: models().join("counter.btor2"),
            format: ModelFormat::Btor2,
            timeout: Duration::from_secs(5),
        };
        let r = e
            .check(&req, &RunContext::new(Duration::from_secs(5)))
            .unwrap();
        let w = r.witness.unwrap();
        assert!(
            w.starts_with("fdvw 1\ndepth 7\ninit cnt[0]=0 cnt[1]=0 cnt[2]=0\n@0\n@1\n"),
            "{w}"
        );
        // A witness that does not fit the model is dropped (a note goes to
        // the engine log) rather than passed on.
        let bad = toml.replace("0 000 cnt", "0 00 cnt");
        let e = parse_config(&bad).unwrap().remove(0);
        let r = e
            .check(&req, &RunContext::new(Duration::from_secs(5)))
            .unwrap();
        assert!(matches!(r.verdict, Verdict::Unsafe { .. }) && r.witness.is_none());
    }
}
