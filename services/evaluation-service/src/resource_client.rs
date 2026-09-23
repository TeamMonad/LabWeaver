//! Re-export of the shared one-shot Resource task client and lifecycle.
//!
//! The implementation moved to `crates/task-execution` so Agent authoring and Evaluation share
//! exactly one Resource admission protocol. This module keeps the existing Evaluation import
//! paths stable.

pub use task_execution::resource::*;
