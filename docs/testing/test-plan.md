# Test Plan

## API-01a gates

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked
```

The current test surface covers liveness, readiness, correlation headers, a stable not-found diagnostic and fail-fast binding configuration. It does not prove database, JetStream, OIDC, KubeVirt, MinIO, authorization or business behavior.

## EvaluationSpec v1alpha1 gates

```sh
cargo run --locked -p evaluation-domain --example export_schema -- schemas/evaluation
cargo test --locked -p evaluation-domain
cargo clippy -p evaluation-domain --all-targets --all-features -- -D warnings
```

The contract tests validate OJ and Linux examples against generated JSON Schema and semantic rules.
An external-crate integration test reads Metadata, Submission, Step, Runner, Checker, Aggregation and
Review values through the public immutable domain API. Direct public Serde deserialization is tested
against invalid `EvaluationSpec` and `GoalReview` documents to prove that wire APIs cannot bypass
semantic validation.
Negative coverage includes duplicate IDs, missing dependencies, DAG cycles, score mismatches, unsafe
Collector/Runner/LLM paths, protected LLM score fields, raw command fields, zero-value scores and Ansible modules outside
the allowlist. The JSON Schema and semantic reader both reject attempts to disable mandatory teacher
approval. Score aggregation overflow is exercised in both debug and release profiles. This is
E1 evidence and does not execute a Runner or production evaluation path.

## Governance verification

Read back Milestones, Labels, branch protection, Sprint parents, sub-issues and Project fields through GitHub APIs. The verified governance result is 20 Project items and 15 P0 items with `Workflow Status=Ready`, plus Owner, Review Role, Sprint, Area, Codex Mode, Risk, SP and Evidence metadata.

## PostgreSQL ownership and Migration planned gates

[ADR 0002](../adr/0002-postgresql-schema-and-migration-policy.md) and the
[Database Migration contract](../development/database-migrations.md) are E0
design evidence only. Before any persistence path is marked implemented, tests
must prove that runtime roles cannot execute DDL or cross-domain writes;
unknown, missing, ahead, behind and checksum-mismatched schema identities deny
readiness with stable diagnostics; and concurrent Migration Jobs serialize with
the domain advisory lock.

The integration suite must also prove repeat execution, immutable history,
build-identity report validation, partial-failure blocking, Expand/Contract
compatibility, reviewed forward repair, transaction-plus-domain-Outbox
atomicity, publisher retry, duplicate/replayed event handling, and
Control-owned `shared_audit` projection idempotency. A database container and
real Migration Job are required for this evidence; fixture-only checks cannot
promote it above E1.

The persistence suite must additionally prove that bootstrap revokes `PUBLIC`
and default privileges, each pool uses only its assigned role and fixed
`search_path`, and runtime history access is read-only. It must exercise the
global release lock, domain locks, crash-released lock recovery, same-release
retry, different-release blocking, transactional file failure and retained
partial-domain ledger evidence. It must prove process termination for
`DB_SCHEMA_MISSING`, `DB_SCHEMA_UNKNOWN`, `DB_SCHEMA_AHEAD`,
`DB_SCHEMA_INCOMPLETE` and `DB_SCHEMA_CHECKSUM_MISMATCH`, plus live/NotReady
503 behavior for `DB_SCHEMA_BEHIND` and `DB_SCHEMA_UNAVAILABLE`.

Audit tests must prove that a projection failure cannot change a committed
business transaction, that it records an unhealthy watermark, and that
idempotent replay/backfill plus dual-write watermark comparison supports a
future approved handoff to the audit-projection worker.
