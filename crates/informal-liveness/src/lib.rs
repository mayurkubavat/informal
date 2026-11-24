//! Native liveness model-checking engines for InFormal:
//!
//! - [`KLive`] (`informal-klive`): Claessen–Sörensson `k`-liveness with
//!   certified one-shot streak detection.
//! - [`RLive`] (`informal-rlive`): SCC-guided IC3 liveness with certified
//!   fairness-free trap peeling and `O(log N)` forward-invariant SCC bisection,
//!   avoiding both `l2s` shadow latch duplication and `k`-counter blowup.

mod cycle;
pub mod klive;
pub mod lasso;
pub mod rlive;

use informal_engine::Engine;
use std::sync::Arc;

pub use klive::KLive;
pub use rlive::RLive;

/// The in-house liveness engines (empty when CaDiCaL is unavailable).
pub fn engines() -> Vec<Arc<dyn Engine>> {
    if informal_sat::Cadical::signature().is_none() {
        return Vec::new();
    }
    vec![Arc::new(KLive), Arc::new(RLive)]
}
