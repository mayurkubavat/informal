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
//! **Shedding.** A portfolio [`register`]s each in-process engine's cancel
//! token with a shed rank. From 80% of the budget on, the sampler first cancels the
//! running engine with the highest rank (the likeliest memory hog: deep
//! BMC before IC3), waits for it to return its memory, and only sheds the
//! next one if usage is still over. [`exceeded`] (stop everything) turns
//! true only when at most one registered engine is left running. Before
//! this, going over the budget stopped every engine at once, and 30 of
//! the 319 HWMCC'24 models in the 2026-11-09 full run lost engines that
//! solve them alone.
//!
//! When several models run in one process, they all stop while it is over
//! budget: run one model per process (scripts/inhouse_capped.sh) to keep
//! the others going.

use crate::CancelToken;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, Once, OnceLock};
use std::time::{Duration, Instant};

static BUDGET: AtomicU64 = AtomicU64::new(0);
static HARD: AtomicU64 = AtomicU64::new(0);
static OVER: AtomicBool = AtomicBool::new(false);
static START: Once = Once::new();
static CGROUP: OnceLock<Option<Cgroup>> = OnceLock::new();
type Hook = Box<dyn Fn() + Send + Sync>;
static HOOK: Mutex<Option<Hook>> = Mutex::new(None);

/// Exit status of a process that ran out of memory (`informal` memout).
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
        "informal: memout: {} MiB in use, hard limit {} MiB (budget {} MiB{}); exiting",
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
            "informal: memory budget cut to {} MiB (90% of the cgroup's memory limit)",
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
            .name("informal-mem".into())
            .spawn(|| loop {
                let (b, h) = (BUDGET.load(Ordering::Relaxed), HARD.load(Ordering::Relaxed));
                let u = usage_bytes().unwrap_or(0);
                match level(u, b, h) {
                    Level::Memout => memout(u, b, h),
                    l => {
                        // Shed from 80% of the budget on: a shed engine
                        // may be in a phase that does not poll, and the
                        // others keep allocating until it returns.
                        let shed = if u > shed_threshold(b) {
                            shed_step(Instant::now())
                        } else {
                            None
                        };
                        if let Some(name) = &shed {
                            eprintln!(
                                "informal: {} MiB in use (budget {} MiB); stopping {name} to free memory",
                                u >> 20,
                                b >> 20
                            );
                        }
                        let waiting = shed.is_some() || shedding_in_flight(Instant::now());
                        OVER.store(l == Level::Over && !waiting, Ordering::Relaxed)
                    }
                }
                // Poll faster near the hard limit: engines in a phase that
                // does not poll can allocate a few GB/s.
                let near = u >= h / 4 * 3;
                std::thread::sleep(Duration::from_millis(if near { 10 } else { 50 }));
            })
            .expect("spawn memory sampler");
    });
}

/// An engine that the sampler may stop on its own when the process is
/// over budget (see the module docs).
struct Shedable {
    id: u64,
    name: String,
    rank: u32,
    token: CancelToken,
    /// When the sampler cancelled it; `None` while it runs.
    shed_at: Option<Instant>,
}

static ENGINES: Mutex<Vec<Shedable>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// How long the sampler waits for a shed engine to return (and free its
/// memory) before it sheds the next one. A shed engine is unregistered
/// when it returns, so the wait usually ends much sooner.
const SHED_WAIT: Duration = Duration::from_secs(10);

fn engines() -> std::sync::MutexGuard<'static, Vec<Shedable>> {
    ENGINES.lock().unwrap_or_else(|p| p.into_inner())
}

/// Keeps an engine registered for shedding; dropping it (when the engine
/// returns) unregisters it.
#[must_use]
pub struct Registration(u64);

impl Drop for Registration {
    fn drop(&mut self) {
        let was_shed = {
            let mut es = engines();
            let i = es.iter().position(|e| e.id == self.0);
            i.map(|i| es.remove(i))
                .and_then(|e| Some((e.name, e.shed_at?)))
        };
        if let Some((name, at)) = was_shed {
            trim();
            eprintln!(
                "informal: {name} stopped {} ms after it was shed",
                at.elapsed().as_millis()
            );
        }
    }
}

/// Register a running engine: over the budget, the sampler cancels
/// `token` before stopping everything. Higher `rank` is shed first; rank
/// 0 is never shed (an external engine, whose memory is not ours).
pub fn register(name: &str, rank: u32, token: CancelToken) -> Registration {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    if rank > 0 {
        engines().push(Shedable {
            id,
            name: name.to_string(),
            rank,
            token,
            shed_at: None,
        });
    }
    Registration(id)
}

/// True when `token` was cancelled by shedding (its engine should say it
/// stopped for "memory").
pub fn was_shed(token: &CancelToken) -> bool {
    engines()
        .iter()
        .any(|e| e.shed_at.is_some() && e.token.same(token))
}

/// One sampler step over the budget: shed the highest-ranked running
/// engine if no earlier shed is still waiting to return and more than
/// one engine is running. Returns the shed engine's name.
fn shed_step(now: Instant) -> Option<String> {
    let mut es = engines();
    let i = pick_shed(&es, now)?;
    let e = &mut es[i];
    e.shed_at = Some(now);
    e.token.cancel();
    Some(e.name.clone())
}

/// Usage above which engines are shed: 80% of the budget.
fn shed_threshold(budget: u64) -> u64 {
    budget / 10 * 8
}

/// The engine [`shed_step`] sheds, if any (pure, for the tests).
fn pick_shed(es: &[Shedable], now: Instant) -> Option<usize> {
    if es
        .iter()
        .any(|e| e.shed_at.is_some_and(|t| now < t + SHED_WAIT))
    {
        return None;
    }
    if es.iter().filter(|e| e.shed_at.is_none()).count() < 2 {
        return None;
    }
    // Highest rank; among equals the one registered last.
    es.iter()
        .enumerate()
        .filter(|(_, e)| e.shed_at.is_none())
        .max_by_key(|(_, e)| (e.rank, e.id))
        .map(|(i, _)| i)
}

/// True while a shed engine is still returning: wait for its
/// memory before stopping everything.
fn shedding_in_flight(now: Instant) -> bool {
    engines()
        .iter()
        .any(|e| e.shed_at.is_some_and(|t| now < t + SHED_WAIT))
}

/// Give freed heap memory back to the system (glibc keeps it otherwise,
/// and the budget is measured on what the process holds).
fn trim() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        // SAFETY: malloc_trim only releases free heap pages.
        unsafe {
            malloc_trim(0);
        }
    }
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

    fn shedable(id: u64, rank: u32, shed_at: Option<Instant>) -> Shedable {
        Shedable {
            id,
            name: format!("e{id}"),
            rank,
            token: CancelToken::new(),
            shed_at,
        }
    }

    #[test]
    fn sheds_the_highest_rank_one_at_a_time_and_keeps_the_last() {
        let now = Instant::now();
        let mut es = vec![
            shedable(1, 1, None),
            shedable(2, 5, None),
            shedable(3, 2, None),
        ];
        assert_eq!(pick_shed(&es, now), Some(1));
        // While a shed engine has not returned, nothing more is shed.
        es[1].shed_at = Some(now);
        assert_eq!(pick_shed(&es, now + Duration::from_millis(100)), None);
        // After it, the next highest rank.
        let later = now + SHED_WAIT + Duration::from_millis(1);
        assert_eq!(pick_shed(&es, later), Some(2));
        // The last engine running is never shed (exceeded() stops it).
        es[2].shed_at = Some(now);
        assert_eq!(pick_shed(&es, later), None);
        // Equal ranks: the one registered last.
        let es = vec![shedable(7, 2, None), shedable(9, 2, None)];
        assert_eq!(pick_shed(&es, now), Some(1));
    }

    #[test]
    fn rank_zero_is_never_registered() {
        let t = CancelToken::new();
        let r = register("external", 0, t.clone());
        assert!(!engines().iter().any(|e| e.id == r.0));
        assert!(!was_shed(&t));
    }
}
