//! Re-export of the shared one-shot Kubernetes execution mechanics.
//!
//! The implementation moved to `crates/task-execution` so Agent authoring and Evaluation share
//! exactly one Kubernetes execution backend. This module keeps the existing Evaluation import
//! paths stable.

pub use task_execution::kubernetes::*;
