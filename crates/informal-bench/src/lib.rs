//! Benchmark harness and run database.
//!
//! Runs engines over a set of models and appends one JSON line per engine
//! run to the run database (`runs.jsonl`). The same records feed the UI,
//! regression reuse and, from phase 3, training of the engine selector.
//! Phase 0 uses JSON Lines so the file is easy to inspect and load into
//! pandas; a SQLite store can replace it without changing the record.

use anyhow::Context;
use informal_engine::selector::aiger_features;
use informal_engine::{portfolio, CheckRequest, Engine, EngineResult, ModelFormat, Verdict};
use informal_ir::Features;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Runs in progress, so a memout exit can still record them.
static IN_FLIGHT: Mutex<Vec<(u64, Instant, RunRecord)>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

/// Marks `rec` in flight until dropped.
struct InFlight(u64);

impl InFlight {
    fn new(rec: &RunRecord) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed) as u64;
        let mut v = IN_FLIGHT.lock().unwrap_or_else(|p| p.into_inner());
        v.push((id, Instant::now(), rec.clone()));
        InFlight(id)
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        let mut v = IN_FLIGHT.lock().unwrap_or_else(|p| p.into_inner());
        v.retain(|(id, ..)| *id != self.0);
    }
}

/// The runs in flight, as unknown "memout" records.
pub fn memout_records() -> Vec<RunRecord> {
    let Ok(v) = IN_FLIGHT.try_lock() else {
        return Vec::new();
    };
    v.iter()
        .map(|(_, t0, rec)| RunRecord {
            verdict: "unknown".into(),
            detail: Some("memout".into()),
            wall_ms: t0.elapsed().as_millis(),
            ..rec.clone()
        })
        .collect()
}

/// Append [`memout_records`] to the run database at `db` and echo them
/// to stderr (the memory sampler calls this just before a memout exit).
pub fn write_memouts(db: &Path) -> anyhow::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(db)?;
    for rec in memout_records() {
        let mut line = serde_json::to_vec(&rec)?;
        line.push(b'\n');
        f.write_all(&line)?;
        eprintln!(
            "  {:<14} {:<8} {:>8} ms  {}",
            rec.engine, "memout", rec.wall_ms, rec.model
        );
    }
    f.flush()?;
    Ok(())
}

/// One engine run, as stored in the run database.
#[derive(Debug, Clone, Serialize)]
pub struct RunRecord {
    pub ts_unix_ms: u128,
    pub model: String,
    pub model_sha256: String,
    pub format: ModelFormat,
    /// Structural features (Btor2: from the model; AIGER: its header).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub features: Option<Features>,
    /// `single` for one engine alone, `portfolio` for the parallel portfolio.
    pub mode: &'static str,
    pub engine: String,
    /// `safe`, `unsafe`, `unknown` or `error`.
    pub verdict: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub wall_ms: u128,
    pub timeout_ms: u128,
}

impl RunRecord {
    pub fn solved(&self) -> bool {
        self.verdict == "safe" || self.verdict == "unsafe"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Run every engine alone on every model (data for engine comparison
    /// and selector training).
    EachEngine,
    /// Run the parallel portfolio once per model.
    Portfolio,
}

/// Every model file under `dir` (recursively) whose format is known, sorted.
pub fn collect_models(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).with_context(|| format!("reading {}", d.display()))? {
            let p = e?.path();
            if p.is_dir() {
                stack.push(p);
            } else if ModelFormat::of_path(&p).is_some() {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Hash and features for one model file.
pub fn describe(path: &Path) -> anyhow::Result<(String, Option<Features>)> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let sha = format!("{:x}", Sha256::digest(&bytes));
    let features = match ModelFormat::of_path(path) {
        Some(ModelFormat::Btor2) => std::str::from_utf8(&bytes)
            .ok()
            .and_then(|t| informal_ir::parse(t).ok())
            .map(|m| Features::of(&m)),
        Some(ModelFormat::Aiger) => aiger_features(&bytes),
        _ => None,
    };
    Ok((sha, features))
}

/// Run the benchmark and append one record per engine run to `db`.
/// `on_record` sees each record as it lands (for progress output).
/// `jobs` models are checked at once (1 = one after another); records are
/// written as each model finishes, so their order follows completion.
pub fn run(
    models: &[PathBuf],
    engines: &[Arc<dyn Engine>],
    timeout: Duration,
    mode: Mode,
    jobs: usize,
    db: &mut (dyn Write + Send),
    on_record: impl FnMut(&RunRecord) + Send,
) -> anyhow::Result<Vec<RunRecord>> {
    run_selected(models, engines, timeout, mode, jobs, false, db, on_record)
}

/// [`run`], with explicit-filter control for [`Mode::Portfolio`]. When
/// `explicit_filter` is `false`, portfolio mode applies
/// [`informal_engine::selector::select_for_features`] so large models do not
/// oversubscribe CPU cores with overlapping engines.
#[allow(clippy::too_many_arguments)]
pub fn run_selected(
    models: &[PathBuf],
    engines: &[Arc<dyn Engine>],
    timeout: Duration,
    mode: Mode,
    jobs: usize,
    explicit_filter: bool,
    db: &mut (dyn Write + Send),
    on_record: impl FnMut(&RunRecord) + Send,
) -> anyhow::Result<Vec<RunRecord>> {
    let next = AtomicUsize::new(0);
    let sink = Mutex::new((db, on_record, Vec::new()));
    std::thread::scope(|s| {
        let workers: Vec<_> = (0..jobs.clamp(1, models.len().max(1)))
            .map(|_| {
                s.spawn(|| -> anyhow::Result<()> {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(path) = models.get(i) else {
                            return Ok(());
                        };
                        let records = run_model(path, engines, timeout, mode, explicit_filter)?;
                        let mut guard = sink.lock().unwrap_or_else(|p| p.into_inner());
                        let (db, on_record, all) = &mut *guard;
                        for rec in records {
                            serde_json::to_writer(&mut **db, &rec)?;
                            db.write_all(b"\n")?;
                            db.flush()?;
                            on_record(&rec);
                            all.push(rec);
                        }
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .try_for_each(|w| w.join().expect("bench worker panicked"))
    })?;
    Ok(sink.into_inner().unwrap_or_else(|p| p.into_inner()).2)
}

/// Every record for one model: one per engine, or one for the portfolio.
fn run_model(
    path: &Path,
    engines: &[Arc<dyn Engine>],
    timeout: Duration,
    mode: Mode,
    explicit_filter: bool,
) -> anyhow::Result<Vec<RunRecord>> {
    let Some(format) = ModelFormat::of_path(path) else {
        return Ok(Vec::new());
    };
    let (sha, features) = describe(path)?;
    let req = CheckRequest::new(path.to_path_buf(), format, timeout);
    let base = |mode, engine: String| RunRecord {
        ts_unix_ms: now_ms(),
        model: path.display().to_string(),
        model_sha256: sha.clone(),
        format,
        features: features.clone(),
        mode,
        engine,
        verdict: String::new(),
        depth: None,
        detail: None,
        wall_ms: 0,
        timeout_ms: timeout.as_millis(),
    };

    let mut records = Vec::new();
    match mode {
        Mode::EachEngine => {
            for e in engines
                .iter()
                .filter(|e| e.capabilities().formats.contains(&format))
            {
                let ctx = informal_engine::RunContext::new(timeout);
                let mut rec = base("single", e.name().to_string());
                let _in_flight = InFlight::new(&rec);
                fill(
                    &mut rec,
                    e.check(&req, &ctx).map_err(|err| format!("{err:#}")),
                );
                records.push(rec);
            }
        }
        Mode::Portfolio => {
            let mut rec = base("portfolio", "portfolio".into());
            let in_flight = InFlight::new(&rec);
            let fmt_ok: Vec<_> = engines
                .iter()
                .filter(|e| e.capabilities().formats.contains(&format))
                .cloned()
                .collect();
            let plan = match &features {
                Some(f) => informal_engine::selector::plan_for_features(
                    &fmt_ok,
                    f,
                    explicit_filter,
                    req.effort,
                    req.timeout,
                ),
                None => informal_engine::selector::PortfolioPlan {
                    schedule: fmt_ok
                        .into_iter()
                        .map(informal_engine::ScheduledEngine::immediate)
                        .collect(),
                    config: if explicit_filter {
                        informal_engine::ScheduleConfig::unlimited()
                    } else {
                        informal_engine::ScheduleConfig::default_for_host()
                    },
                },
            };
            let o = portfolio::run_scheduled(&plan.schedule, &req, None, None, None, plan.config);
            drop(in_flight);
            let v = portfolio::verdict(&o);
            rec.wall_ms = o.wall_ms;
            fill_verdict(&mut rec, &v);
            // An engine conflict's reason stays in the detail.
            if let (Some(w), true) = (&o.winner, o.conflicts.is_empty()) {
                rec.detail = Some(format!("won by {}", w.engine));
            }
            records.push(rec);
        }
    }
    Ok(records)
}

fn fill(rec: &mut RunRecord, r: Result<EngineResult, String>) {
    match r {
        Ok(res) => {
            rec.wall_ms = res.wall_ms;
            fill_verdict(rec, &res.verdict);
        }
        Err(e) => {
            rec.verdict = "error".into();
            rec.detail = Some(e);
        }
    }
}

fn fill_verdict(rec: &mut RunRecord, v: &Verdict) {
    rec.verdict = v.label().into();
    match v {
        Verdict::Unsafe { depth } => rec.depth = *depth,
        Verdict::Unknown { reason } => rec.detail = Some(reason.clone()),
        Verdict::Safe => {}
    }
}

/// Per-engine score over a set of records.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Score {
    pub runs: usize,
    pub safe: usize,
    pub unsafe_: usize,
    pub unknown: usize,
    pub error: usize,
    /// PAR-2 in seconds: wall time when solved, twice the timeout otherwise.
    pub par2_s: f64,
}

/// Score each engine, plus a `virtual-best` row: for every model, the
/// fastest solving engine. The gap between the best real engine and the
/// virtual best is what the learned selector tries to close.
pub fn score(records: &[RunRecord]) -> BTreeMap<String, Score> {
    let mut out: BTreeMap<String, Score> = BTreeMap::new();
    let mut best: BTreeMap<&str, Option<&RunRecord>> = BTreeMap::new();
    for r in records {
        let s = out.entry(r.engine.clone()).or_default();
        s.runs += 1;
        match r.verdict.as_str() {
            "safe" => s.safe += 1,
            "unsafe" => s.unsafe_ += 1,
            "unknown" => s.unknown += 1,
            _ => s.error += 1,
        }
        s.par2_s += par2(r);
        if r.mode == "single" {
            let slot = best.entry(r.model.as_str()).or_insert(None);
            if r.solved() && slot.is_none_or(|b| r.wall_ms < b.wall_ms) {
                *slot = Some(r);
            }
        }
    }
    if !best.is_empty() && out.len() > 1 {
        let vb = out.entry("virtual-best".into()).or_default();
        for (model, b) in &best {
            vb.runs += 1;
            match b {
                Some(r) => {
                    if r.verdict == "safe" {
                        vb.safe += 1
                    } else {
                        vb.unsafe_ += 1
                    }
                    vb.par2_s += r.wall_ms as f64 / 1000.0;
                }
                None => {
                    vb.unknown += 1;
                    let t = records
                        .iter()
                        .find(|r| r.model == *model)
                        .map_or(0, |r| r.timeout_ms);
                    vb.par2_s += 2.0 * t as f64 / 1000.0;
                }
            }
        }
    }
    out
}

fn par2(r: &RunRecord) -> f64 {
    if r.solved() {
        r.wall_ms as f64 / 1000.0
    } else {
        2.0 * r.timeout_ms as f64 / 1000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use informal_engine::{Capabilities, RunContext};

    struct Fixed(&'static str, Verdict, u128);
    impl Engine for Fixed {
        fn name(&self) -> &str {
            self.0
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                formats: vec![ModelFormat::Btor2],
                ..Default::default()
            }
        }
        fn check(&self, _: &CheckRequest, _: &RunContext) -> anyhow::Result<EngineResult> {
            Ok(EngineResult {
                engine: self.0.into(),
                verdict: self.1.clone(),
                wall_ms: self.2,
                witness: None,
                certificate: None,
            })
        }
    }

    /// The Btor2 sample models (the fake engines only read Btor2).
    fn models() -> Vec<PathBuf> {
        collect_models(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/models"))
            .unwrap()
            .into_iter()
            .filter(|p| ModelFormat::of_path(p) == Some(ModelFormat::Btor2))
            .collect()
    }

    #[test]
    fn logs_one_record_per_engine_run_and_scores_them() {
        let engines: Vec<Arc<dyn Engine>> = vec![
            Arc::new(Fixed("prover", Verdict::Safe, 1_000)),
            Arc::new(Fixed(
                "stuck",
                Verdict::Unknown {
                    reason: "timeout".into(),
                },
                10_000,
            )),
        ];
        let ms = models();
        assert_eq!(ms.len(), 3);
        let mut db = Vec::new();
        let recs = run(
            &ms,
            &engines,
            Duration::from_secs(10),
            Mode::EachEngine,
            1,
            &mut db,
            |_| {},
        )
        .unwrap();
        assert_eq!(recs.len(), 6);
        let lines: Vec<serde_json::Value> = String::from_utf8(db)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 6);
        assert!(lines[0]["features"]["states"].as_u64().is_some());
        assert_eq!(lines[0]["model_sha256"].as_str().unwrap().len(), 64);

        let s = score(&recs);
        assert_eq!(s["prover"].safe, 3);
        assert_eq!(s["prover"].par2_s, 3.0);
        assert_eq!(s["stuck"].par2_s, 60.0);
        assert_eq!(s["virtual-best"].safe, 3);
    }

    #[test]
    fn portfolio_mode_logs_one_record_per_model() {
        let engines: Vec<Arc<dyn Engine>> = vec![Arc::new(Fixed(
            "bmc",
            Verdict::Unsafe { depth: Some(7) },
            5,
        ))];
        let mut db = Vec::new();
        let recs = run(
            &models(),
            &engines,
            Duration::from_secs(10),
            Mode::Portfolio,
            1,
            &mut db,
            |_| {},
        )
        .unwrap();
        assert_eq!(recs.len(), 3);
        assert!(recs
            .iter()
            .all(|r| r.engine == "portfolio" && r.depth == Some(7)));
        assert_eq!(recs[0].detail.as_deref(), Some("won by bmc"));
    }

    #[test]
    fn aiger_header_features() {
        let f = aiger_features(b"aig 10 3 4 0 3 1 2\n...").unwrap();
        assert_eq!(
            (f.inputs, f.states, f.bad, f.constraints, f.nodes),
            (3, 4, 1, 2, 10)
        );
        // AIGER 1.0: outputs are the bad states.
        let f = aiger_features(b"aag 5 1 1 2 3\n").unwrap();
        assert_eq!(f.bad, 2);
        assert!(aiger_features(b"btor 1 2\n").is_none());
    }
}
