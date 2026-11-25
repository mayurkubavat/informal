//! The FPV session and the parts of InFormal that live inside it: the
//! design and property database, elaboration and the prove flow over the
//! engine portfolio, the result cache, progress, statistics and proof
//! cores, and the formal apps whose state is a field of [`session::Session`]
//! (`sec` with its miter, `cov`, `xprop`, `spv`, `lp`, and the counter
//! abstractions of `complexity`). Those apps and the session use each other,
//! so they are one crate until their state moves out of the session type.
//! `informal-fpv` re-exports every module here at its old path.

pub mod cache;
pub mod complexity;
pub mod cov;
pub mod lp;
pub mod miter;
pub mod progress;
pub mod proof_core;
pub mod sec;
pub mod session;
pub mod snapshot;
pub mod spv;
pub mod stats;
pub mod xprop;

pub use session::Session;

use informal_trace as trace;
