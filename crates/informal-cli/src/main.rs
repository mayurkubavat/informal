//! `informal`: command-line entry point.
//!
//!   informal info  <model.btor2>                      parse, validate, print features
//!   informal fmt   <model.btor2>                      print canonical Btor2
//!   informal check <model> [--engines F] [--timeout S]  run the portfolio
//!                  [--certificate DIR]                   and write a checkable certificate
//!   informal simp  <model>                          bit-level model before / after preprocessing
//!   informal lower <files.sv> [--top T] [-D X]     SystemVerilog to Btor2 (svlang)
//!   informal shell [script.tcl] [--flavor jg|vcf]     JasperGold / VC Formal style Tcl
//!                  [--certificate DIR]                   and a certificate per property
//!   informal mcp [--mode M] [--root DIR]              serve one FPV session to an AI model
//!   informal reduce <model.aig> (--disagree A,B | --test CMD) [-o small.aag]
//!   informal split <model.aig> [--timeout S] [--jobs N]   prove a wide property term by term
//!   informal bench <dir>   [--engines F] [--timeout S] [--each] [--jobs N] [--db runs.jsonl]

mod split;

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use informal_engine::{portfolio, CheckRequest, ModelFormat, Verdict};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "informal",
    version,
    about = "InFormal: formal verification orchestrator"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
    /// Memory budget for in-process engines, in GB (0 = none). Default:
    /// 90% of the cgroup's memory.max (or memory.high) when it has one,
    /// else 75% of physical memory. Over it, they stop with unknown
    /// "memory"; 10% over it (or near the cgroup limit), informal exits with
    /// a memout (status 3).
    #[arg(long, global = true)]
    mem_gb: Option<f64>,
    /// Run each in-house engine in a process of its own (also with the
    /// environment variable INFORMAL_ISOLATE): an engine that runs out of
    /// memory or crashes ends alone. Each then loads the model itself.
    #[arg(long, global = true)]
    isolate: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Parse and validate a Btor2 model and print its features as JSON.
    Info { model: PathBuf },
    /// Run one in-house engine on a model and print progress and the
    /// result as JSON lines (the child side of --isolate).
    #[command(hide = true)]
    Engine {
        name: String,
        model: PathBuf,
        #[arg(long, default_value_t = 60_000)]
        timeout_ms: u64,
    },
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
        /// Write the verdict's certificate into this directory, for a
        /// checker that shares no code with InFormal: a counterexample as
        /// an AIGER witness (aigsim), a proof as a witness circuit
        /// (Certifaiger), and `<model>.cert.json`, which says what was
        /// written (see docs/certificates.md).
        #[arg(long, value_name = "DIR")]
        certificate: Option<PathBuf>,
    },
    /// Load a model at bit level (AIGER, or Btor2 bit-blasted), preprocess
    /// it and print its size before and after; `--aag` prints the result.
    Simp {
        model: PathBuf,
        #[arg(long)]
        aag: bool,
    },
    /// Lower SystemVerilog to Btor2 with svlang; immediate assertions
    /// become bad states.
    Lower {
        files: Vec<PathBuf>,
        #[arg(long)]
        top: Option<String>,
        /// Preprocessor defines (NAME or NAME=VALUE).
        #[arg(short = 'D')]
        define: Vec<String>,
        /// Leave out assertions in initial blocks that check a signal at
        /// time zero (otherwise not supported).
        #[arg(long)]
        skip_initial_checks: bool,
        /// Lower $isunknown as 0 (otherwise not supported).
        #[arg(long)]
        isunknown_is_false: bool,
    },
    /// Run a JasperGold or VC Formal style Tcl script (or an interactive
    /// shell when no script is given).
    Shell {
        script: Option<PathBuf>,
        /// Command set: jg or vcf (default: detected from the script).
        #[arg(long)]
        flavor: Option<String>,
        #[arg(long, default_value = "config/engines.toml")]
        engines: PathBuf,
        /// Scratch directory for generated models.
        #[arg(long, default_value = "informal_work")]
        workdir: PathBuf,
        /// Evaluate this Tcl before the script (repeatable).
        #[arg(short = 'c', long = "command")]
        commands: Vec<String>,
        /// Check the script instead of running it: nothing is elaborated
        /// or proved; every command and option informal would reject,
        /// ignore or does not know is listed with its line. Exit status 1
        /// when the script would not run to its end.
        #[arg(long)]
        check: bool,
        /// With --check: print the report as JSON.
        #[arg(long, requires = "check")]
        json: bool,
        /// Write the certificate of every property the in-house engines
        /// settle into this directory, as `check --certificate` does for a
        /// model (docs/certificates.md).
        #[arg(long, value_name = "DIR", conflicts_with = "check")]
        certificate: Option<PathBuf>,
    },
    /// Serve one FPV session to an AI model as Model Context Protocol
    /// tools on standard input and output (docs/ai-interface.md). Replies
    /// are JSON; what the session prints goes to standard error.
    Mcp {
        /// What the model may do: read-only (read and prove), propose
        /// (changes wait for a person) or auto (changes apply at once,
        /// tagged AI-added; for an evaluation sandbox).
        #[arg(long, default_value = "propose")]
        mode: String,
        /// Scripts and design files are only opened below this directory.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long, default_value = "config/engines.toml")]
        engines: PathBuf,
        /// Scratch directory for generated models and traces.
        #[arg(long, default_value = "informal_work")]
        workdir: PathBuf,
        /// The longest time limit, in seconds, a prove call may ask for.
        #[arg(long, default_value_t = 300)]
        max_time: u64,
        /// Do not start a session: connect to the one a running desktop
        /// app serves and pass the messages through, so the model works
        /// on the session a person is looking at. Without a socket path,
        /// the most recently started app's. The app sets the mode; the
        /// other options are not used.
        #[arg(long, value_name = "SOCKET", num_args = 0..=1, default_missing_value = "")]
        connect: Option<PathBuf>,
    },
    /// Shrink an AIGER model while two engines keep disagreeing on it, or
    /// while a test command keeps exiting 0 (delta debugging).
    Reduce {
        model: PathBuf,
        /// Two engine names; the model is kept interesting while they give
        /// the same two conflicting definitive verdicts as on the original.
        #[arg(long, value_delimiter = ',', num_args = 1)]
        disagree: Vec<String>,
        /// A shell command run on each candidate; `{}` is replaced by the
        /// candidate's path (else the path is appended). Exit 0 = keep.
        #[arg(long)]
        test: Option<String>,
        #[arg(long, default_value = "config/engines.toml")]
        engines: PathBuf,
        /// Seconds per engine run (with --disagree).
        #[arg(long, default_value_t = 10)]
        timeout: u64,
        /// Where the reduced model is written (ASCII AIGER).
        #[arg(short, long, default_value = "reduced.aag")]
        out: PathBuf,
        /// Scratch directory for candidates.
        #[arg(long, default_value = "informal_work/reduce")]
        workdir: PathBuf,
    },
    /// Prove a property whose bad output is an OR of several terms part by
    /// part: one model per term, proved a few at a time with the
    /// invariant store carrying each certified invariant to the others,
    /// then the original with all of them (see docs/engines.md).
    Split {
        model: PathBuf,
        /// Wall-clock budget in seconds, for the terms and the original.
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        /// Terms proved at once.
        #[arg(long, default_value_t = 4)]
        jobs: usize,
        /// Open OR nodes until there are this many terms.
        #[arg(long, default_value_t = 256)]
        max_terms: usize,
        /// Seconds per term in the first round; doubles every round.
        #[arg(long, default_value_t = 10)]
        slice: u64,
        /// Scratch directory for the term models and the private store.
        #[arg(long, default_value = "informal_work/split")]
        workdir: PathBuf,
        /// Only write the term models (kept in the scratch directory).
        #[arg(long)]
        write_only: bool,
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
    /// Run engines over models in batch mode.
    Batch {
        script: PathBuf,
        #[arg(long, default_value = "config/engines.toml")]
        engines: PathBuf,
        #[arg(long, default_value_t = 60)]
        timeout: u64,
    },
}

/// The engines built into this binary; with `isolate`, each as a child
/// process of it (informal_engines_ext::isolated).
fn in_house(isolate: bool) -> anyhow::Result<Vec<std::sync::Arc<dyn informal_engine::Engine>>> {
    let mut engines = informal_bmc::engines();
    engines.extend(informal_bitwuzla::engines());
    if !isolate {
        return Ok(engines);
    }
    let exe = std::env::current_exe().context("finding this binary for --isolate")?;
    Ok(informal_engines_ext::isolated::wrap(engines, &exe))
}

fn load_engines(
    path: &Path,
    only: &[String],
    isolate: bool,
) -> anyhow::Result<Vec<std::sync::Arc<dyn informal_engine::Engine>>> {
    let mut engines = informal_engines_ext::load(path)?;
    engines.extend(in_house(isolate)?);
    if !only.is_empty() {
        engines.retain(|e| only.iter().any(|n| n == e.name()));
        if engines.is_empty() {
            bail!("none of {only:?} is defined in {}", path.display());
        }
    }
    Ok(engines)
}

/// Where the protocol's messages go: the process's standard output, which
/// from here on is no longer file descriptor 1. Tcl's `puts`, the report
/// tables and anything an engine prints write there, and would corrupt the
/// message stream, so descriptor 1 now leads to standard error.
#[cfg(unix)]
fn protocol_output() -> anyhow::Result<std::fs::File> {
    use std::os::fd::FromRawFd;
    // SAFETY: plain descriptor calls; `saved` is a fresh descriptor that
    // nothing else owns, so the File may own it.
    unsafe {
        let saved = libc::dup(1);
        if saved < 0 || libc::dup2(2, 1) < 0 {
            bail!("cannot redirect standard output");
        }
        Ok(std::fs::File::from_raw_fd(saved))
    }
}

/// `informal mcp --connect`: standard input to the socket, the socket to
/// standard output, until either side ends.
#[cfg(unix)]
fn relay(socket: &Path) -> anyhow::Result<()> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    /// Plain reads and writes, each piece passed on at once (the messages
    /// are lines, and the other side waits for each).
    fn pass(mut from: impl Read, mut to: impl Write) -> std::io::Result<()> {
        let mut buf = [0u8; 64 * 1024];
        loop {
            match from.read(&mut buf)? {
                0 => return Ok(()),
                n => {
                    to.write_all(&buf[..n])?;
                    to.flush()?;
                }
            }
        }
    }
    let stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    let to_app = stream.try_clone()?;
    std::thread::spawn(move || {
        let _ = pass(std::io::stdin().lock(), &to_app);
        // End of input: tell the app, which then ends its side.
        let _ = to_app.shutdown(std::net::Shutdown::Write);
    });
    pass(stream, std::io::stdout().lock())?;
    Ok(())
}

#[cfg(not(unix))]
fn relay(_socket: &Path) -> anyhow::Result<()> {
    bail!("--connect needs a Unix socket")
}

#[cfg(not(unix))]
fn protocol_output() -> anyhow::Result<std::io::Stdout> {
    Ok(std::io::stdout())
}

fn read_btor2(path: &Path) -> anyhow::Result<informal_ir::Model> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(informal_ir::parse(&text)?)
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let budget = match cli.mem_gb {
        Some(gb) => (gb * 1e9) as u64,
        None => informal_engine::memory::default_budget(
            informal_engine::memory::cgroup().map(|c| c.max),
            informal_engine::memory::total_bytes(),
        ),
    };
    informal_engine::memory::set_budget(budget);
    let isolate = cli.isolate || std::env::var_os("INFORMAL_ISOLATE").is_some();
    match cli.cmd {
        Cmd::Engine {
            name,
            model,
            timeout_ms,
        } => {
            let engine = in_house(false)?
                .into_iter()
                .find(|e| e.name() == name)
                .with_context(|| format!("no in-house engine named {name}"))?;
            let format = informal_engine::ModelFormat::of_path(&model)
                .with_context(|| format!("unknown model format: {}", model.display()))?;
            let req = informal_engine::CheckRequest {
                model,
                format,
                timeout: Duration::from_millis(timeout_ms),
            };
            informal_engines_ext::isolated::serve(engine.as_ref(), &req)?;
        }
        Cmd::Info { model } => {
            let m = read_btor2(&model)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&informal_ir::Features::of(&m))?
            );
        }
        Cmd::Fmt { model } => print!("{}", informal_ir::print(&read_btor2(&model)?)),
        Cmd::Check {
            model,
            engines,
            timeout,
            only,
            certificate,
        } => {
            let format = ModelFormat::of_path(&model)
                .context("unknown model extension (expected .btor2, .btor, .aig, .aag)")?;
            if format == ModelFormat::Btor2 {
                read_btor2(&model)?; // fail fast on malformed input
            }
            let engines = load_engines(&engines, &only, isolate)?;
            let req = CheckRequest {
                model,
                format,
                timeout: Duration::from_secs(timeout),
            };
            informal_engine::memory::on_memout(|| println!("UNKNOWN (memout)"));
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
            if let (Some(dir), Some(w), true) = (&certificate, &o.winner, v.is_definitive()) {
                let s = informal_bmc::certificate::write(
                    dir,
                    &req.model,
                    &w.engine,
                    &v,
                    w.certificate.as_ref(),
                )?;
                println!("{}", s.line());
            }
            std::process::exit(match v {
                Verdict::Unsafe { .. } => 10,
                Verdict::Safe => 20,
                Verdict::Unknown { .. } => 0,
            });
        }
        Cmd::Split {
            model,
            timeout,
            jobs,
            max_terms,
            slice,
            workdir,
            write_only,
        } => {
            let o = split::Options {
                model,
                timeout: Duration::from_secs(timeout),
                jobs,
                max_terms,
                slice: Duration::from_secs(slice.max(1)),
                workdir,
                write_only,
            };
            let out = split::run(&o, &in_house(false)?);
            split::clean(&o);
            let out = out?;
            for (i, t) in out.terms.iter().enumerate() {
                if write_only {
                    println!("{}", t.file.display());
                } else {
                    eprintln!(
                        "  term {i:<4} {:<8} round {} {:>8} ms",
                        t.verdict.label(),
                        t.round,
                        t.wall_ms
                    );
                }
            }
            if write_only {
                return Ok(());
            }
            let proved = out
                .terms
                .iter()
                .filter(|t| t.verdict == Verdict::Safe)
                .count();
            let n = out.terms.len();
            match &out.verdict {
                Verdict::Unsafe { depth } => println!(
                    "UNSAFE (by {}, depth {})",
                    out.by,
                    depth.map_or("?".into(), |d| d.to_string())
                ),
                Verdict::Safe => println!("SAFE (by {}, {n} terms)", out.by),
                Verdict::Unknown { reason } => {
                    println!("UNKNOWN ({reason}; {proved} of {n} terms proved)")
                }
            }
            std::process::exit(match out.verdict {
                Verdict::Unsafe { .. } => 10,
                Verdict::Safe => 20,
                Verdict::Unknown { .. } => 0,
            });
        }
        Cmd::Simp { model, aag } => {
            let t0 = std::time::Instant::now();
            let g = informal_bmc::load_aig(&model)?;
            let t1 = t0.elapsed();
            let (s, st) = informal_aig::preprocess::simplify(&g);
            eprintln!(
                "loaded    {} ({:.1} ms)",
                g.summary(),
                t1.as_secs_f64() * 1e3
            );
            eprintln!(
                "simplified {} ({:.1} ms, {} constant latches)",
                s.summary(),
                (t0.elapsed() - t1).as_secs_f64() * 1e3,
                st.constant_latches
            );
            if aag {
                print!("{}", informal_aig::aiger::write_ascii(&s));
            }
        }
        Cmd::Reduce {
            model,
            disagree,
            test,
            engines,
            timeout,
            out,
            workdir,
        } => reduce(
            &model,
            &disagree,
            test.as_deref(),
            &engines,
            timeout,
            &out,
            &workdir,
        )?,
        Cmd::Lower {
            files,
            top,
            define,
            skip_initial_checks,
            isunknown_is_false,
        } => {
            let req = informal_svlang::Request {
                files,
                top,
                defines: define
                    .iter()
                    .map(|d| {
                        d.split_once('=')
                            .map_or((d.clone(), None), |(k, v)| (k.into(), Some(v.into())))
                    })
                    .collect(),
                skip_initial_checks,
                isunknown_is_false,
                ..Default::default()
            };
            let l = informal_svlang::lower(&req);
            for n in &l.notes {
                eprintln!("note: {n}");
            }
            if !l.ok {
                bail!("{}", l.errors.join("\n"));
            }
            print!("{}", l.btor2);
        }
        Cmd::Batch {
            script,
            engines,
            timeout,
        } => {
            let text = std::fs::read_to_string(&script)
                .with_context(|| format!("reading {}", script.display()))?;
            let flavor = informal_fpv::detect_flavor(&text);
            let mut engines = informal_engines_ext::load(&engines)?;
            engines.extend(in_house(isolate)?);
            let mut session = informal_fpv::Session::new(engines, PathBuf::from("informal_work"));
            session.time_limit = Duration::from_secs(timeout);
            let shell = informal_fpv::Shell::new(session, flavor);
            if let Err(e) = shell.source(&script) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Cmd::Shell {
            script,
            flavor,
            engines,
            workdir,
            commands,
            check,
            json,
            certificate,
        } => {
            let text = match &script {
                Some(p) => std::fs::read_to_string(p)
                    .with_context(|| format!("reading {}", p.display()))?,
                None => String::new(),
            };
            let flavor = match flavor.as_deref() {
                Some(f) => informal_fpv::Flavor::parse(f)
                    .with_context(|| format!("unknown flavor {f} (jg or vcf)"))?,
                None => informal_fpv::detect_flavor(&text),
            };
            if check {
                let script = script.context("--check needs a script")?;
                let session = informal_fpv::Session::new(Vec::new(), workdir);
                let shell = informal_fpv::Shell::new(session, flavor);
                let report = shell.check(&script);
                if json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    print!("{}", report.text());
                }
                std::process::exit(if report.clean() { 0 } else { 1 });
            }
            let mut engines = informal_engines_ext::load(&engines)?;
            engines.extend(in_house(isolate)?);
            let mut session = informal_fpv::Session::new(engines, workdir);
            session.certificates = certificate;
            let shell = informal_fpv::Shell::new(session, flavor);
            for c in &commands {
                if let Err(e) = shell.eval(c) {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
            match &script {
                Some(p) => {
                    if let Err(e) = shell.source(p) {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                }
                None if commands.is_empty() => repl(&shell)?,
                None => {}
            }
        }
        Cmd::Mcp {
            mode,
            root,
            engines,
            workdir,
            max_time,
            connect,
        } => {
            if let Some(socket) = connect {
                let socket = match socket.as_os_str().is_empty() {
                    false => socket,
                    true => informal_mcp::socket::newest().with_context(|| {
                        format!(
                            "no desktop app is serving a session (no socket in {})",
                            informal_mcp::socket::dir().display()
                        )
                    })?,
                };
                return relay(&socket);
            }
            let mode = informal_mcp::Mode::parse(&mode)
                .with_context(|| format!("unknown mode {mode} (read-only, propose or auto)"))?;
            // The external engines are optional here: without their
            // configuration the in-house ones serve.
            let mut all = match engines.is_file() {
                true => informal_engines_ext::load(&engines)?,
                false => {
                    eprintln!(
                        "informal: no {}: using the in-house engines only",
                        engines.display()
                    );
                    Vec::new()
                }
            };
            all.extend(in_house(isolate)?);
            let out = protocol_output()?;
            let api = informal_mcp::Api::spawn(
                all,
                workdir.clone(),
                informal_mcp::Options {
                    mode,
                    root,
                    workdir,
                    max_time: Duration::from_secs(max_time.max(1)),
                },
            );
            informal_mcp::serve(std::sync::Arc::new(api), std::io::stdin().lock(), out)?;
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
            let engines = load_engines(&engines, &only, isolate)?;
            let models = informal_bench::collect_models(&dir)?;
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
            // On a memout, record the runs in flight before exiting.
            let memout_db = db.clone();
            informal_engine::memory::on_memout(move || {
                let _ = informal_bench::write_memouts(&memout_db);
            });
            let mode = if each {
                informal_bench::Mode::EachEngine
            } else {
                informal_bench::Mode::Portfolio
            };
            eprintln!(
                "{} models, {} engines, {timeout}s timeout, {jobs} jobs -> {}",
                models.len(),
                engines.len(),
                db.display()
            );
            let recs = informal_bench::run(
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
            for (name, s) in informal_bench::score(&recs) {
                println!(
                    "{name:<14} {:>5} {:>5} {:>7} {:>7} {:>5} {:>10.1}",
                    s.runs, s.safe, s.unsafe_, s.unknown, s.error, s.par2_s
                );
            }
        }
    }
    Ok(())
}

/// A minimal interactive prompt: reads lines until a command is complete.
fn repl(shell: &informal_fpv::Shell) -> anyhow::Result<()> {
    use std::io::{BufRead, Write};
    let flavor = shell.interp.get_var("informal_flavor").unwrap_or_default();
    let stdin = std::io::stdin();
    let mut buf = String::new();
    loop {
        print!(
            "{}",
            if buf.is_empty() {
                format!("informal[{flavor}]> ")
            } else {
                "... ".into()
            }
        );
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            println!();
            return Ok(());
        }
        buf += &line;
        if !informal_tcl::Interp::is_complete(&buf) {
            continue;
        }
        match shell.eval(&buf) {
            Ok(r) if !r.is_empty() => println!("{r}"),
            Ok(_) => {}
            Err(e) => eprintln!("error: {}", e.message),
        }
        buf.clear();
    }
}

/// `informal reduce`: see [`informal_aig::reduce`].
fn reduce(
    model: &Path,
    disagree: &[String],
    test: Option<&str>,
    engines: &Path,
    timeout: u64,
    out: &Path,
    workdir: &Path,
) -> anyhow::Result<()> {
    use informal_aig::reduce;
    if !matches!(
        model.extension().and_then(|e| e.to_str()),
        Some("aig" | "aag")
    ) {
        // A bit-blasted Btor2 model may carry non-constant initial values,
        // which AIGER cannot hold: the candidates would mean something else.
        bail!("reduce takes AIGER models (.aig, .aag)");
    }
    let g = informal_aig::load(model)?;
    std::fs::create_dir_all(workdir)?;
    let cand = workdir.join("candidate.aig");
    let write = |h: &informal_aig::Aig| std::fs::write(&cand, informal_aig::aiger::write_binary(h));

    let mut check: Box<dyn FnMut(&informal_aig::Aig) -> bool> = match (test, disagree.len()) {
        (Some(cmd), 0) => {
            let cmd = if cmd.contains("{}") {
                cmd.replace("{}", &cand.display().to_string())
            } else {
                format!("{cmd} {}", cand.display())
            };
            Box::new(move |h| {
                write(h).is_ok()
                    && std::process::Command::new("sh")
                        .arg("-c")
                        .arg(&cmd)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()
                        .is_ok_and(|s| s.success())
            })
        }
        (None, 2) => {
            let pair = load_engines(engines, disagree, false)?;
            let pair: Vec<_> = disagree
                .iter()
                .map(|n| pair.iter().find(|e| e.name() == n).cloned())
                .collect::<Option<_>>()
                .with_context(|| format!("engines {disagree:?} not both found"))?;
            let req = CheckRequest {
                model: cand.clone(),
                format: ModelFormat::Aiger,
                timeout: Duration::from_secs(timeout),
            };
            let run = move |h: &informal_aig::Aig| -> Option<(Verdict, Verdict)> {
                write(h).ok()?;
                let mut v = Vec::new();
                for e in &pair {
                    let ctx = informal_engine::RunContext::new(req.timeout);
                    let r = e.check(&req, &ctx).ok()?;
                    if !r.verdict.is_definitive() {
                        return None;
                    }
                    v.push(r.verdict);
                }
                let b = v.pop()?;
                Some((v.pop()?, b))
            };
            let kind = |v: &Verdict| matches!(v, Verdict::Safe);
            let (a0, b0) = run(&g)
                .context("the engines do not both give a definitive verdict on the original")?;
            if kind(&a0) == kind(&b0) {
                bail!(
                    "{} and {} agree on the original ({})",
                    disagree[0],
                    disagree[1],
                    a0.label()
                );
            }
            eprintln!(
                "original: {} says {}, {} says {}",
                disagree[0],
                a0.label(),
                disagree[1],
                b0.label()
            );
            Box::new(move |h| {
                run(h).is_some_and(|(a, b)| kind(&a) == kind(&a0) && kind(&b) == kind(&b0))
            })
        }
        _ => bail!("give either --disagree A,B or --test CMD"),
    };
    if test.is_some() && !check(&g) {
        bail!("the test command does not succeed on the original model");
    }
    let (i, l, a) = reduce::size(&g);
    eprintln!("original: {i} inputs, {l} latches, {a} ANDs");
    let (h, tests) = reduce::reduce(&g, &mut check, |m| eprintln!("  {m}"));
    let (i, l, a) = reduce::size(&h);
    std::fs::write(out, informal_aig::aiger::write_ascii(&h))?;
    eprintln!(
        "reduced:  {i} inputs, {l} latches, {a} ANDs after {tests} tests -> {}",
        out.display()
    );
    Ok(())
}
