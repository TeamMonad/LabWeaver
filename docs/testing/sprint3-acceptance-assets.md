# Sprint 3 acceptance assets

Issue #94 freezes the acceptance contracts needed by TEST-03b/c/d and REL-03a. The checked-in
assets are E1/static fixtures. They define future E4 requirements; they do not implement or prove
an Evaluation runtime, provider, scoring service, production UI, connected cluster, or Feature
Complete state.

## Single source of truth

- `tests/fixtures/acceptance/sprint3-acceptance-assets.json` is the machine-readable scenario,
  sample, negative-matrix, Mock-boundary, frontend and Feature Complete inventory.
- `schemas/results/sprint3-acceptance-assets.v1.schema.json` validates that inventory.
- `schemas/results/sprint3-acceptance-evidence.v1.schema.json` is the evidence report contract.
- `schemas/results/sprint3-feature-complete.v1.schema.json` is the Feature Complete index contract.
- `tests/fixtures/acceptance/fixture-expectations.json` binds every invalid fixture to its exact
  diagnostic. `validate-fixtures` discovers JSON recursively; a new invalid fixture without an
  expectation, a stale expectation, or an accepted invalid fixture fails the command.

No parallel Markdown-only matrix is authoritative. This document explains the checked-in machine
contracts.

## Commands

```sh
cargo xtask acceptance-assets validate
cargo xtask acceptance-assets list
cargo xtask acceptance-assets validate-fixtures
cargo xtask acceptance-assets validate-report --report tests/fixtures/acceptance/reports/valid/planned-e1.json
cargo xtask acceptance-assets validate-feature-complete --report <report-under-tests/fixtures/acceptance/reports>
```

All commands fail closed. Report references in a Feature Complete index must be existing JSON files
below `tests/fixtures/acceptance/reports`. Traversal, absolute paths, Windows drive paths, UNC paths,
root escape, canonical/symlink escape, wrong extensions and missing files are rejected before a
referenced report is read.

## Three future E4 paths

`acceptance-assets list` prints exactly:

1. `oj-real-e4`: real product submission, six deterministic C++17 outcomes, immutable releases,
   evaluator case facts, deterministic aggregation, replay, readback, cleanup and rollback.
2. `container-linux-clone-real-e4`: real Environment/Resource container clone, digest identity,
   formal AccessGrant, evaluation input, revoke/expiry, cleanup and rollback.
3. `kubevirt-linux-clone-real-e4`: real KubeVirt VM and disk identity, hardware-backed readiness,
   formal access, evaluation input, revoke/expiry, cleanup and rollback. A container or fixture VM
   cannot substitute.

Each manifest entry freezes actor, tenant/course, authentication/authorization, submission,
release, image/spec/artifact, Run/Step/Attempt, Resource, Access and trace identity requirements.
These strings are requirements, not fabricated runtime identifiers.

## C++17 fixtures

The corpus contains one correct submission plus compile error, WrongAnswer, time limit, memory
limit and output limit samples. Every source path and SHA-256 is validated. The correct and
WrongAnswer samples also bind fixed input and expected output hashes; WrongAnswer binds a distinct
actual-output hash. Language, positive resource limits, stable terminal state/diagnostic and the
no-network/no-filesystem boundary are machine checked.

Timeout, memory-limit and output-limit sources are intentionally static-only. The aggregate command
does not compile or execute any C++ fixture on the host. A future reviewed sandboxed Runner must
exercise the declared CPU, wall-clock, memory, output and cleanup policy.

## Negative matrix and frontend inventory

The manifest enumerates all required vectors for SSRF, vulnerability, Secret, malicious file,
license, signature, invalid evaluator output and cross-tenant access. Every entry is a minimal
synthetic input with a stable diagnostic and declares that validation performs no network request
and persists no state. Cross-tenant entries additionally require no existence disclosure,
idempotency fact or Outbox event.

The frontend inventory freezes six planned E4 observations across teacher, student and admin routes.
Every item requires authentication, authorization, backend readback and cleanup. It remains
`planned`; a component fixture or route existing in source cannot satisfy it.

## Evidence and Feature Complete boundary

Evidence explicitly distinguishes `planned`, `fixture`, `local`, `ci`, `connected` and
`live-runtime`, plus E1/E2/E3/E4 and `none`/`fixture`/`mock`/`real` provider mode. The validator
rejects planned or fixture pass claims, local-connected claims, CI-live-runtime claims, E3/E4
fixtures, E4 Mock providers, missing immutable identity/digests, cross-commit identity, missing
cleanup/readback, unverified rollback, skipped steps, blockers, limitations and wrong diagnostics.

Feature Complete requires passed real E4 for Issue #64 and all three Sprint 3 scenarios. All four
references must share source, build, deployment and environment identity; the three Sprint 3
reports must also share tenant/course identity. Fixtures and Mock evidence cannot satisfy this
gate. No checked-in fixture claims Feature Complete.

## Current evidence

The validator, JSON Schemas, Rust unit tests and static assets provide E1 evidence only. The two
valid report fixtures demonstrate `planned/E1` and `local/E2` representation; the latter is still a
fixture and is not connected evidence. TEST-03b/c/d, real multi-role browser replay, real providers,
Resource/Access, failure recovery, deployment/rollback and same-build D Verify remain required for
E4 and release readiness.
