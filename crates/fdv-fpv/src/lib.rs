//! Formal property verification (FPV) for fdv: sessions that hold a design
//! and its properties, an SVA subset compiler, the prove flow over the
//! engine portfolio, and Tcl command sets compatible with JasperGold and
//! VC Formal scripts.

pub mod compat;
pub mod session;
pub mod sva;

pub use compat::{detect_flavor, Flavor, Shell};
pub use session::{Kind, Property, Session, Status};
