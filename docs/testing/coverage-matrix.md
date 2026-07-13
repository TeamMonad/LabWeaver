# Coverage Matrix

| Path | Positive | Negative | Current evidence | Required next evidence |
| --- | --- | --- | --- | --- |
| Service process | listener starts with explicit address | missing/invalid address fails | E1: `cargo xtask check`; Control service smoke | E2 process integration |
| Health contract | live/ready return versioned response | unknown route returns stable diagnostic | E1: unit tests and Control service smoke | E2 HTTP integration for all services |
| Request correlation | response contains request ID | malformed external ID policy not yet frozen | E1 unit | Contract decision and negative tests |
| Telemetry | JSON subscriber initializes | invalid filter fails | E1 build and code review | captured structured-log tests |
| Testable requirements baseline | US-01 to US-10 have stable 3C and acceptance IDs | missing target evidence, failed prerequisite, or weak evidence cannot be represented as done | E0 documented requirements matrix | mapped AC-01 to AC-10 P0 paths at their required E3/E4 levels |
| NATS v1 contract | catalogued Subject, CloudEvent and Owner semantics are documented | Subject/type/dataschema mismatch, unowned Subject and sensitive payload are prohibited by contract | E0 ADR 0003 and NATS event catalog | E1 contract fixtures and E2 real PostgreSQL plus JetStream validation |
| NATS delivery and quarantine | Owner publishes committed Outbox facts; durable Consumer processes declared purpose | duplicate/replay, stale/gap sequence, malformed envelope, retry exhaustion and consumer restart cause no silent business success | E0 design only | E2 publish/consume/replay/ack/quarantine integration evidence |
| EvaluationSpec v1alpha1 | OJ/Linux examples pass generated Schema and semantic validation | duplicate/missing/cyclic dependencies, unsafe paths, scoring boundary and tool allowlist are rejected | E1 contract tests in current worktree | Runner contract suite and E2 Evaluation Service integration |
| PostgreSQL bootstrap and roles | planned provisioner creates NOLOGIN owners, constrained identities and fixed pools/search paths | `PUBLIC`/default privilege leakage, runtime DDL, history write, connection identity reuse and cross-domain access are denied | E0 ADR 0002 and formal Migration contract | E2 PostgreSQL role and pool-isolation integration |
| Schema startup policy | valid applied identity accepts traffic | missing/unknown/ahead/incomplete/checksum mismatch terminates; behind/unavailable is live but NotReady with 503 | E0 design only | E2 process and PostgreSQL diagnostic tests |
| Domain and release Migration execution | manifest-selected domains run once under global then domain locks | manifest/history mismatch, lock contention, crash retry, different-release attempt, partial domain failure and unsupported down migration block release | E0 design only | E2 immutable ledger/report identity and Expand/Contract/forward-repair tests |
| Outbox and audit projection | domain transaction atomically records its local Outbox event; Control projection appends sanitized audit record | projection failure cannot affect a business transaction; duplicate/replayed event does not duplicate projection; business service cannot write `shared_audit` | E0 design only | E2 PostgreSQL plus JetStream replay/backfill and cutover evidence |
| EnvironmentLifecycle v1alpha1 | proposed state/authority contract | no runtime behavior is proven | E0 proposed ADR and contract only | transition/idempotency contract tests, Resource/Access integration and real provider/KubeVirt evidence |
| Linux Nginx material contract | public template, candidate manifest and normal fact mapping agree | missing material, hash/marker mismatch, restricted content, oversized report, stopped service/not-listening mapping and site-mismatch mapping fail explicitly | E1 Python material validator in current worktree | B-approved SubmissionManifest/Probe profile, then E3 real KubeVirt VM evidence for normal and both negative paths |
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
