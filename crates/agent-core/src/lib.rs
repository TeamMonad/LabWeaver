//! Deterministic Agent state and explicitly bound Tool dispatch contracts.

mod state_machine;
mod tool;

pub use state_machine::{
    AgentEvent, AgentRun, AgentState, AgentStateError, TransitionOutcome, TransitionRecord,
};
pub use tool::{
    AgentContext, AgentTool, AgentToolError, ToolAudit, ToolAuditOutcome, ToolBinding,
    ToolCancellation, ToolDescriptor, ToolDispatchFailure, ToolExecution, ToolExecutionError,
    ToolExecutionFailureCode, ToolOutput, ToolRegistry, ToolRetryPolicy, ToolRisk,
};
