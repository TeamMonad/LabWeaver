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
| PostgreSQL bootstrap and roles | planned provisioner creates NOLOGIN owners, constrained identities and fixed pools/search paths | `PUBLIC`/default privilege leakage, runtime DDL, history write, connection identity reuse and cross-domain access are denied | E0 ADR 0002 and formal Migration contract | E2 PostgreSQL role and pool-isolation integration |
| Schema startup policy | valid applied identity accepts traffic | missing/unknown/ahead/incomplete/checksum mismatch terminates; behind/unavailable is live but NotReady with 503 | E0 design only | E2 process and PostgreSQL diagnostic tests |
| Domain and release Migration execution | manifest-selected domains run once under global then domain locks | manifest/history mismatch, lock contention, crash retry, different-release attempt, partial domain failure and unsupported down migration block release | E0 design only | E2 immutable ledger/report identity and Expand/Contract/forward-repair tests |
| Outbox and audit projection | domain transaction atomically records its local Outbox event; Control projection appends sanitized audit record | projection failure cannot affect a business transaction; duplicate/replayed event does not duplicate projection; business service cannot write `shared_audit` | E0 design only | E2 PostgreSQL plus JetStream replay/backfill and cutover evidence |
| GitHub governance | Milestones/Labels/Issues/branch rules and 15 P0 Ready items read back | insufficient scope was diagnosed and resolved in Issue #20 | E0 metadata | human review and continued Project maintenance |
| KubeVirt/Access/Evaluation | none | none | planned/blocked | E3/E4 evidence required by release gates |
