//! Non-blocking interactive Tcl REPL backed by [`informal_fpv::Worker`] (D9).
//!
//! Commands run on the dedicated Tcl shell thread so the prompt stays live
//! during a long `prove` or `elaborate`. Fast commands complete inline within a
//! short grace window; long commands continue in the background while read-only
//! queries (`status`, `report`, `get_property_info`, `list_tasks`,
//! `get_design_info`) answer immediately from the live [`SessionSnapshot`] and
//! `stop` cancels the running command without queueing behind it.

use informal_fpv::{Flavor, Reply, Session, SessionSnapshot, Status, Worker};
use std::io::{BufRead, IsTerminal, Write};
use std::sync::mpsc::{self, TryRecvError};
use std::time::Duration;

/// Inline wait window before returning to the prompt for a background command.
const INLINE_GRACE: Duration = Duration::from_millis(50);

/// Run the interactive REPL on a dedicated [`Worker`] shell thread.
pub fn run(session: Session, flavor: Flavor, init_commands: &[String]) -> anyhow::Result<()> {
    let worker = Worker::spawn(flavor, move || session);
    for c in init_commands {
        let r = worker.call(informal_fpv::Request::Eval(c.clone()));
        if !r.ok {
            eprintln!("error: {}", r.result);
            std::process::exit(1);
        }
    }
    let async_mode = std::io::stdin().is_terminal()
        || std::env::var_os("INFORMAL_ASYNC_REPL").is_some_and(|v| v != "0" && !v.is_empty());
    let flavor_label = match flavor {
        Flavor::Jg => "jg",
        Flavor::Vcf => "vcf",
    };
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    run_io(
        &worker,
        flavor_label,
        async_mode,
        stdin.lock(),
        stdout.lock(),
    )
}

/// Core REPL loop parameterized over reader/writer for deterministic testing.
pub fn run_io<R: BufRead, W: Write>(
    worker: &Worker,
    flavor: &str,
    async_mode: bool,
    mut input: R,
    mut out: W,
) -> anyhow::Result<()> {
    let mut buf = String::new();
    let mut inflight: Option<mpsc::Receiver<Reply>> = None;

    loop {
        drain_finished(&mut inflight, &mut out)?;
        let prompt = if !buf.is_empty() {
            "... ".to_string()
        } else if inflight.is_some() {
            format!("informal[{flavor} running]> ")
        } else {
            format!("informal[{flavor}]> ")
        };
        write!(out, "{prompt}")?;
        out.flush()?;

        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            writeln!(out)?;
            wait_inflight(&mut inflight, &mut out)?;
            return Ok(());
        }
        drain_finished(&mut inflight, &mut out)?;

        // While a long command is running in the background, allow instant
        // read-only inspection and cancellation without queueing behind Tcl.
        if inflight.is_some() && buf.is_empty() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if handle_live_command(worker, trimmed, &mut inflight, &mut out)? {
                continue;
            }
            writeln!(
                out,
                "busy: a command is running (type `status`, `report`, `get_property_info <name>`, `wait`, or `stop`)"
            )?;
            out.flush()?;
            continue;
        }

        buf += &line;
        if !informal_tcl::Interp::is_complete(&buf) {
            continue;
        }
        let cmd = std::mem::take(&mut buf);
        let trimmed = cmd.trim();
        if trimmed.is_empty() {
            continue;
        }
        if matches!(trimmed, "exit" | "quit") {
            wait_inflight(&mut inflight, &mut out)?;
            return Ok(());
        }
        let rx = worker.eval_async(cmd);
        if async_mode {
            match rx.recv_timeout(INLINE_GRACE) {
                Ok(reply) => print_reply(&reply, &mut out)?,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    writeln!(
                        out,
                        "[running in background — type `status`, `report`, `get_property_info`, `wait`, or `stop`]"
                    )?;
                    out.flush()?;
                    inflight = Some(rx);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    writeln!(out, "error: shell thread stopped")?;
                }
            }
        } else {
            match rx.recv() {
                Ok(reply) => print_reply(&reply, &mut out)?,
                Err(_) => writeln!(out, "error: shell thread stopped")?,
            }
        }
    }
}

fn drain_finished<W: Write>(
    inflight: &mut Option<mpsc::Receiver<Reply>>,
    out: &mut W,
) -> anyhow::Result<()> {
    let done = match inflight {
        Some(rx) => match rx.try_recv() {
            Ok(reply) => Some(Ok(reply)),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(())),
        },
        None => None,
    };
    if let Some(res) = done {
        *inflight = None;
        match res {
            Ok(reply) => print_reply(&reply, out)?,
            Err(()) => writeln!(out, "error: shell thread stopped")?,
        }
        out.flush()?;
    }
    Ok(())
}

fn wait_inflight<W: Write>(
    inflight: &mut Option<mpsc::Receiver<Reply>>,
    out: &mut W,
) -> anyhow::Result<()> {
    if let Some(rx) = inflight.take() {
        match rx.recv() {
            Ok(reply) => print_reply(&reply, out)?,
            Err(_) => writeln!(out, "error: shell thread stopped")?,
        }
        out.flush()?;
    }
    Ok(())
}

fn print_reply<W: Write>(reply: &Reply, out: &mut W) -> anyhow::Result<()> {
    if reply.ok {
        if !reply.result.is_empty() {
            writeln!(out, "{}", reply.result)?;
        }
    } else {
        writeln!(out, "error: {}", reply.result)?;
    }
    Ok(())
}

/// Handle a read-only inspection or control command while a background prove
/// is running. Returns `true` when handled directly from the live snapshot.
fn handle_live_command<W: Write>(
    worker: &Worker,
    line: &str,
    inflight: &mut Option<mpsc::Receiver<Reply>>,
    out: &mut W,
) -> anyhow::Result<bool> {
    let mut parts = line.split_whitespace();
    let Some(cmd) = parts.next() else {
        return Ok(true);
    };
    let rest: Vec<&str> = parts.collect();
    match cmd {
        "stop" | "prove_stop" => {
            worker.stop();
            writeln!(out, "stop requested")?;
            out.flush()?;
            Ok(true)
        }
        "wait" => {
            wait_inflight(inflight, out)?;
            Ok(true)
        }
        "status" | "prove_status" => {
            let snap = worker.snapshot_live();
            write_live_status(&snap, worker.is_running(), out)?;
            out.flush()?;
            Ok(true)
        }
        "report" => {
            let snap = worker.snapshot_live();
            let csv = rest.contains(&"-csv");
            let task = rest.windows(2).find(|w| w[0] == "-task").map(|w| w[1]);
            let text = if csv {
                snap.report_csv_of(task)
            } else {
                snap.report_of(task)
            };
            write!(out, "{text}")?;
            out.flush()?;
            Ok(true)
        }
        "get_property_info" | "property_get" => {
            let snap = worker.snapshot_live();
            let name = rest.iter().rfind(|a| !a.starts_with('-')).copied();
            match name.and_then(|n| snap.props.iter().find(|p| p.name == n)) {
                Some(p) => {
                    writeln!(
                        out,
                        "{} ({}) status={} task={} expr={{{}}}",
                        p.name,
                        p.kind.label(),
                        p.status.word(),
                        p.task,
                        p.expr
                    )?;
                }
                None => writeln!(out, "error: unknown property")?,
            }
            out.flush()?;
            Ok(true)
        }
        "list_tasks" => {
            let snap = worker.snapshot_live();
            for t in &snap.tasks {
                let mark = if t.name == snap.current_task {
                    "*"
                } else {
                    " "
                };
                writeln!(out, "{mark} {}", t.name)?;
            }
            out.flush()?;
            Ok(true)
        }
        "get_design_info" => {
            let snap = worker.snapshot_live();
            writeln!(
                out,
                "top={} elaborated={} props={} clocks={}",
                snap.design.top.as_deref().unwrap_or("(none)"),
                snap.elaborated,
                snap.props.len(),
                snap.clocks.len()
            )?;
            out.flush()?;
            Ok(true)
        }
        "help" => {
            writeln!(
                out,
                "Live commands during prove: status, report [-csv] [-task <t>], get_property_info <name>, list_tasks, get_design_info, wait, stop"
            )?;
            out.flush()?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn write_live_status<W: Write>(
    snap: &SessionSnapshot,
    running: bool,
    out: &mut W,
) -> anyhow::Result<()> {
    let (mut proven, mut cex, mut covered, mut undet, mut pending) = (0, 0, 0, 0, 0);
    for p in &snap.props {
        match &p.status {
            Status::Proven { .. } | Status::BoundedProven { .. } | Status::Unreachable { .. } => {
                proven += 1
            }
            Status::Cex { .. } => cex += 1,
            Status::Covered { .. } => covered += 1,
            Status::Undetermined { .. } | Status::Error { .. } => undet += 1,
            Status::Unprocessed => pending += 1,
        }
    }
    writeln!(
        out,
        "running={running} total={} proven={proven} cex={cex} covered={covered} undetermined={undet} pending={pending}",
        snap.props.len()
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn live_reads_and_stop_work_while_background_tcl_runs() {
        let w = Worker::spawn(Flavor::Jg, || {
            Session::new(Vec::new(), std::env::temp_dir().join("informal-repl-test"))
        });
        assert!(
            w.call(informal_fpv::Request::Eval(
                "assert -name a_demo {x}".into()
            ))
            .ok
        );
        // Simulate a long-running Tcl command (`after 400`), then immediately run
        // read-only live commands (`status`, `report`, `get_property_info`, `stop`, `wait`).
        let script = "after 300\nstatus\nreport\nget_property_info a_demo\nstop\nwait\n";
        let mut out = Vec::new();
        run_io(&w, "jg", true, Cursor::new(script), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("[running in background"), "{text}");
        assert!(text.contains("running=true total=1"), "{text}");
        assert!(text.contains("a_demo"), "{text}");
        assert!(text.contains("stop requested"), "{text}");
    }
}
