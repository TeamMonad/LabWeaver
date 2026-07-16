//! Agent Service runtime adapters and orchestration.
#![allow(
    clippy::missing_errors_doc,
    reason = "stable diagnostics and the contracts crate document the public failure surface"
)]

pub mod api;
pub mod build_pipeline;
pub mod build_provider;
pub mod build_store;
pub mod classifier;
pub mod claude_code;
pub mod messaging;
pub mod run_store;
