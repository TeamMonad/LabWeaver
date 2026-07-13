# Coverage Matrix

| Path | Positive | Negative | Current evidence | Required next evidence |
| --- | --- | --- | --- | --- |
| Service process | listener starts with explicit address | missing/invalid address fails | E1: `cargo xtask check`; Control service smoke | E2 process integration |
| Health contract | live/ready return versioned response | unknown route returns stable diagnostic | E1: unit tests and Control service smoke | E2 HTTP integration for all services |
| Request correlation | response contains request ID | malformed external ID policy not yet frozen | E1 unit | Contract decision and negative tests |
| Telemetry | JSON subscriber initializes | invalid filter fails | E1 build and code review | captured structured-log tests |
| EvaluationSpec v1alpha1 | OJ/Linux examples pass generated Schema and semantic validation | duplicate/missing/cyclic dependencies, unsafe paths, scoring boundary and tool allowlist are rejected | E1 contract tests in current worktree | Runner contract suite and E2 Evaluation Service integration |
| PostgreSQL ownership and roles | planned domain-local persistence, Migration and Outbox boundaries | runtime DDL/cross-domain write denial; unknown, mismatched or incomplete schema identity denies readiness | E0 ADR 0002 and formal Migration contract | E2 PostgreSQL integration with real roles, Migration Job and stable diagnostics |
| Domain Migration execution | manifest-selected domain Migration runs once under an advisory lock | checksum/history mismatch, lock contention, partial failure and unsupported down migration block release | E0 design only | E2 immutable history/build report and Expand/Contract/forward-repair tests |
| Outbox and audit projection | domain transaction atomically records its local Outbox event; Control projection appends sanitized audit record | transaction failure emits no event; duplicate/replayed event does not duplicate projection; business service cannot write `shared_audit` | E0 design only | E2 PostgreSQL plus JetStream integration evidence |
| GitHub governance | Milestones/Labels/Issues/branch rules and 15 P0 Ready items read back | insufficient scope was diagnosed and resolved in Issue #20 | E0 metadata | human review and continued Project maintenance |
| KubeVirt/Access/Evaluation | none | none | planned/blocked | E3/E4 evidence required by release gates |
