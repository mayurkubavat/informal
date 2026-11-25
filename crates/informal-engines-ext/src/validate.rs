//! File-based verdict, witness, and certificate validation for external engines (D1/D8).
//!
//! Replaces fragile stdout-only verdict parsing when an external checker writes
//! structured artifacts (`{cert}` for rIC3 `--cert`, `{cex}` for ABC `write_cex`):
//! - Witness files are converted to `informalw 1` and independently replayed on
//!   the original unsimplified AIG (every constraint must hold and `bad` must
//!   assert at the reported depth). A forged or non-replaying witness withdraws
//!   an `Unsafe` claim to `Unknown`.
//! - AIGER proof certificates are structurally parsed and cross-checked with
//!   `certifaiger` when installed; a corrupt or rejected certificate withdraws
//!   a `Safe` claim to `Unknown`.
//! - When stdout wording changes across tool versions, a validated artifact file
//!   still recovers the exact verdict without relying on regexes.

use informal_aig::{is_neg, var, Aig, Init, Lit, Node};
use informal_engine::{ModelFormat, Verdict};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

/// Temporary `{cert}` / `{cex}` output file paths owned by one external run.
/// Removed automatically when dropped.
pub struct ArtifactFiles {
    pub cert: Option<PathBuf>,
    pub cex: Option<PathBuf>,
}

impl ArtifactFiles {
    pub fn for_args(args: &[String], format: ModelFormat) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let needs_cert = args.iter().any(|a| a.contains("{cert}"));
        let needs_cex = args.iter().any(|a| a.contains("{cex}"));
        let pid = std::process::id();
        let dir = std::env::temp_dir();
        let cert = needs_cert.then(|| {
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let ext = match format {
                ModelFormat::Aiger => "aag",
                ModelFormat::Btor2 => "btor2",
            };
            dir.join(format!("informal-ext-cert-{pid}-{n}.{ext}"))
        });
        let cex = needs_cex.then(|| {
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            dir.join(format!("informal-ext-cex-{pid}-{n}.wit"))
        });
        Self { cert, cex }
    }

    pub fn substitute(&self, arg: &str) -> String {
        let mut s = arg.to_string();
        if let Some(p) = &self.cert {
            s = s.replace("{cert}", &p.to_string_lossy());
        }
        if let Some(p) = &self.cex {
            s = s.replace("{cex}", &p.to_string_lossy());
        }
        s
    }
}

impl Drop for ArtifactFiles {
    fn drop(&mut self) {
        if let Some(p) = &self.cert {
            let _ = std::fs::remove_file(p);
        }
        if let Some(p) = &self.cex {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Result of cross-validating an external engine's classified verdict against
/// its `{cert}` / `{cex}` artifact files (and optional stdout witness).
pub struct ValidatedResult {
    pub verdict: Verdict,
    pub witness: Option<String>,
    pub note: Option<String>,
}

/// Reconcile a stdout/exit-code `verdict` with structured `{cert}` / `{cex}`
/// files produced by the external engine.
#[allow(clippy::too_many_arguments)]
pub fn reconcile_verdict(
    verdict: Verdict,
    model: &Path,
    format: ModelFormat,
    staged: bool,
    artifacts: &ArtifactFiles,
    stdout: &str,
    cex_witness: bool,
    witness_on_stdout: bool,
    validate_files: bool,
) -> ValidatedResult {
    // 1. Inspect any non-empty artifact file written to `{cex}` or `{cert}`.
    let cex_text = read_nonempty(artifacts.cex.as_deref());
    let cert_text = read_nonempty(artifacts.cert.as_deref());

    // Witness file candidate: explicit `{cex}` first, or `{cert}` if it looks
    // like an AIGER/Btor2 witness (rIC3 writes witnesses into `--cert` on SAT).
    let witness_file: Option<(String, &Path)> = if let Some((t, p)) = cex_text {
        Some((t, p))
    } else if let Some((ref t, p)) = cert_text {
        if looks_like_witness(t, format) {
            Some((t.clone(), p))
        } else {
            None
        }
    } else {
        None
    };

    // 2. If a witness file exists and model was not preprocessed, validate it!
    if let Some((raw_text, _path)) = witness_file {
        if !staged {
            match convert_and_replay_witness(&raw_text, model, format) {
                Ok((named, depth)) => {
                    // File-validated counterexample: confirms Unsafe (and even
                    // recovers Unsafe if stdout regex failed across versions!).
                    if matches!(verdict, Verdict::Safe) {
                        return ValidatedResult {
                            verdict: Verdict::Unknown {
                                reason: "external engine claimed safe but wrote a valid counterexample witness".into(),
                            },
                            witness: None,
                            note: Some("verdict/witness conflict withdrawn".into()),
                        };
                    }
                    return ValidatedResult {
                        verdict: Verdict::Unsafe {
                            depth: Some(depth as u32),
                        },
                        witness: Some(named),
                        note: None,
                    };
                }
                Err(why) => {
                    if validate_files && matches!(verdict, Verdict::Unsafe { .. }) {
                        // Fail closed: an engine that wrote a bad witness file
                        // must not have its Unsafe claim trusted blindly.
                        return ValidatedResult {
                            verdict: Verdict::Unknown {
                                reason: format!("external witness failed validation: {why}"),
                            },
                            witness: None,
                            note: Some(format!("witness file rejected: {why}")),
                        };
                    }
                    return ValidatedResult {
                        verdict,
                        witness: None,
                        note: Some(format!("witness file ignored: {why}")),
                    };
                }
            }
        }
    }

    // 3. If a proof certificate file exists on `{cert}` (not a witness), validate it!
    if let Some((ref text, cert_path)) = cert_text {
        if !looks_like_witness(text, format) && !staged {
            match validate_proof_cert(model, cert_path, text, format) {
                Ok(cert_note) => {
                    if matches!(verdict, Verdict::Unsafe { .. }) {
                        return ValidatedResult {
                            verdict: Verdict::Unknown {
                                reason:
                                    "external engine claimed unsafe but wrote a proof certificate"
                                        .into(),
                            },
                            witness: None,
                            note: Some("verdict/certificate conflict withdrawn".into()),
                        };
                    }
                    // Valid certificate confirms SAFE (even if stdout wording shifted!).
                    return ValidatedResult {
                        verdict: Verdict::Safe,
                        witness: None,
                        note: cert_note,
                    };
                }
                Err(why) => {
                    if validate_files {
                        return ValidatedResult {
                            verdict: Verdict::Unknown {
                                reason: format!("external certificate rejected: {why}"),
                            },
                            witness: None,
                            note: Some(format!("certificate file rejected: {why}")),
                        };
                    }
                }
            }
        }
    }

    // 4. Fallback: stdout witness extraction when no file artifact was written.
    let witness = if !matches!(verdict, Verdict::Unsafe { .. }) {
        None
    } else if cex_witness && !staged {
        match convert_and_replay_witness(stdout, model, format) {
            Ok((w, depth)) => {
                let verdict = match verdict {
                    Verdict::Unsafe { depth: None } => Verdict::Unsafe {
                        depth: Some(depth as u32),
                    },
                    other => other,
                };
                return ValidatedResult {
                    verdict,
                    witness: Some(w),
                    note: None,
                };
            }
            Err(e) => {
                return ValidatedResult {
                    verdict,
                    witness: None,
                    note: Some(format!("witness not used: {e}")),
                };
            }
        }
    } else {
        witness_on_stdout.then(|| stdout.to_string())
    };

    ValidatedResult {
        verdict,
        witness,
        note: None,
    }
}

fn read_nonempty(path: Option<&Path>) -> Option<(String, &Path)> {
    let p = path?;
    let meta = std::fs::metadata(p).ok()?;
    if !meta.is_file() || meta.len() == 0 {
        return None;
    }
    let text = std::fs::read_to_string(p).ok()?;
    (!text.trim().is_empty()).then_some((text, p))
}

fn looks_like_witness(text: &str, format: ModelFormat) -> bool {
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with(';'));
    matches!(
        (format, first),
        (ModelFormat::Aiger, Some("1")) | (ModelFormat::Btor2, Some("sat"))
    )
}

/// Convert raw AIGER/Btor2 witness text to `informalw 1` AND simulate-replay
/// it on the original unsimplified model when bit-level load succeeds.
pub fn convert_and_replay_witness(
    raw_source: &str,
    model: &Path,
    format: ModelFormat,
) -> Result<(String, usize), String> {
    let btor2 = format == ModelFormat::Btor2;
    let raw = informal_engine::witness::extract(raw_source, btor2)
        .ok_or_else(|| "no counterexample witness found".to_string())?;
    let (named, g_opt) = if btor2 {
        let text =
            std::fs::read_to_string(model).map_err(|e| format!("{}: {e}", model.display()))?;
        let named = informal_engine::witness::from_btor2(&raw, &text)?;
        // Replay on bit-blasted AIG when the Btor2 model has no arrays.
        let g_opt = informal_aig::load(model).ok();
        (named, g_opt)
    } else {
        let g = informal_aig::load(model).map_err(|e| format!("{}: {e:#}", model.display()))?;
        let name = |l: u32| g.names.get(&var(l)).cloned();
        let inputs: Vec<_> = g.inputs.iter().map(|&i| name(i)).collect();
        let latches: Vec<_> = g.latches.iter().map(|l| name(l.lit)).collect();
        let named = informal_engine::witness::from_aiger(&raw, &inputs, &latches)?;
        (named, Some(g))
    };

    let depth = parse_named_depth(&named).ok_or_else(|| "malformed named witness".to_string())?;
    if let Some(g) = g_opt {
        if !g.bads.is_empty() {
            replay_named_on_aig(&g, &named, depth)?;
        }
    }
    Ok((named, depth))
}

fn parse_named_depth(named: &str) -> Option<usize> {
    for l in named.lines() {
        if let Some(rest) = l.strip_prefix("depth ") {
            return rest.trim().parse().ok();
        }
    }
    None
}

/// Independent bit-level simulation replay of an `informalw 1` named witness
/// against the original unsimplified AIG `g`.
fn replay_named_on_aig(g: &Aig, named: &str, depth: usize) -> Result<(), String> {
    let mut lines = named.lines().map(str::trim).filter(|l| !l.is_empty());
    if lines.next() != Some("informalw 1") {
        return Err("missing informalw 1 header".into());
    }
    let mut init_map: HashMap<String, bool> = HashMap::new();
    let mut frames: Vec<HashMap<String, bool>> = Vec::new();
    let parse_pairs = |rest: &str| -> Result<HashMap<String, bool>, String> {
        let mut m = HashMap::new();
        for kv in rest.split_whitespace() {
            let (k, v) = kv
                .rsplit_once('=')
                .ok_or_else(|| format!("bad witness token `{kv}`"))?;
            m.insert(k.to_string(), v == "1");
        }
        Ok(m)
    };
    for l in lines {
        let (head, rest) = l.split_once(' ').unwrap_or((l, ""));
        match head {
            "depth" => {}
            "init" => init_map = parse_pairs(rest)?,
            h if h.starts_with('@') => frames.push(parse_pairs(rest)?),
            _ => return Err(format!("unexpected witness line `{l}`")),
        }
    }
    if frames.len() != depth + 1 {
        return Err(format!(
            "witness has {} frames, expected {}",
            frames.len(),
            depth + 1
        ));
    }

    let lookup = |m: &HashMap<String, bool>, v: u32, idx: usize, prefix: &str| -> bool {
        if let Some(n) = g.names.get(&v) {
            if let Some(&b) = m.get(n) {
                return b;
            }
        }
        m.get(&format!("{prefix}{idx}")).copied().unwrap_or(false)
    };
    let lit_val = |val: &[bool], l: Lit| val[var(l) as usize] ^ is_neg(l);

    let mut state: Vec<bool> = g
        .latches
        .iter()
        .enumerate()
        .map(|(i, l)| match l.init {
            Init::Zero => false,
            Init::One => true,
            _ => lookup(&init_map, var(l.lit), i, "l"),
        })
        .collect();

    let mut hit_bad = false;
    for (f, frame) in frames.iter().enumerate() {
        let mut val = vec![false; g.nodes.len()];
        let (mut ii, mut li) = (0usize, 0usize);
        for v in 0..g.nodes.len() {
            val[v] = match g.nodes[v] {
                Node::Const => false,
                Node::Input => {
                    let b = lookup(frame, v as u32, ii, "i");
                    ii += 1;
                    b
                }
                Node::Latch => {
                    let b = state[li];
                    li += 1;
                    b
                }
                Node::And(a, b) => lit_val(&val, a) && lit_val(&val, b),
            };
        }
        if let Some(c) = g.constraints.iter().position(|&c| !lit_val(&val, c)) {
            return Err(format!("constraint {c} violated at frame {f}"));
        }
        if f == depth && g.bads.iter().any(|&b| lit_val(&val, b)) {
            hit_bad = true;
        }
        for (i, l) in g.latches.iter().enumerate() {
            state[i] = lit_val(&val, l.next);
        }
    }
    if !hit_bad {
        return Err(format!("bad state not asserted at depth {depth}"));
    }
    Ok(())
}

/// Validate a SAFE proof certificate file (`{cert}`).
fn validate_proof_cert(
    model: &Path,
    cert_path: &Path,
    text: &str,
    format: ModelFormat,
) -> Result<Option<String>, String> {
    match format {
        ModelFormat::Aiger => {
            // 1. Structural AIGER parse check (catches empty/corrupt/truncated output).
            let parsed = informal_aig::load(cert_path)
                .map_err(|e| format!("invalid AIGER certificate: {e:#}"))?;
            if parsed.bads.is_empty() {
                return Err("AIGER certificate declares no bad/output property".into());
            }
            // 2. When `certifaiger` is installed, run independent certificate check!
            if let Some(cf) = find_tool("certifaiger") {
                let out = Command::new(&cf)
                    .arg(model)
                    .arg(cert_path)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output()
                    .map_err(|e| format!("running certifaiger: {e}"))?;
                if !out.status.success() {
                    let msg = String::from_utf8_lossy(&out.stderr);
                    let msg = msg.trim();
                    return Err(format!(
                        "certifaiger rejected certificate (exit {:?}): {msg}",
                        out.status.code()
                    ));
                }
                return Ok(Some("certificate confirmed by certifaiger".into()));
            }
            Ok(Some(
                "AIGER certificate parsed (certifaiger not on PATH)".into(),
            ))
        }
        ModelFormat::Btor2 => {
            // Structural Btor2 proof-certificate validation: must contain valid
            // numbered Btor2 declarations (e.g., `1 sort ...`).
            let has_sort = text.lines().any(|l| {
                let mut w = l.split_whitespace();
                w.next().and_then(|id| id.parse::<u32>().ok()).is_some() && w.next() == Some("sort")
            });
            if !has_sort {
                return Err("Btor2 certificate contains no valid sort declarations".into());
            }
            Ok(Some("Btor2 certificate structurally validated".into()))
        }
    }
}

fn find_tool(name: &str) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("INFORMAL_TOOLS_DIR") {
        let base = PathBuf::from(dir);
        for rel in [
            format!("{name}/build/check"),
            format!("{name}/{name}"),
            name.to_string(),
        ] {
            let cand = base.join(rel);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// Availability and capability report for one configured external engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineProbe {
    pub name: String,
    pub program: String,
    pub available: bool,
    pub path: Option<PathBuf>,
    pub formats: Vec<ModelFormat>,
    pub validates_files: bool,
}

/// Probe which configured external engines have executable binaries on `PATH`.
pub fn probe_specs(specs: &[crate::EngineSpec]) -> Vec<EngineProbe> {
    specs
        .iter()
        .map(|s| {
            let path = find_executable(&s.program);
            EngineProbe {
                name: s.name.clone(),
                program: s.program.clone(),
                available: path.is_some(),
                path,
                formats: s.formats.clone(),
                validates_files: s.validate_files,
            }
        })
        .collect()
}

fn find_executable(program: &str) -> Option<PathBuf> {
    let p = Path::new(program);
    if p.components().count() > 1 {
        return p.is_file().then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(program))
        .find(|cand| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                cand.metadata()
                    .is_ok_and(|m| m.is_file() && (m.permissions().mode() & 0o111) != 0)
            }
            #[cfg(not(unix))]
            {
                cand.is_file()
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_config;
    use informal_engine::{CheckRequest, RunContext};
    use std::time::Duration;

    fn models() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/models")
    }

    #[test]
    fn recovers_unsafe_purely_from_validated_cex_file_without_stdout_pattern() {
        // Fake engine prints NOTHING matching any regex, only writes a valid
        // AIGER witness to `{cex}`.
        let toml = r#"
[[engine]]
name = "file-only-cex"
program = "sh"
args = ["-c", "printf '1\\nb0\\n00\\n1\\n1\\n1\\n0\\n.\\n' > \"$2\"", "sh", "{model}", "{cex}"]
formats = ["aiger"]
finds_bugs = true
validate_files = true
"#;
        let e = parse_config(toml).unwrap().remove(0);
        let req = CheckRequest::new(
            models().join("counter2_unsafe.aag"),
            ModelFormat::Aiger,
            Duration::from_secs(5),
        );
        let r = e
            .check(&req, &RunContext::new(Duration::from_secs(5)))
            .unwrap();
        assert_eq!(r.verdict, Verdict::Unsafe { depth: Some(3) });
        assert!(r.witness.as_deref().unwrap().contains("informalw 1"));
    }

    #[test]
    fn withdraws_unsafe_when_cex_file_fails_replay() {
        // Fake engine claims SAT on stdout, but writes a bogus witness where en=0
        // so the 2-bit counter never reaches 3.
        let toml = r#"
[[engine]]
name = "bogus-cex"
program = "sh"
args = ["-c", "echo SAT; printf '1\\nb0\\n00\\n0\\n0\\n0\\n0\\n.\\n' > \"$2\"", "sh", "{model}", "{cex}"]
formats = ["aiger"]
finds_bugs = true
validate_files = true
[engine.patterns]
unsafe = '(?m)^SAT$'
"#;
        let e = parse_config(toml).unwrap().remove(0);
        let req = CheckRequest::new(
            models().join("counter2_unsafe.aag"),
            ModelFormat::Aiger,
            Duration::from_secs(5),
        );
        let r = e
            .check(&req, &RunContext::new(Duration::from_secs(5)))
            .unwrap();
        assert!(
            matches!(r.verdict, Verdict::Unknown { ref reason } if reason.contains("failed validation")),
            "expected withdrawal, got {:?}",
            r.verdict
        );
        assert!(r.witness.is_none());
    }

    #[test]
    fn withdraws_safe_when_cert_file_is_corrupt() {
        let toml = r#"
[[engine]]
name = "corrupt-cert"
program = "sh"
args = ["-c", "echo UNSAT; echo 'not valid aiger' > \"$2\"", "sh", "{model}", "{cert}"]
formats = ["aiger"]
proves = true
validate_files = true
[engine.patterns]
safe = '(?m)^UNSAT$'
"#;
        let e = parse_config(toml).unwrap().remove(0);
        let req = CheckRequest::new(
            models().join("split_safe.aag"),
            ModelFormat::Aiger,
            Duration::from_secs(5),
        );
        let r = e
            .check(&req, &RunContext::new(Duration::from_secs(5)))
            .unwrap();
        assert!(
            matches!(r.verdict, Verdict::Unknown { ref reason } if reason.contains("certificate rejected")),
            "expected withdrawal, got {:?}",
            r.verdict
        );
    }

    #[test]
    fn ric3_cert_file_validation_works_end_to_end() {
        if find_executable("ric3").is_none() {
            informal_engine::tool_missing("ric3 binary not on PATH");
            return;
        }
        let cfg = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/engines.toml");
        let engines = crate::load(&cfg).unwrap();
        let ric3 = engines
            .iter()
            .find(|e| e.name() == "ric3-ic3")
            .expect("ric3-ic3 in config/engines.toml");

        let safe_req = CheckRequest::new(
            models().join("split_safe.aag"),
            ModelFormat::Aiger,
            Duration::from_secs(10),
        );
        let safe_res = ric3
            .check(&safe_req, &RunContext::new(Duration::from_secs(10)))
            .unwrap();
        assert_eq!(safe_res.verdict, Verdict::Safe);

        let unsafe_req = CheckRequest::new(
            models().join("counter2_unsafe.aag"),
            ModelFormat::Aiger,
            Duration::from_secs(10),
        );
        let unsafe_res = ric3
            .check(&unsafe_req, &RunContext::new(Duration::from_secs(10)))
            .unwrap();
        assert_eq!(unsafe_res.verdict, Verdict::Unsafe { depth: Some(3) });
        assert!(unsafe_res
            .witness
            .as_deref()
            .is_some_and(|w| w.starts_with("informalw 1\ndepth 3")));
    }
}
