//! Formal property verification (FPV) for informal: sessions that hold a design
//! and its properties, an SVA subset compiler, the prove flow over the
//! engine portfolio, and Tcl command sets compatible with JasperGold and
//! VC Formal scripts.

pub mod compat;
pub mod why_cex;
pub mod worker;

pub use informal_app_conn::conn;
pub use informal_app_csr::csr;
pub use informal_app_protocol::protocol;
pub use informal_session::{
    cache, complexity, cov, lp, miter, progress, proof_core, sec, session, spv, stats, xprop,
};
pub use informal_trace as trace;

pub use compat::{detect_flavor, Flavor, Shell};
pub use progress::{EngineLine, ProgressKind, PropProgress, ProveProgress, ProveSink};
pub use session::{
    Effort, Environment, Kind, Orchestration, Property, Session, SourceLoc, Status, Task, Vacuity,
    EMBEDDED,
};
pub use stats::{DesignInfo, ProofStats, PropertyInfo};
pub use worker::{Reply, Request, Worker};
