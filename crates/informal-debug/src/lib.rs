//! Counterexample explanation (`why_cex`). `informal-fpv` re-exports it as
//! `informal_fpv::why_cex`. Traces (`informal-trace`) and the complexity
//! report (`informal-session`) stay where the session can reach them.

pub mod why_cex;

use informal_session::{sec, session, spv, xprop};
