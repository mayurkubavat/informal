//! Formal property verification (FPV) for informal: sessions that hold a design
//! and its properties, an SVA subset compiler, the prove flow over the
//! engine portfolio, and Tcl command sets compatible with JasperGold and
//! VC Formal scripts.

pub mod cache;
pub mod compat;
pub mod complexity;
pub mod conn;
pub mod cov;
pub mod csr;
pub mod lp;
pub mod miter;
pub mod progress;
pub mod proof_core;
pub mod protocol;
pub mod sec;
pub mod session;
pub mod spv;
pub mod stats;
pub mod why_cex;
pub mod worker;
pub mod xprop;

pub use informal_trace as trace;

pub use compat::{detect_flavor, Flavor, Shell};
pub use progress::{EngineLine, ProgressKind, PropProgress, ProveProgress, ProveSink};
pub use session::{
    Effort, Environment, Kind, Orchestration, Property, Session, SourceLoc, Status, Task, Vacuity,
    EMBEDDED,
};
pub use stats::{DesignInfo, ProofStats, PropertyInfo};
pub use worker::{Reply, Request, Worker};
