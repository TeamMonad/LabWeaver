//! Shared one-shot task admission and Kubernetes execution boundary.
//!
//! Business services own task meaning and results. Resource owns admission. This crate owns the
//! role-neutral mechanics every one-shot task backend shares: the admitted execution witness
//! built from an authoritative Resource status, the observed execution timing contract, and the
//! exact Kubernetes REST mutations, ownership verification, observation, and deterministic
//! cleanup for attempt-scoped workloads. Role-specific document rendering and evidence receipt
//! parsing stay in the business service; this crate never interprets a score, a probe fact, or an
//! Agent result.

#![allow(
    missing_docs,
    clippy::missing_errors_doc,
    reason = "the shared execution boundary is documented at the type level"
)]

pub mod admission;
pub mod kubernetes;
pub mod resource;
pub mod timing;

pub use admission::{
    AdmittedExecution, ExecutionAdmissionError, cleanup_unknown, observation_timing,
};
pub use kubernetes::{SANDBOX_RUNTIME_CLASS, parse_egress_destination, valid_cidr};
pub use timing::ExecutionTiming;
