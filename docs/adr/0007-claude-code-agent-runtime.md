# ADR 0007: Claude Code-only Agent Runtime

Status: proposed; Issue #50 requires A+B human approval and D Verify before this
contract or runtime can be treated as accepted.

## Context

LabWeaver needs one Agent execution boundary for generating independent
EnvironmentSpec and EvaluationSpec candidates. Embedding a provider SDK in the
Agent Service would make provider transport, authentication, retries and model
compatibility LabWeaver responsibilities. Maintaining several framework or SDK
paths would also create fallback and audit ambiguity.

Claude Code already owns its provider-facing configuration and exposes a
headless JSON CLI. LabWeaver must still own course egress policy,
immutable input selection, budgets, cancellation, candidate Schema validation,
protected-field rejection and audit evidence. Claude Code output remains
advisory and cannot approve, publish, release, execute or score a submission.

## Decision

LabWeaver supports exactly one v1 LLM runtime: a pinned Claude Code CLI in an
immutable worker container. `ClaudeCodeBindingV1` contains an opaque deployment
profile name, exact model identifier, exact CLI version, worker image SHA-256
and sanitized effective runtime-configuration SHA-256. Provider endpoint,
transport and authentication fields are deliberately absent from the public
contract and remain deployment-owned Claude Code configuration.

Before the billable command starts, the worker verifies the actual `claude
--version` result against the binding. A service-owned egress gate reads every
ProblemPackage object through an explicit store binding, rechecks size and
SHA-256, applies a revision-bound deterministic classifier, rejects every hard
data class, and freezes a text-only structured envelope. No public constructor
can bypass this gate.

The Agent worker invokes the fixed `claude` executable directly without a
shell. It supplies immutable teacher material on stdin and reproduces the exact
EnvironmentSpec or EvaluationSpec JSON Schema in a controlled prompt. Claude
Code must return one bare JSON object through its `stream-json` protocol; the
worker accepts the terminal result as the process boundary and validates the
candidate locally against the authoritative typed contract. The invocation is
bounded by wall time, request/turn count, output tokens and cost. Session
persistence, Chrome, built-in subagents, MCP discovery and all Tools are
disabled. There is no alternate SDK, provider implementation, model fallback
or second Agent runtime.

Environment and Evaluation tracks execute independently. A failure in one
track does not discard a validated result from the other. Each attempt retains
hash-only package, classifier, policy, input, prompt, Schema, Tool policy,
runtime, output and stderr identities together with bounded usage, session
identity, outcome and one stable diagnostic. Raw teacher material, model output
and provider stderr are not exposed through public errors or audit records.

Successful CLI exit is not sufficient. LabWeaver accepts a candidate only when
the terminal result envelope is supported, reported usage remains within
policy, the candidate JSON contains no protected authority field, typed deserialization
performs semantic validation, and canonical output hashing succeeds.

## Alternatives considered

- Direct provider SDKs were rejected because they would make multi-provider
  transport and authentication LabWeaver complexity.
- Goose, Rig and a custom Rust Agent framework were rejected because they add a
  second orchestration and provider-compatibility layer.
- Multiple runtime backends or automatic fallback were rejected because they
  make binding identity, budgets, failure ownership and replay ambiguous.
- A shell command string was rejected because untrusted material must never be
  interpreted as command syntax.

## Security and data implications

The service-owned gate is responsible for constructing the allowlisted and
classified LLM egress input. Invalid, modified, denied, non-text or oversized
input fails before process creation. The worker receives no LabWeaver Tool
permissions and must run under a restricted container security context and
network policy. Provider credentials must come from deployment-owned secret
injection and must not appear in contracts, arguments, audit, logs or reports.

The runtime hashes stderr before discarding it and uses payload-free stable
diagnostics for unavailable binaries, protocol errors, upstream exhaustion,
budget violations, timeout, cancellation and protected fields. E1 process
tests do not prove container isolation, network policy, credential handling or
provider behavior.

## Compatibility and consequences

This intentionally replaces the unshipped `OpenAiResponsesBindingV1` projection
with `ClaudeCodeBindingV1`. No v1 runtime or persisted production policy has
been released, so the change is handled before publication rather than by
maintaining two provider contracts. Existing callers and fixtures using the
OpenAI-shaped binding must migrate before review completion.

The current increment supplies the contract, egress gate, shell-free worker
adapter and an Agent-owned PostgreSQL path over the existing Sprint 2 schema.
Creation reserves the idempotency ledger, requested run and requested Outbox
event in one transaction. A new owner records both running attempts; terminal
processing writes the two independent checkpoints, derived run state and one
terminal Outbox event atomically. An exact replay reloads the authoritative run
and cannot invoke Claude Code or create new candidate identities.

The current Control-owned candidate projection, JetStream publisher,
object-store checkpoint reference, HTTP handler, isolated Kubernetes worker Job
and real credentialed Claude Code execution remain separate integration work.

## Evidence and rollback

E1 runtime-boundary evidence consists of contract regression tests, generated-schema drift
checks, package/hash/classification gates, CLI identity and command-policy
tests, strict envelope/Schema validation, budget and protected-field failures,
payload-free stderr handling, and dual-track partial-success behavior. On
2026-07-14, the real PostgreSQL 17 integration test passed against an ephemeral
local instance and covered isolated schema migration, transaction, checkpoint,
Outbox and exact replay behavior at E2. This does not raise the Claude Code
worker or provider path above E1.

A billable local probe on 2026-07-14 used Claude Code `2.1.159` with an exact
deployment-provided model. A minimal two-turn structured-output request
succeeded, confirming the inherited Claude/provider environment and supported
result envelope. The initial StructuredOutput-based adapter's complete
Environment candidate path then reached its wall-time limit without a terminal
result, which led to the exact-Schema prompt and terminal `stream-json` protocol
described below. The local login shell supplied provider environment variables;
workers must receive equivalent deployment-owned Secret/Config injection and
must never source a developer's shell startup files.
Because the timed-out processes returned no result envelope, their provider
usage is recorded as unobserved rather than as a misleading zero cost.

Debug probes isolated the compatibility boundary without retaining provider
logs. A root object with a nested `$ref` dispatched Claude Code's internal
`StructuredOutput` Tool, while a nested `oneOf` did not. This provider-specific
Tool path is therefore not the v1 protocol. The accepted Claude Code-only path
gives the model the complete authoritative Schema, requires one bare JSON
response, and then applies protected-field, strict typed and semantic
validation locally; it does not substitute a weaker provider Schema or accept
unvalidated text.

The final local live Environment test used pinned Claude Code `2.1.209` and the
same exact deployment-provided model binding. Claude Code emitted documented
`stream-json` events, including `system/thinking_tokens` progress, one assistant
JSON candidate and a terminal success result. The worker stopped at that
terminal result rather than waiting for CLI EOF, accepted the candidate in 9.31
seconds, and audited a cost of 34,755 microusd. This proves a real local
Environment candidate path. It does not prove Evaluation generation, the
worker-container credential/config path, Kubernetes isolation or E3 deployment.

Before any runtime publication, rollback is whole-PR reversion together with
regeneration of the public Schema artifacts. After an accepted v1 policy is
persisted, this binding cannot be silently reinterpreted; replacement requires
a versioned contract and explicit migration decision.
