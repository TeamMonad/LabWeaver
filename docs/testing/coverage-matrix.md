# Coverage Matrix

| Path | Positive | Negative | Current evidence | Required next evidence |
| --- | --- | --- | --- | --- |
| Service process | listener starts with explicit address | missing/invalid address fails | E1: `cargo xtask check`; Control service smoke | E2 process integration |
| Health contract | live/ready return versioned response | unknown route returns stable diagnostic | E1: unit tests and Control service smoke | E2 HTTP integration for all services |
| Request correlation | response contains request ID | malformed external ID policy not yet frozen | E1 unit | Contract decision and negative tests |
| Telemetry | JSON subscriber initializes | invalid filter fails | E1 build and code review | captured structured-log tests |
| EvaluationSpec v1alpha1 | OJ/Linux examples pass generated Schema and semantic validation | duplicate/missing/cyclic dependencies, unsafe paths, scoring boundary and tool allowlist are rejected | E1 contract tests in current worktree | Runner contract suite and E2 Evaluation Service integration |
| GitHub governance | Milestones/Labels/Issues/branch rules and 15 P0 Ready items read back | insufficient scope was diagnosed and resolved in Issue #20 | E0 metadata | human review and continued Project maintenance |
| Direct VM access | documented DirectAccessGrant device/IP/port scope and Router-first dual-revision activation | missing/stale Headscale or Router receipt, inactive device, wrong endpoint, IP reuse, unsupported protocol and cross-user access remain blocked | E0: ACCESS-01a documentation and ADR 0001 | E1/E2 contract and dual-enforcement tests; E3 Headscale/Router evidence; E4 multi-device containment replay |
| Browser SSH/VNC proxy | documented Keycloak PKCE handoff, custom Guacamole extension and scoped credential boundary | handoff replay/expiry, invalid Access decision, credential disclosure and stale session are rejected | E0: ACCESS-01a documentation and ADR 0001 | E1/E2 token and extension tests; E3 deployed Guacamole path; E4 browser replay |
| KubeVirt/Evaluation | none | none | planned/blocked | E3/E4 evidence required by release gates |
