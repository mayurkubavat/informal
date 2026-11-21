//! In-house engines as child processes (decision D15).
//!
//! [`wrap`] turns an in-process engine into one that runs
//! `informal engine <name> <model>` in a process of its own. The child
//! writes JSON lines on stdout ([`Line`]): progress while it runs, then one
//! result. What that buys:
//!
//! - **Memory:** an engine that runs out ends alone. Each child gets the
//!   budget as a kernel limit (a systemd scope with `MemoryMax`, when user
//!   scopes work here) and as its own in-process budget (exit status 3).
//!   Across children, a monitor keeps the sum of their resident memory
//!   under the budget by killing the one with the highest shed rank, which
//!   frees its memory at once.
//! - **Crashes:** a fault in an FFI library ends one engine, not the session.
//! - **Plug-ins:** the line protocol is all a new engine has to speak.
//!
//! What it costs: every child loads and prepares the model itself, so the
//! model and CNF template that in-process engines share are built once per
//! engine. It is opt-in (`--isolate`) until measured.

use crate::{kill_tree, scopes_available};
use anyhow::Context;
use informal_engine::{
    memory, Capabilities, CheckRequest, Engine, EngineResult, Progress, RunContext, Verdict,
};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// One line of a child's stdout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Line {
    Progress {
        bound: Option<u32>,
        #[serde(default)]
        note: String,
    },
    Result(WireResult),
}

/// A verdict on the wire: `verdict` is `safe`, `unsafe` or `unknown`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireResult {
    pub verdict: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<informal_engine::Certificate>,
}

impl WireResult {
    pub fn of(r: &EngineResult) -> Self {
        let (verdict, depth, reason) = match &r.verdict {
            Verdict::Safe => ("safe", None, None),
            Verdict::Unsafe { depth } => ("unsafe", *depth, None),
            Verdict::Unknown { reason } => ("unknown", None, Some(reason.clone())),
        };
        WireResult {
            verdict: verdict.into(),
            depth,
            reason,
            witness: r.witness.clone(),
            certificate: r.certificate.clone(),
        }
    }

    fn verdict(&self) -> Verdict {
        match self.verdict.as_str() {
            "safe" => Verdict::Safe,
            "unsafe" => Verdict::Unsafe { depth: self.depth },
            _ => Verdict::Unknown {
                reason: self.reason.clone().unwrap_or_else(|| "unknown".into()),
            },
        }
    }
}

/// The child side: run `engine` on `req` and write the protocol to stdout.
pub fn serve(engine: &dyn Engine, req: &CheckRequest) -> anyhow::Result<()> {
    let put = |l: &Line| {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        let _ = serde_json::to_writer(&mut out, l);
        let _ = out.write_all(b"\n");
        let _ = out.flush();
    };
    let (tx, rx) = std::sync::mpsc::channel::<Progress>();
    let mut ctx = RunContext::new(req.timeout);
    ctx.progress = Some(tx);
    let r = std::thread::scope(|sc| {
        sc.spawn(move || {
            // At most one progress line per 50 ms (BMC reports every
            // frame); the latest one is what the parent wants.
            let mut last = Instant::now() - Duration::from_secs(1);
            let mut held: Option<Line> = None;
            for p in rx {
                if p.output.is_some() {
                    continue;
                }
                held = Some(Line::Progress {
                    bound: p.bound,
                    note: p.note,
                });
                if last.elapsed() >= Duration::from_millis(50) {
                    put(&held.take().expect("just set"));
                    last = Instant::now();
                }
            }
            if let Some(l) = held {
                put(&l);
            }
        });
        let r = engine.check(req, &ctx);
        drop(ctx); // closes the channel, ending the printer
        r
    })?;
    put(&Line::Result(WireResult::of(&r)));
    Ok(())
}

/// Every in-house engine of `engines` as a child process of `exe` (the
/// `informal` binary).
pub fn wrap(engines: Vec<Arc<dyn Engine>>, exe: &Path) -> Vec<Arc<dyn Engine>> {
    engines
        .into_iter()
        .map(|e| {
            Arc::new(Isolated {
                name: e.name().to_string(),
                caps: e.capabilities(),
                rank: e.shed_rank(),
                exe: exe.to_path_buf(),
            }) as Arc<dyn Engine>
        })
        .collect()
}

struct Isolated {
    name: String,
    caps: Capabilities,
    /// The wrapped engine's shed rank, for the monitor.
    rank: u32,
    exe: PathBuf,
}

/// A running child, as the memory monitor sees it.
struct Running {
    pid: u32,
    rank: u32,
    /// Set by the monitor when it kills the child for memory.
    shed: Arc<std::sync::atomic::AtomicBool>,
}

fn running() -> &'static Mutex<Vec<Running>> {
    static R: OnceLock<Mutex<Vec<Running>>> = OnceLock::new();
    R.get_or_init(|| {
        std::thread::Builder::new()
            .name("informal-isolated-mem".into())
            .spawn(monitor)
            .expect("spawn monitor");
        Mutex::new(Vec::new())
    })
}

/// Resident memory of process `pid`, in bytes.
fn rss(pid: u32) -> Option<u64> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
    // SAFETY: plain sysconf call.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    Some(pages * u64::try_from(page).unwrap_or(4096))
}

/// Keep the children's total resident memory under 80% of the budget: over
/// it, kill the child with the highest rank (the largest among equals).
fn monitor() {
    loop {
        std::thread::sleep(Duration::from_millis(200));
        let budget = memory::budget();
        if budget == 0 {
            continue;
        }
        let list = running().lock().unwrap_or_else(|e| e.into_inner());
        let sizes: Vec<(usize, u64)> = list
            .iter()
            .enumerate()
            .filter_map(|(i, r)| rss(r.pid).map(|b| (i, b)))
            .collect();
        let total: u64 = sizes.iter().map(|s| s.1).sum();
        if sizes.len() < 2 || total <= budget / 5 * 4 {
            continue; // a child alone is held by its own limit
        }
        if let Some(&(i, _)) = sizes
            .iter()
            .filter(|(i, _)| list[*i].rank > 0)
            .max_by_key(|(i, b)| (list[*i].rank, *b))
        {
            let r = &list[i];
            if !r.shed.swap(true, std::sync::atomic::Ordering::SeqCst) {
                // SAFETY: a process group we created (see `check`).
                unsafe {
                    libc::killpg(r.pid as i32, libc::SIGKILL);
                }
            }
        }
    }
}

impl Engine for Isolated {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }

    /// Its memory is limited on its own (and by the monitor here).
    fn shed_rank(&self) -> u32 {
        0
    }

    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let start = Instant::now();
        let left = ctx.deadline.saturating_duration_since(start);
        let budget = memory::budget();
        let mut cmd = match (budget > 0)
            .then_some(budget)
            .filter(|_| scopes_available())
        {
            Some(b) => {
                let mut c = Command::new("systemd-run");
                c.args(["--user", "--scope", "--quiet", "--collect"])
                    .args(crate::rdev_slice())
                    // Above the child's own budget, so that stops it first.
                    .arg(format!("-pMemoryMax={}M", b / 1_000_000 / 10 * 12))
                    .arg("-pMemorySwapMax=0")
                    .arg(&self.exe);
                c
            }
            None => Command::new(&self.exe),
        };
        cmd.arg("--mem-gb")
            .arg(format!("{}", budget as f64 / 1e9))
            .arg("engine")
            .arg(&self.name)
            .arg(&req.model)
            .arg("--timeout-ms")
            .arg(left.as_millis().to_string())
            .arg("--effort")
            .arg(req.effort.as_str());
        if let Some(md) = req.max_depth {
            cmd.arg("--max-depth").arg(md.to_string());
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        crate::die_with_parent(&mut cmd);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("starting {} for {}", self.exe.display(), self.name))?;
        let shed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        running()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Running {
                pid: child.id(),
                rank: self.rank,
                shed: shed.clone(),
            });

        // stdout: the protocol. stderr: the engine's own messages, passed on.
        let out = child.stdout.take().expect("piped");
        let err = child.stderr.take().expect("piped");
        let (name, tx) = (self.name.clone(), ctx.progress.clone());
        let out_t = std::thread::spawn(move || {
            let mut result = None;
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                match serde_json::from_str::<Line>(&l) {
                    Ok(Line::Progress { bound, note }) => {
                        if let Some(tx) = &tx {
                            let _ = tx.send(Progress {
                                engine: name.clone(),
                                bound,
                                note,
                                output: None,
                            });
                        }
                    }
                    Ok(Line::Result(r)) => result = Some(r),
                    Err(_) => {} // not ours: ignore
                }
            }
            result
        });
        let (name, tx) = (self.name.clone(), ctx.progress.clone());
        let err_t = std::thread::spawn(move || {
            for l in BufReader::new(err).lines().map_while(Result::ok) {
                match &tx {
                    Some(tx) => {
                        let _ = tx.send(Progress::line(&name, l, true));
                    }
                    None => eprintln!("{l}"),
                }
            }
        });

        // The child has the same time limit: give it a moment past the
        // deadline to report before it is killed.
        let grace = Duration::from_secs(3);
        let status = loop {
            if let Some(st) = child.try_wait()? {
                break Some(st);
            }
            if ctx.cancel.is_cancelled() || Instant::now() >= ctx.deadline + grace {
                kill_tree(&mut child);
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        running()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|r| r.pid != child.id());
        let result = out_t.join().unwrap_or(None);
        let _ = err_t.join();

        let unknown = |reason: &str| Verdict::Unknown {
            reason: reason.into(),
        };
        let was_shed = shed.load(std::sync::atomic::Ordering::SeqCst);
        let mut certificate = None;
        let (verdict, witness) = match (result, status) {
            (Some(r), _) => {
                certificate = r.certificate.clone();
                (r.verdict(), r.witness)
            }
            (None, _) if was_shed => (unknown("memory"), None),
            (None, None) if ctx.cancel.is_cancelled() => (unknown("cancelled"), None),
            (None, None) => (unknown("timeout"), None),
            (None, Some(st)) => {
                use std::os::unix::process::ExitStatusExt;
                let v = match (st.code(), st.signal()) {
                    (Some(c), _) if c == memory::MEMOUT_EXIT => unknown("memory"),
                    // The kernel's limit: an out-of-memory kill.
                    (_, Some(libc::SIGKILL)) => unknown("memory"),
                    (_, Some(sig)) => unknown(&format!("engine process died (signal {sig})")),
                    (c, _) => unknown(&format!("engine process failed (status {c:?})")),
                };
                (v, None)
            }
        };
        Ok(EngineResult {
            engine: self.name.clone(),
            verdict,
            wall_ms: start.elapsed().as_millis(),
            witness,
            certificate,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_round_trip() {
        let p = Line::Progress {
            bound: Some(7),
            note: String::new(),
        };
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(s, r#"{"progress":{"bound":7,"note":""}}"#);
        assert_eq!(serde_json::from_str::<Line>(&s).unwrap(), p);
        let r = Line::Result(WireResult {
            verdict: "unsafe".into(),
            depth: Some(3),
            reason: None,
            witness: Some("1\nb0\n.\n".into()),
            certificate: Some(informal_engine::Certificate::Trace {
                init: "0".into(),
                inputs: vec!["1".into()],
            }),
        });
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<Line>(&s).unwrap(), r);
        let Line::Result(w) = r else { unreachable!() };
        assert_eq!(w.verdict(), Verdict::Unsafe { depth: Some(3) });
    }
}
