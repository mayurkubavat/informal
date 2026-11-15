//! The AI interface of InFormal (decision D17): one FPV session, the same
//! one the shell and the app use, served to an AI model as Model Context
//! Protocol tools.
//!
//! * [`api`]: the tools. Each reply is JSON with a `schema` field naming
//!   its shape and version (`informal.trace/1`), so nothing has to be read
//!   out of console text.
//! * [`tools`]: their names, descriptions and JSON Schemas.
//! * [`server`]: the protocol (JSON-RPC 2.0, one message per line) over
//!   any pair of byte streams; `informal mcp` serves it on standard input
//!   and output.
//!
//! The rule of D17: an AI can read anything and propose anything, but only
//! a checked engine result is a verdict. Nothing here sets a property's
//! status except a prove. What an AI adds is tagged AI-added in every
//! listing and can be removed in one step, and a result found with an
//! AI-added assumption or cut point in the model says so (`under_ai`).

pub mod api;
pub mod cone;
pub mod server;
pub mod socket;
pub mod tools;
pub mod vcd;

pub use api::{Api, ApiError, Gui, Mode, Options, Proposal, State};
pub use server::{serve, Server};
