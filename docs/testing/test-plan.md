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

- `ConsoleCapability` discovery never returns a locator or handoff secret;
  issuance requires BFF/Origin/CSRF/idempotency and exact AccessGrant,
  Environment and conditional Work Lease revisions. The response has a
  30-second relative locator and a kind-matching versioned WebSocket
  subprotocol. Its secret is a path-scoped Secure HttpOnly cookie, not a body
  field or URL component.
- Work capabilities require a Lease fence; Experiment capabilities reject one.
  Expired/consumed capability, revision drift, unsupported kind, capacity,
  control-channel loss and upstream failure remain distinct diagnostics.
- Runtime implementations must use local expiry deadlines plus transactional
  Outbox/durable JetStream lifecycle delivery, never authorization polling.
  Loss of control delivery closes sessions; #131/#124/#126 provide the
  persistence, real stream and E4 evidence.
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

## Issue #140 C++17 OJ gate

The local gate proves only strict semantics and resource construction:

```sh
cargo test -p evaluation-service --lib --test oj --test oj_job
cargo clippy -p evaluation-service --all-targets --all-features -- -D warnings
LABWEAVER_OJ_RUNNER_NAMESPACE=labweaver-evaluation \
  cargo test -p evaluation-service --test oj_job \
  generated_resources_pass_kubernetes_server_side_dry_run -- --ignored
docker buildx build --file containers/Containerfile.oj-cpp17 \
  --platform linux/amd64 \
  --build-arg SOURCE_COMMIT="$(git rev-parse HEAD)" \
  --build-arg SOURCE_DATE_EPOCH="$(git show -s --format=%ct HEAD)" \
  --provenance=false .
```

The image command requires a working BuildKit daemon. A skipped or unavailable
build remains a blocker rather than a pass. CI must build the image twice,
compare OCI archive identities, reject High/Critical vulnerabilities and
secrets for the OJ image, and retain the Trivy JSON artifact.

After #123 supplies the authoritative attempt path, connected D Verify must use
one immutable submission, evaluator and image identity to exercise accepted,
compile error, wrong answer, time limit, memory limit and output limit. It must
also prove zero egress, no service-account token, readonly private inputs,
cancel/retry fencing, exact terminal cleanup and absence of private input,
expected output, command text and raw logs from the student projection and
machine-readable report. The private-payload case must also run an adversarial
binary that tries to read `/etc/labweaver`, `/input/evaluator`,
`/input/submission`, `/evidence` and unlisted `/work` files; every read must
fail under fully enforced Landlock. Equivalent adversarial preprocessor
includes and assembler `.incbin` attempts must fail during compilation. A
daemon/double-fork fixture must attempt `setsid`, `setpgid`, namespace `clone`,
`clone3`, `unshare` and `setns`; each escape must fail, the process count must
remain bounded by an observed cgroup v2 `pids.max` no greater than 128, and no
descendant may survive into the next case. D must also replace an attempt
resource between GET and DELETE and verify the
UID/resourceVersion precondition produces an identity conflict.

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
6. `cargo xtask demo replay` twice, each time using the exact connected package
   through the non-destructive `sprint2-application` adoption path;
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
export LABWEAVER_DEMO_PACKAGE_MANIFEST=<current-package-manifest>
pnpm --dir web test:e2e:live
```

Fixture specifications are excluded from this live invocation. Conversely,
the fixture gate excludes the live specifications, so neither evidence class
can silently satisfy the other.

## Fixture console preview

Issue #143 supplies `pnpm --dir web preview:console:fixture` for local layout
review without a backend. Its scope, startup path, and hard evidence boundary
are documented in [`fixture-console-preview.md`](fixture-console-preview.md).
