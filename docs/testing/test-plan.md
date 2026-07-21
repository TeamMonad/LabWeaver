# Test Plan

The Sprint 2 terminal gate and its evidence contract are documented in
[`release-gate.md`](release-gate.md). A local or Fixture pass cannot satisfy a
check whose gate mode is `connected`.

Tests are grouped by the boundary they actually exercise. Reports must name the
source identity and must not promote Fixture or static evidence to a connected
runtime claim.

## Pull request gate

Target duration: less than ten minutes.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
pnpm --dir web lint
pnpm --dir web typecheck
pnpm --dir web test
pnpm --dir web contracts:check
```

The PR gate also renders Helm with valid and invalid Sprint 2 values, checks
Ansible syntax/lint, validates generated contract drift, and applies every
baseline migration to a disposable PostgreSQL instance.

## Contract and unit behavior

- one current v1 REST/event/schema representation;
- no Sigstore, Kyverno, SBOM, provenance or attestation field in the active
  product contract;
- Critical Trivy findings reject publication and High findings remain visible;
- Container candidate kind, course, approval, digest and scanner identity cannot
  drift; VM publication instead requires the exact deployment-locked OCI source
  digest, disk SHA-256, capacity, format, provider and storage bindings and
  carries no fabricated Container scan result;
- publication requests cannot submit artifact or policy evidence; Control
  resolves the authoritative Container build projection or VM base binding;
- candidate reads are Control-owned views: approval history and deterministic
  build state are reloaded from PostgreSQL, and a succeeded build is rejected
  unless both the immutable artifact and its policy evaluation are present;
- a Container build context must resolve to a verified object in a completed
  upload for the same course; cross-course or pending uploads fail before the
  approval and outbox transaction commits;
- invalid state transitions, duplicate IDs, illegal paths, over-limit content,
  timeout, cancellation and dependency failure return stable diagnostics;
- LLM output cannot approve, execute, publish or score a candidate.

## Integration behavior

- PostgreSQL transaction, idempotency, lease recovery and clean-baseline tests;
- JetStream duplicate, out-of-order, restart replay and quarantine tests;
- MinIO exact-version immutable ProblemPackage and FrozenSubmission tests;
- BuildKit/Harbor/Trivy executor success, Critical rejection, cancellation,
  stale generation and cleanup tests;
- Kubernetes/KubeVirt executor apply, observe, restart, deletion tombstone,
  deadline, RBAC denial and cleanup tests;
- Access/Environment mTLS authority and OpenSSH Gateway negative tests.
- freeze command idempotency, owner-scoped readback, restart recovery, exact
  Environment binding, immutable Job resources, worker NetworkPolicy and
  terminal cleanup-before-result tests;
- retained-infrastructure adoption rejects migration-ledger, stream, Harbor,
  Keycloak, configuration-bundle or package identity conflicts before workload
  rollout, and contains no namespace, schema, stream, bucket, project, realm,
  trust-plane, CRD, PVC or image deletion step.

## Connected Sprint 2 verification

The adopted-cluster run uses one commit, deployment manifest, migration
catalog, image digest set and Run ID. It must execute:

1. a real Container journey through freeze and cleanup;
2. a real KubeVirt/CDI VM journey through SSH, freeze and cleanup;
3. invalid, expired, revoked and cross-course AccessGrant cases;
4. a second idempotent Ansible deploy;
5. real teacher and student Playwright projects without fixed sleeps;
6. `cargo xtask demo replay` twice;
7. `cargo xtask release-gate` and report-schema validation.

Failed browser runs retain Trace, screenshot and video. Failed deployment or
cleanup retains a sanitized diagnostic and blocks the release report.

The real browser gate consumes existing connected-flow identities and reads
Keycloak passwords only from private files. Values are never accepted on the
command line or written to reports:

```sh
export LABWEAVER_BASE_URL=https://demo.lab.example
export LABWEAVER_E2E_PRIVATE_DIR=.private/e2e
export LABWEAVER_TEACHER_USERNAME=teacher
export LABWEAVER_TEACHER_PASSWORD_FILE="$LABWEAVER_E2E_PRIVATE_DIR/teacher-password"
export LABWEAVER_STUDENT_USERNAME=student
export LABWEAVER_STUDENT_PASSWORD_FILE="$LABWEAVER_E2E_PRIVATE_DIR/student-password"
export LABWEAVER_E2E_AGENT_RUN_ID=<approved-agent-run-id>
export LABWEAVER_E2E_CONTAINER_ENVIRONMENT_ID=<frozen-container-environment-id>
export LABWEAVER_E2E_VM_ENVIRONMENT_ID=<frozen-kubevirt-environment-id>
pnpm --dir web test:e2e:live
```

Fixture specifications are excluded from this live invocation. Conversely,
the fixture gate excludes the live specifications, so neither evidence class
can silently satisfy the other.
