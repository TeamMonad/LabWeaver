# Coverage Matrix

| Path | Positive | Negative | Current evidence | Required next evidence |
| --- | --- | --- | --- | --- |
| Service process | listener starts with explicit address | missing/invalid address fails | E1: `cargo xtask check`; Control service smoke | E2 process integration |
| Health contract | live/ready return versioned response | unknown route returns stable diagnostic | E1: unit tests and Control service smoke | E2 HTTP integration for all services |
| Request correlation | response contains request ID | malformed external ID policy not yet frozen | E1 unit | Contract decision and negative tests |
| Telemetry | JSON subscriber initializes | invalid filter fails | E1 build and code review | captured structured-log tests |
| NATS v1 contract | catalogued Subject, CloudEvent and Owner semantics are documented | Subject/type/dataschema mismatch, unowned Subject and sensitive payload are prohibited by contract | E0 ADR 0003 and NATS event catalog | E1 contract fixtures and E2 real PostgreSQL plus JetStream validation |
| NATS delivery and quarantine | Owner publishes committed Outbox facts; durable Consumer processes declared purpose | duplicate/replay, stale/gap sequence, malformed envelope, retry exhaustion and consumer restart cause no silent business success | E0 design only | E2 publish/consume/replay/ack/quarantine integration evidence |
| EvaluationSpec v1alpha1 | OJ/Linux examples pass generated Schema and semantic validation | duplicate/missing/cyclic dependencies, unsafe paths, scoring boundary and tool allowlist are rejected | E1 contract tests in current worktree | Runner contract suite and E2 Evaluation Service integration |
| Agent state and Tool registry | explicit state path, capability-pinned low-risk Tool dispatch, bound timeout, cancellation, no implicit retry, idempotency identity propagation, validated output and payload-free attempt audit | illegal transitions, repair exhaustion, timeout, cancellation, unbound/duplicate/missing/version-risk-capability-conflict Tools and invalid input/output are rejected; closed implementation failure codes cannot forge Registry diagnostics or leak input markers; elevated/high-risk Tools fail closed | Partial E1 `agent-core` tests in current worktree | Role A decision for permissions and approval evidence; durable Agent Service idempotency reservation/replay and audit integration; AG-01b Fixture Backend |
| GitHub governance | Milestones/Labels/Issues/branch rules and 15 P0 Ready items read back | insufficient scope was diagnosed and resolved in Issue #20 | E0 metadata | human review and continued Project maintenance |
| KubeVirt/Access/Evaluation | none | none | planned/blocked | E3/E4 evidence required by release gates |

## Infrastructure automation

| Capability | Owner | Entry point | Target evidence | Blocking condition |
| --- | --- | --- | --- | --- |
| Cluster preflight | A/D | `python tools/ansible.py preflight` | E1 then E3 replay | unresolved private config or network prerequisite |
| Kubernetes baseline | A | `python tools/ansible.py deploy` | E3 | package/version/bootstrap failure |
| Storage | A/D | deploy + verify | E3 RWO/RWX | unsafe formatting or mount/provision failure |
| KubeVirt/CDI | A/B/D | deploy + verify | E3 real VM lifecycle | missing KVM/CDI/scratch class |
| Gateway and policy | A/D | deploy + verify | E3 internal route | VIP not programmed or controller unavailable |
| Backup | A | `python tools/ansible.py backup` | E2 snapshot status | snapshot integrity failure |
| Ansible controller | D | Linux CI | E1/E2 lint, syntax, fictional Vault and storage fixtures | CI failure or missing E3 replay |
| Direct VM access | documented DirectAccessGrant device/IP/port scope and Router-first dual-revision activation | missing/stale Headscale or Router receipt, inactive device, wrong endpoint, IP reuse, unsupported protocol and cross-user access remain blocked | E0: ACCESS-01a documentation and ADR 0001 | E1/E2 contract and dual-enforcement tests; E3 Headscale/Router evidence; E4 multi-device containment replay |
| Browser SSH/VNC proxy | documented Keycloak PKCE handoff, custom Guacamole extension and scoped credential boundary | handoff replay/expiry, invalid Access decision, credential disclosure and stale session are rejected | E0: ACCESS-01a documentation and ADR 0001 | E1/E2 token and extension tests; E3 deployed Guacamole path; E4 browser replay |
| KubeVirt/Evaluation | none | none | planned/blocked | E3/E4 evidence required by release gates |
