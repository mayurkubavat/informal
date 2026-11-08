//! A process-wide memory budget for in-process engines.
//!
//! External engines run under their own systemd memory caps; in-process
//! engines (and the SAT solvers they link) share this process. A sampler
//! thread measures usage every 50 ms (10 ms near the limit) and acts at
//! two thresholds:
//!
//! - **budget** (soft): [`exceeded`] turns true, every
//!   [`crate::RunContext::should_stop`] says stop, and engines report
//!   unknown with reason "memory".
//! - **hard limit** (budget + 10%, and at most 95% of the cgroup's
//!   `memory.max` or `memory.high`): stopping is cooperative, and engines in a phase that
//!   does not poll (model load, preprocessing, CNF build) can keep
//!   allocating past the budget. Before the cgroup's OOM killer would
//!   SIGKILL the process, the sampler runs the [`on_memout`] hook (the CLI
//!   writes "memout" results) and exits with [`MEMOUT_EXIT`].
//!
//! Usage is the process's resident set, or, when the process sits in a
//! cgroup with its own limit (a `systemd-run -p MemoryMax=` scope),
//! that cgroup's non-file memory: what the limit is enforced on, children
//! included, without the page cache the kernel reclaims first.
//!
//! When several models run in one process, they all stop while it is over
//! budget: run one model per process (scripts/inhouse_capped.sh) to keep
//! the others going.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, Once, OnceLock};
use std::time::Duration;

static BUDGET: AtomicU64 = AtomicU64::new(0);
static HARD: AtomicU64 = AtomicU64::new(0);
static OVER: AtomicBool = AtomicBool::new(false);
static START: Once = Once::new();
static CGROUP: OnceLock<Option<Cgroup>> = OnceLock::new();
type Hook = Box<dyn Fn() + Send + Sync>;
static HOOK: Mutex<Option<Hook>> = Mutex::new(None);

/// Exit status of a process that ran out of memory (`fdv` memout).
pub const MEMOUT_EXIT: i32 = 3;

/// Resident set size of this process in bytes (Linux; `None` elsewhere).
pub fn rss_bytes() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096)
}

/// Total physical memory in bytes (Linux; `None` elsewhere).
pub fn total_bytes() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = s.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

/// The cgroup (v2) this process is in, when it has its own memory limit.
#[derive(Debug, Clone)]
pub struct Cgroup {
    pub dir: PathBuf,
    /// The lower of `memory.max` (OOM kill) and `memory.high` (reclaim
    /// throttling, which without swap stalls the process near the limit).
    pub max: u64,
}

impl Cgroup {
    /// Memory the limit is enforced on, less the reclaimable page cache:
    /// `memory.current` minus `file` in `memory.stat`.
    pub fn usage(&self) -> Option<u64> {
        let cur = parse_u64(&std::fs::read_to_string(self.dir.join("memory.current")).ok()?)?;
        let stat = std::fs::read_to_string(self.dir.join("memory.stat")).ok()?;
        Some(cur.saturating_sub(stat_field(&stat, "file").unwrap_or(0)))
    }
}

/// This process's cgroup, if it has its own memory limit (checked once).
pub fn cgroup() -> Option<&'static Cgroup> {
    CGROUP
        .get_or_init(|| {
            let proc_cgroup = std::fs::read_to_string("/proc/self/cgroup").ok()?;
            let path = cgroup_path(&proc_cgroup)?.trim_start_matches('/');
            let dir = PathBuf::from("/sys/fs/cgroup").join(path);
            let read = |f: &str| {
                std::fs::read_to_string(dir.join(f))
                    .ok()
                    .and_then(|s| parse_max(&s))
            };
            let max = match (read("memory.max"), read("memory.high")) {
                (Some(m), Some(h)) => m.min(h),
                (m, h) => m.or(h)?,
            };
            let cg = Cgroup { dir, max };
            cg.usage()?;
            Some(cg)
        })
        .as_ref()
}

/// The v2 path in `/proc/self/cgroup` (the `0::` line).
fn cgroup_path(proc_cgroup: &str) -> Option<&str> {
    proc_cgroup
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .map(str::trim)
        .filter(|p| !p.is_empty())
}

/// `memory.max` or `memory.high`: a byte count, or `max` (no limit).
fn parse_max(s: &str) -> Option<u64> {
    parse_u64(s).filter(|&b| b > 0)
}

fn parse_u64(s: &str) -> Option<u64> {
    s.trim().parse().ok()
}

fn stat_field(stat: &str, key: &str) -> Option<u64> {
    stat.lines().find_map(|l| {
        let (k, v) = l.split_once(' ')?;
        (k == key).then(|| parse_u64(v)).flatten()
    })
}

/// Memory in use, as measured against the budget.
pub fn usage_bytes() -> Option<u64> {
    match cgroup() {
        Some(cg) => cg.usage(),
        None => rss_bytes(),
    }
}

/// The budget when none is given: 90% of the cgroup limit, else 75% of
/// physical memory, else none (0).
pub fn default_budget(cgroup_max: Option<u64>, total: Option<u64>) -> u64 {
    match (cgroup_max, total) {
        (Some(m), _) => m / 10 * 9,
        (None, Some(t)) => t / 4 * 3,
        (None, None) => 0,
    }
}

/// (budget, hard limit) for a requested budget: the budget is cut to 90%
/// of the cgroup limit, and the hard limit is 10% above it, at most 95%
/// of the cgroup limit.
pub fn limits(budget: u64, cgroup_max: Option<u64>) -> (u64, u64) {
    let budget = cgroup_max.map_or(budget, |m| budget.min(m / 10 * 9));
    let hard = budget.saturating_add(budget / 10);
    let hard = cgroup_max.map_or(hard, |m| hard.min(m / 100 * 95));
    (budget, hard)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Under,
    /// Over the budget: engines stop.
    Over,
    /// At the hard limit: the process exits.
    Memout,
}

pub fn level(usage: u64, budget: u64, hard: u64) -> Level {
    if budget == 0 {
        Level::Under
    } else if usage >= hard {
        Level::Memout
    } else if usage > budget {
        Level::Over
    } else {
        Level::Under
    }
}

/// Run `f` just before a memout exit (for example, to record the runs in
/// flight). Replaces any earlier hook.
pub fn on_memout(f: impl Fn() + Send + Sync + 'static) {
    *HOOK.lock().unwrap_or_else(|p| p.into_inner()) = Some(Box::new(f));
}

fn memout(usage: u64, budget: u64, hard: u64) -> ! {
    OVER.store(true, Ordering::Relaxed);
    let mib = |b: u64| b >> 20;
    eprintln!(
        "fdv: memout: {} MiB in use, hard limit {} MiB (budget {} MiB{}); exiting",
        mib(usage),
        mib(hard),
        mib(budget),
        cgroup().map_or(String::new(), |c| format!(
            ", cgroup limit {} MiB",
            mib(c.max)
        ))
    );
    if let Ok(h) = HOOK.try_lock() {
        if let Some(f) = h.as_ref() {
            f();
        }
    }
    std::process::exit(MEMOUT_EXIT)
}

/// Set the budget in bytes (0 turns it off) and start the sampler.
pub fn set_budget(bytes: u64) {
    let (budget, hard) = limits(bytes, cgroup().map(|c| c.max));
    if bytes > budget {
        eprintln!(
            "fdv: memory budget cut to {} MiB (90% of the cgroup's memory limit)",
            budget >> 20
        );
    }
    let budget = if bytes == 0 { 0 } else { budget };
    HARD.store(hard, Ordering::Relaxed);
    BUDGET.store(budget, Ordering::Relaxed);
    if budget == 0 || usage_bytes().is_none() {
        OVER.store(false, Ordering::Relaxed);
        return;
    }
    START.call_once(|| {
        std::thread::Builder::new()
            .name("fdv-mem".into())
            .spawn(|| loop {
                let (b, h) = (BUDGET.load(Ordering::Relaxed), HARD.load(Ordering::Relaxed));
                let u = usage_bytes().unwrap_or(0);
                match level(u, b, h) {
                    Level::Memout => memout(u, b, h),
                    l => OVER.store(l == Level::Over, Ordering::Relaxed),
                }
                // Poll faster near the hard limit: engines in a phase that
                // does not poll can allocate a few GB/s.
                let near = u >= h / 4 * 3;
                std::thread::sleep(Duration::from_millis(if near { 10 } else { 50 }));
            })
            .expect("spawn memory sampler");
    });
}

pub fn budget() -> u64 {
    BUDGET.load(Ordering::Relaxed)
}

/// True while the process is over its budget.
pub fn exceeded() -> bool {
    OVER.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The readings the sampler relies on. (Setting a budget here would
    /// stop every other test running in this process.)
    #[test]
    fn readings_are_sane() {
        if let (Some(r), Some(t)) = (rss_bytes(), total_bytes()) {
            assert!(r > 0 && r < t);
        }
        if let Some(u) = usage_bytes() {
            assert!(u > 0);
        }
        assert!(!exceeded());
    }

    #[test]
    fn parses_cgroup_files() {
        let pc = "0::/user.slice/user-1000.slice/user@1000.service/rdev.slice/run-p1.scope\n";
        assert_eq!(
            cgroup_path(pc),
            Some("/user.slice/user-1000.slice/user@1000.service/rdev.slice/run-p1.scope")
        );
        // cgroup v1 lines are ignored; a v1-only host has no 0:: line.
        assert_eq!(cgroup_path("12:memory:/foo\n0::/\n"), Some("/"));
        assert_eq!(cgroup_path("12:memory:/foo\n"), None);
        assert_eq!(parse_max("6442450944\n"), Some(6 << 30));
        assert_eq!(parse_max("max\n"), None);
        assert_eq!(parse_max("0\n"), None);
        let stat = "anon 6400000000\nfile 123456\nkernel 9000\nfile_mapped 77\n";
        assert_eq!(stat_field(stat, "file"), Some(123456));
        assert_eq!(stat_field(stat, "anon"), Some(6_400_000_000));
        assert_eq!(stat_field(stat, "shmem"), None);
    }

    #[test]
    fn default_budget_follows_the_cgroup() {
        assert_eq!(
            default_budget(Some(10 << 30), Some(32 << 30)),
            (10 << 30) / 10 * 9
        );
        assert_eq!(default_budget(None, Some(32 << 30)), 24 << 30);
        assert_eq!(default_budget(None, None), 0);
    }

    #[test]
    fn limits_stay_under_the_cgroup_max() {
        let g = 1u64 << 30;
        // inhouse_capped.sh: MemoryMax=6G, --mem-gb 5.4.
        let (b, h) = limits(5_400_000_000, Some(6 * g));
        assert_eq!(b, 5_400_000_000);
        assert_eq!(h, 5_940_000_000);
        assert!(h < 6 * g);
        // A budget above the cgroup limit is cut to 90% of it.
        let (b, h) = limits(8 * g, Some(6 * g));
        assert_eq!(b, 6 * g / 10 * 9);
        assert!(h <= 6 * g / 100 * 95 && h > b);
        // No cgroup: 10% above the budget.
        assert_eq!(limits(10 * g, None), (10 * g, 11 * g));
    }

    #[test]
    fn levels() {
        let (b, h) = (1000, 1100);
        assert_eq!(level(999, b, h), Level::Under);
        assert_eq!(level(1000, b, h), Level::Under);
        assert_eq!(level(1001, b, h), Level::Over);
        assert_eq!(level(1100, b, h), Level::Memout);
        // Budget 0 is off, whatever the usage.
        assert_eq!(level(u64::MAX, 0, 0), Level::Under);
    }
}
