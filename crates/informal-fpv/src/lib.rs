//! Formal property verification (FPV) for informal: sessions that hold a design
//! and its properties, an SVA subset compiler, the prove flow over the
//! engine portfolio, and Tcl command sets compatible with JasperGold and
//! VC Formal scripts.

pub mod compat;
pub mod progress;
pub mod session;
pub mod trace;
pub mod worker;

pub use compat::{detect_flavor, Flavor, Shell};
pub use progress::{EngineLine, ProgressKind, PropProgress, ProveProgress, ProveSink};
pub use session::{Kind, Property, Session, SourceLoc, Status};
pub use worker::{Reply, Request, Worker};
