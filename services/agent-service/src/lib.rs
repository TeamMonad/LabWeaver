//! Agent Service runtime adapters and orchestration.
#![allow(
    missing_docs,
    clippy::missing_errors_doc,
    reason = "stable diagnostics and the contracts crate document the public failure surface"
)]

#[cfg(test)]
extern crate self as agent_service;

#[path = "../../service_runtime.rs"]
pub mod service_runtime;

pub mod api;
pub mod build_executor;
pub mod build_pipeline;
pub mod build_provider;
pub mod build_store;
pub mod candidate_materializer;
pub mod classifier;
pub mod claude_code;
pub mod containerdisk;
pub mod generated_artifacts;
pub mod llm_review;
pub mod messaging;
pub mod model_diagnostics;
pub mod model_gateway;
pub mod model_input_authorization;
pub mod oci_import;
pub mod oci_registry;
pub mod platform_image_import;
pub mod platform_image_jobs;
pub mod platform_images;
pub mod run_store;
pub mod sandbox;
pub mod sandbox_process;
pub mod task_progress;
pub mod work_execution;
