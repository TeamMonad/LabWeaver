# Architecture Decision Records

The active Sprint 2 simplification baseline is ADR 0011. ADR 0012 freezes the
browser-console contract that follows it. Superseded ADRs remain
as decision history and must not be treated as deployment instructions.

ADRs record accepted cross-domain or high-risk decisions. A draft design statement is not an accepted ADR.

Each ADR must include status, context, decision, alternatives, consequences, security/data implications, compatibility, evidence and rollback or replacement conditions. Number new records sequentially as `NNNN-short-title.md`.

ADR 0001 records the proposed ACCESS-01a dual-path decision; it is not accepted until the required human reviews are complete.
No ADR is accepted in ARC-01a; this directory establishes the review process without inventing approvals.

## Proposed records

- [ADR 0002: PostgreSQL Schema Ownership and Migration Policy](0002-postgresql-schema-and-migration-policy.md)
  resolves Issue #17 at E0 design level and requires A/B review before runtime implementation.
- [ADR 0003: NATS Subject and Delivery Contract](0003-nats-subject-and-delivery-contract.md)
  resolves Issue #18 at E0 design level and requires A/B review before runtime
  implementation.
- [ADR 0004: Environment Lifecycle Domain](0004-environment-lifecycle-domain.md)
  freezes desired/observed state, operation concurrency, restart/reset and cleanup semantics.
- [ADR 0005: Contracts SSOT and Environment-first Release](0005-contracts-ssot-environment-first-release.md)
  makes the Rust `contracts` crate the sole public semantic authority.
- [ADR 0008: Environment Resource Management API](0008-environment-resource-management-api.md)
  defines course-scoped inventory, operation and AccessGrant discovery, stream synchronization,
  and the authenticated Web SDK transport boundary.
- [ADR 0006: Trusted Runtime Artifact Supply Chain](0006-trusted-runtime-artifact-supply-chain.md)
  freezes immutable Container/VM artifact, scan, signature and release evidence bindings.
- [ADR 0007: Claude Code-only Agent Runtime](0007-claude-code-agent-runtime.md)
  proposes one pinned, shell-free Claude Code worker boundary with provider-opaque configuration.
- [ADR 0009: Fenced KubeVirt Runtime Provider and Private SSH Endpoint](0009-kubevirt-runtime-provider.md)
  defines immutable VM/storage binding, executor fencing, guest/SSH readiness,
  identity persistence and deletion tombstones for Issue #53.
- [ADR 0010: Immutable PVC and Certificate-bound SSH Collector](0010-immutable-dual-runtime-collector.md)
  defines bounded dual-runtime snapshots, read-only short-lived SFTP,
  Object Lock publication and durable freeze-attempt fencing for Issue #54.
- [ADR 0012: Unified Console Capability and noVNC Boundary](0012-unified-console-capability-and-novnc.md)
  replaces the historical Guacamole browser-console proposal with one AccessGrant-scoped,
  one-time xterm/noVNC capability contract.
- [ADR 0013: Isolated C++17 OJ Runner](0013-isolated-cpp17-oj-runner.md)
  defines the digest-bound, network-denied C++17 Job, deterministic checker and
  aggregator, payload-free evidence boundary and exact cleanup for Issue #140.
- [ADR 0014: Read-only Ansible Probe Runner for Linux Nginx](0014-read-only-ansible-probe-runner.md)
  defines the allowlisted read-only VM probe Job, bounded typed facts,
  deterministic assertion evaluation, payload-free evidence boundary and exact
  cleanup for Issue #141.
