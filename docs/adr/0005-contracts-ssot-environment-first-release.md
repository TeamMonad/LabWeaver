# ADR 0005: Contracts SSOT and Environment-first Release

Status: proposed; Issue #45 requires A+B human approval and D Verify.

## Context

Public semantics were split between `common-domain`, `evaluation-domain`, service-local DTOs and prose. That permits Rust services, OpenAPI and Web clients to assign different meanings to the same identifier, lifecycle or failure. Environment execution also lacked one immutable release identity linking approved intent to runtime artifact evidence.

## Decision

`crates/contracts` is the only source for public domain values, validation, lifecycle transitions, REST/SSE metadata, NATS CloudEvents and generated schemas. It has no dependency on a business crate, Axum, Tower, persistence or Provider code. `common-domain` and `evaluation-domain` are removed without compatibility shells; consumers migrate once.

All public IDs are UUIDv7 newtypes. Structured hashes use RFC 8785 canonical JSON and binary hashes use raw bytes. Input documents and events are strict; response evolution is additive. REST and NATS v1 remain compatible only through new endpoints/subjects or optional response fields. Breaking semantics require a parallel v2.

An `EnvironmentTemplateRelease` is environment-first: one release binds one exact approved Environment candidate, one runtime kind, one verified artifact identity and one evidence set. Evaluation remains independently approved and cannot mutate this release identity.

## Consequences and boundaries

`cargo xtask contracts generate` produces JSON Schema, separate Public/Internal OpenAPI and the Public Axios SDK. `cargo xtask contracts check` regenerates in temporary directories and fails on byte drift. Gateway Internal OpenAPI is excluded from the browser SDK.

This ADR provides E1 contract evidence only. It does not implement Handler, database, Outbox, Provider, UI, deployment or EvaluationRun behavior. Runtime work must consume these contracts and establish its own E2–E4 evidence.

Security-sensitive contract changes require A+B approval; D verifies generation, negative tests and evidence claims. Rollback is whole-PR reversion before runtime consumers publish v1 data. Once v1 is externally published, reinterpretation is forbidden and replacement requires v2.
