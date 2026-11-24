//! Machine-verifiable Formal Verification Plan & Signoff Specification
//! (`fvp.spec.json`, schema `informal.spec/1`) and `spec_audit` evaluator.
//!
//! Cross-checks an active [`informal_fpv::Session`] against a structured design
//! boundary + requirement-to-SVA mapping + signoff gate contract, returning
//! per-requirement verdicts (`verified`, `cex`, `undetermined`,
//! `missing_assert`, `missing_cover`, `vacuous`, `overconstrained`), uncovered
//! requirements, unmapped assertions, and concrete next-action hints.

mod audit;
mod boundary;
mod lp_gates;
mod schema;
#[cfg(test)]
mod tests;

pub use audit::audit_session;
pub use boundary::{discover_spec_file, wildcard_match};
pub use schema::{
    AuditRuntime, BoundarySpec, DesignIntent, FvpSpec, LowPowerGates, RequirementSpec,
    SignoffGates, AUDIT_SCHEMA, SPEC_SCHEMA,
};
