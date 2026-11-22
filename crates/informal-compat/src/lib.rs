//! The vendor-style Tcl shell (`informal shell`): JasperGold and VC Formal
//! script commands over an FPV session and its formal apps.
//! `informal-fpv` re-exports it as `informal_fpv::compat`.

pub mod compat;

use informal_app_conn::conn;
use informal_app_csr::csr;
use informal_app_protocol::protocol;
use informal_debug::why_cex;
#[cfg(test)]
use informal_session::progress;
use informal_session::{cache, complexity, cov, lp, sec, session, spv, xprop};
