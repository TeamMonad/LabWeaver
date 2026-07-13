# Coverage Matrix

| Path | Positive | Negative | Current evidence | Required next evidence |
| --- | --- | --- | --- | --- |
| Service process | listener starts with explicit address | missing/invalid address fails | E1: `cargo xtask check`; Control service smoke | E2 process integration |
| Health contract | live/ready return versioned response | unknown route returns stable diagnostic | E1: unit tests and Control service smoke | E2 HTTP integration for all services |
| Request correlation | response contains request ID | malformed external ID policy not yet frozen | E1 unit | Contract decision and negative tests |
| Telemetry | JSON subscriber initializes | invalid filter fails | E1 build and code review | captured structured-log tests |
| Testable requirements baseline | US-01 to US-10 have stable 3C and acceptance IDs | missing target evidence, failed prerequisite, or weak evidence cannot be represented as done | E0 documented requirements matrix | mapped AC-01 to AC-10 P0 paths at their required E3/E4 levels |
| EvaluationSpec v1alpha1 | OJ/Linux examples pass generated Schema and semantic validation | duplicate/missing/cyclic dependencies, unsafe paths, scoring boundary and tool allowlist are rejected | E1 contract tests in current worktree | Runner contract suite and E2 Evaluation Service integration |
| GitHub governance | Milestones/Labels/Issues/branch rules and 15 P0 Ready items read back | insufficient scope was diagnosed and resolved in Issue #20 | E0 metadata | human review and continued Project maintenance |
| KubeVirt/Access/Evaluation | none | none | planned/blocked | E3/E4 evidence required by release gates |
