//! Re-export of the shared execution admission witness and boundary helpers.
//!
//! The implementation moved to `crates/task-execution`; this module keeps the existing
//! Evaluation import paths stable.

pub use task_execution::admission::*;
