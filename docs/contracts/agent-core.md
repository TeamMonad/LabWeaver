# Agent Core Contract

Status: partially implemented in the current worktree; permission and approval contracts are blocked
pending role A decisions.

`agent-core` owns deterministic Agent transitions and explicit Tool bindings. It does not own
publication, production approval, Runner execution, Fixture LLM generation, infrastructure or
service persistence.

## State machine

An `AgentRun` starts in `Parse` and only changes through `apply(AgentEvent)`. Invalid events return
`LW_AGENT_TRANSITION_INVALID` without modifying state, repair counters or transition history.
Fatal Parse, capability retrieval, Plan, Generate, Schema, Policy, Tool execution or verification
errors use `apply_failure(stable_diagnostic)`: every non-terminal state atomically enters `Failed`,
and its transition evidence preserves both the originating state and the owning boundary's root-cause
diagnostic. For example, a failed Tool dispatch records `LW_AGENT_TOOL_EXECUTION_FAILED` instead of
leaving the run in `ExecuteValidation`. Malformed failure diagnostics are rejected without mutation.

```text
Parse
→ RetrieveCapabilities
→ Plan
→ Generate
→ SchemaValidate
→ PolicyValidate
→ ExecuteValidation
→ Verify
→ AwaitingReleaseApproval
→ Completed
```

Schema, verification or teacher rejection can enter `Repair`. At most two automatic repairs return
to `Generate`; the next attempt transitions to `Failed` with
`LW_AGENT_REPAIR_BUDGET_EXHAUSTED` evidence.

Elevated validation uses a separate `AwaitingExecutionApproval` state. Approval from that state can
only enter `ExecuteValidation`; it cannot skip deterministic verification or final release approval.
Final teacher approval produces an `ApprovalHandedOff` outcome. Agent Core itself has no Publish API.
Any non-terminal state can enter `Cancelled`; terminal states reject further events.

## Agent Tool registry

Each `AgentTool` exposes an immutable name, exact version, risk, JSON input Schema, output Schema
version and JSON output Schema. Each `ToolBinding` independently pins name, version, expected risk
and a SHA-256 capability identity covering all six fields. It also fixes a non-zero wall timeout and
the `Never` retry policy. Registry construction compares the implementation with that binding before
making the Tool callable and rejects:

- empty or duplicate bindings;
- duplicate implementation names;
- missing implementations;
- version or risk conflicts;
- any input/output capability hash conflict;
- invalid input or output Schemas.

Dispatch names one bound Tool directly. The registry never selects by registration order or first
availability. Before implementation code runs, it validates input Schema. Elevated/high-risk Tools
currently fail closed with `LW_AGENT_TOOL_APPROVAL_REQUIRED`; there is no public API for model or
caller code to construct approval evidence. Role A must freeze approval identity/revision/expiry and
input or candidate binding before an approved dispatch path can be added. After execution, Registry
requires the exact bound output schema version and validates the output payload before reporting
success. `AgentContext` is supplied by the authoritative service boundary, not by model output.

Each `AgentContext` requires run/actor identity, immutable candidate revision, caller-reserved
idempotency key and a cancellation handle. Registry includes that identity in Tool context and audit,
invokes the implementation once per `execute` call, and races it against cancellation and the bound
timeout. It never retries implicitly and deliberately owns no in-memory reservation/cache. Agent
Service must durably reserve the invocation identity, merge concurrent duplicates and replay the
stored result before calling Registry; without that integration the path does not claim idempotency.
Tool implementations must not detach untracked work that can outlive their returned future.

Tool implementations return only `ToolExecutionError` with a closed, payload-free
`ToolExecutionFailureCode`; they cannot attach free-form detail or choose a Registry diagnostic.
Registry converts every such error to `LW_AGENT_TOOL_EXECUTION_FAILED`. Its public Display contains
only that stable diagnostic and the bound Tool name; only Registry can emit approval, validation,
timeout or cancellation diagnostics.

Tool input and output remain structured JSON and are never interpreted as Shell. Every dispatch
attempt returns payload-free audit evidence: run/actor identity, requested name, exact bound
version/risk/capability hash when present, input hash, optional output hash, outcome and stable
diagnostic. Approval rejection, input rejection, implementation failure and invalid output therefore
retain the same audit shape as success. Unbound attempts necessarily omit version/risk/capability
because no such binding exists. Schema failure messages mask values and do not echo untrusted
payloads.

This contract does not yet define or enforce filesystem/network/runtime permissions. That is a
blocking gap, not an implicit default. Fixture LLM Backend and generated candidate fixtures belong to
AG-01b and are intentionally outside this crate's current evidence.

## Stable diagnostics

| Diagnostic | Blocking condition |
| --- | --- |
| `LW_AGENT_TRANSITION_INVALID` | Event is illegal in the current Agent state |
| `LW_AGENT_FAILURE_DIAGNOSTIC_INVALID` | A fatal failure lacks a stable `LW_*` diagnostic |
| `LW_AGENT_REPAIR_BUDGET_EXHAUSTED` | More than two automatic repairs are required |
| `LW_AGENT_TOOL_BINDING_MISSING` | Registry has no explicit bindings |
| `LW_AGENT_TOOL_DUPLICATE` | Multiple implementations claim one Tool name |
| `LW_AGENT_TOOL_BINDING_DUPLICATE` | Manifest binds a Tool more than once |
| `LW_AGENT_TOOL_IMPLEMENTATION_MISSING` | Bound Tool implementation is absent |
| `LW_AGENT_TOOL_VERSION_MISMATCH` | Implementation does not match the exact binding |
| `LW_AGENT_TOOL_RISK_MISMATCH` | Implementation risk differs from the bound permission |
| `LW_AGENT_TOOL_CAPABILITY_MISMATCH` | Implementation input/output capability differs from the binding |
| `LW_AGENT_TOOL_OUTPUT_SCHEMA_INVALID` | Bound implementation declares an invalid output Schema |
| `LW_AGENT_TOOL_UNBOUND` | Call names a Tool outside explicit bindings |
| `LW_AGENT_TOOL_APPROVAL_REQUIRED` | Elevated/high-risk Tool lacks approval |
| `LW_AGENT_TOOL_INPUT_REJECTED` | Structured input fails Tool Schema |
| `LW_AGENT_TOOL_TIMEOUT` | Tool exceeded the timeout fixed by its binding |
| `LW_AGENT_TOOL_CANCELLED` | Authoritative caller cancelled Tool dispatch |
| `LW_AGENT_TOOL_EXECUTION_FAILED` | Bound implementation reports failure |
| `LW_AGENT_TOOL_OUTPUT_VERSION_MISMATCH` | Tool returned an incompatible output version |
| `LW_AGENT_TOOL_OUTPUT_REJECTED` | Tool output payload fails its bound Schema |

## Verification

```sh
cargo test --locked -p agent-core
cargo clippy -p agent-core --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

Passing tests are E1 evidence only for implemented state and in-process Registry behavior. They do
not close Issue #13 while permission and approval contracts remain blocked, and do not prove an Axum
Agent endpoint, idempotency reservation/replay, PostgreSQL/JetStream lifecycle, LLM/Fixture Backend, Environment
generation, controlled build, Kubernetes/KubeVirt validation or production approval.
