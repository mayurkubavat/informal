//! Formal property verification (FPV) for informal: sessions that hold a design
//! and its properties, an SVA subset compiler, the prove flow over the
//! engine portfolio, and Tcl command sets compatible with JasperGold and
//! VC Formal scripts.

pub mod cache;
pub mod compat;
pub mod conn;
pub mod cov;
pub mod progress;
pub mod session;
pub mod stats;
pub mod trace;
pub mod worker;

pub use compat::{detect_flavor, Flavor, Shell};
pub use progress::{EngineLine, ProgressKind, PropProgress, ProveProgress, ProveSink};
pub use session::{
    Environment, Kind, Property, Session, SourceLoc, Status, Task, Vacuity, EMBEDDED,
};
pub use stats::{DesignInfo, ProofStats, PropertyInfo};
pub use worker::{Reply, Request, Worker};
