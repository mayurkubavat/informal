//! `fdv`: command-line entry point.
//!
//!   fdv info  <model.btor2>                      parse, validate, print features
//!   fdv fmt   <model.btor2>                      print canonical Btor2
//!   fdv check <model> [--engines F] [--timeout S]  run the portfolio
//!   fdv bench <dir>   [--engines F] [--timeout S] [--each] [--jobs N] [--db runs.jsonl]

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use fdv_engine::{portfolio, CheckRequest, ModelFormat, Verdict};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "fdv",
    version,
    about = "Formal DV: formal verification orchestrator"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Parse and validate a Btor2 model and print its features as JSON.
    Info { model: PathBuf },
    /// Print a Btor2 model in canonical form.
    Fmt { model: PathBuf },
    /// Check one model with the engine portfolio.
    Check {
        model: PathBuf,
        #[arg(long, default_value = "config/engines.toml")]
        engines: PathBuf,
        /// Wall-clock budget in seconds.
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        /// Only use these engines (comma-separated names).
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
    },
    /// Run engines over every model in a directory and log to the run database.
    Bench {
        dir: PathBuf,
        #[arg(long, default_value = "config/engines.toml")]
        engines: PathBuf,
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        /// Run every engine alone instead of the portfolio.
        #[arg(long)]
        each: bool,
        #[arg(long, default_value = "bench/runs.jsonl")]
        db: PathBuf,
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
        /// Models checked at once. Keep it low with the rIC3 portfolio,
        /// which already uses 16 threads per model.
        #[arg(long, default_value_t = 1)]
        jobs: usize,
    },
}

fn load_engines(
    path: &Path,
    only: &[String],
) -> anyhow::Result<Vec<std::sync::Arc<dyn fdv_engine::Engine>>> {
    let mut engines = fdv_engines_ext::load(path)?;
    if !only.is_empty() {
        engines.retain(|e| only.iter().any(|n| n == e.name()));
        if engines.is_empty() {
            bail!("none of {only:?} is defined in {}", path.display());
        }
    }
    Ok(engines)
}

fn read_btor2(path: &Path) -> anyhow::Result<fdv_ir::Model> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(fdv_ir::parse(&text)?)
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().cmd {
        Cmd::Info { model } => {
            let m = read_btor2(&model)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&fdv_ir::Features::of(&m))?
            );
        }
        Cmd::Fmt { model } => print!("{}", fdv_ir::print(&read_btor2(&model)?)),
        Cmd::Check {
            model,
            engines,
            timeout,
            only,
        } => {
            let format = ModelFormat::of_path(&model)
                .context("unknown model extension (expected .btor2, .btor, .aig, .aag)")?;
            if format == ModelFormat::Btor2 {
                read_btor2(&model)?; // fail fast on malformed input
            }
            let engines = load_engines(&engines, &only)?;
            let req = CheckRequest {
                model,
                format,
                timeout: Duration::from_secs(timeout),
            };
            let o = portfolio::run(&engines, &req, None);
            for r in &o.runs {
                match r {
                    Ok(r) => eprintln!(
                        "  {:<14} {:<8} {:>8} ms",
                        r.engine,
                        r.verdict.label(),
                        r.wall_ms
                    ),
                    Err(e) => eprintln!("  error: {e}"),
                }
            }
            let v = portfolio::verdict(&o);
            match (&v, &o.winner) {
                (Verdict::Unsafe { depth }, Some(w)) => {
                    println!(
                        "UNSAFE (by {}, depth {})",
                        w.engine,
                        depth.map_or("?".into(), |d| d.to_string())
                    );
                    if let Some(wit) = &w.witness {
                        print!("{wit}");
                    }
                }
                (Verdict::Safe, Some(w)) => println!("SAFE (by {})", w.engine),
                (Verdict::Unknown { reason }, _) => println!("UNKNOWN ({reason})"),
                _ => unreachable!(),
            }
            std::process::exit(match v {
                Verdict::Unsafe { .. } => 10,
                Verdict::Safe => 20,
                Verdict::Unknown { .. } => 0,
            });
        }
        Cmd::Bench {
            dir,
            engines,
            timeout,
            each,
            db,
            only,
            jobs,
        } => {
            let engines = load_engines(&engines, &only)?;
            let models = fdv_bench::collect_models(&dir)?;
            if models.is_empty() {
                bail!("no .btor2/.aig/.aag models under {}", dir.display());
            }
            if let Some(parent) = db.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&db)?;
            let mode = if each {
                fdv_bench::Mode::EachEngine
            } else {
                fdv_bench::Mode::Portfolio
            };
            eprintln!(
                "{} models, {} engines, {timeout}s timeout, {jobs} jobs -> {}",
                models.len(),
                engines.len(),
                db.display()
            );
            let recs = fdv_bench::run(
                &models,
                &engines,
                Duration::from_secs(timeout),
                mode,
                jobs,
                &mut file,
                |r| {
                    eprintln!(
                        "  {:<14} {:<8} {:>8} ms  {}",
                        r.engine, r.verdict, r.wall_ms, r.model
                    );
                },
            )?;
            println!(
                "{:<14} {:>5} {:>5} {:>7} {:>7} {:>5} {:>10}",
                "engine", "runs", "safe", "unsafe", "unknown", "error", "PAR-2 (s)"
            );
            for (name, s) in fdv_bench::score(&recs) {
                println!(
                    "{name:<14} {:>5} {:>5} {:>7} {:>7} {:>5} {:>10.1}",
                    s.runs, s.safe, s.unsafe_, s.unknown, s.error, s.par2_s
                );
            }
        }
    }
    Ok(())
}
