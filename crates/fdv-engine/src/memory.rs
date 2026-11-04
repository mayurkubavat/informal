//! A process-wide memory budget for in-process engines.
//!
//! External engines run under their own systemd memory caps; in-process
//! engines (and the SAT solvers they link) share this process, so the
//! budget is on the process's resident set. A sampler thread reads it
//! every 50 ms; once it is over budget, [`exceeded`] turns true, every
//! [`crate::RunContext::should_stop`] says stop, and engines report
//! unknown with reason "memory". When several models run in one process,
//! they all stop while it is over budget: run one model per process
//! (scripts/inhouse_capped.sh) to keep the others going.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Once;
use std::time::Duration;

static BUDGET: AtomicU64 = AtomicU64::new(0);
static OVER: AtomicBool = AtomicBool::new(false);
static START: Once = Once::new();

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

/// Set the budget (0 turns it off) and start the sampler.
pub fn set_budget(bytes: u64) {
    BUDGET.store(bytes, Ordering::Relaxed);
    if bytes == 0 || rss_bytes().is_none() {
        OVER.store(false, Ordering::Relaxed);
        return;
    }
    START.call_once(|| {
        std::thread::Builder::new()
            .name("fdv-mem".into())
            .spawn(|| loop {
                let b = BUDGET.load(Ordering::Relaxed);
                let over = b > 0 && rss_bytes().is_some_and(|r| r > b);
                OVER.store(over, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(50));
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
        assert!(!exceeded());
    }
}
