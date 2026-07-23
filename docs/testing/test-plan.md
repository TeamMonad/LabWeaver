# Test Plan

The Sprint 2 terminal gate and its evidence contract are documented in
[`release-gate.md`](release-gate.md). A local or Fixture pass cannot satisfy a
check whose gate mode is `connected`.

## Issue #131 Container browser terminal

The browser terminal is a distinct high-risk acceptance path. Local gates cover
`TerminalSpec` and capability schema rejection, Access admission/session
metadata, Environment identity and release checks, unique Ready Pod selection,
PTY resize/exit/cleanup, and Web same-origin/subprotocol/manual-reconnect
behavior. Migration catalog, Helm render, RBAC and private-bundle checks are
mandatory after rebasing onto the final ex3 baseline.

Connected acceptance must use one clean commit, package manifest, seven-image
digest set and Run ID. Playwright waits for observable state and exercises
write under `/workspace`, disconnect/reconnect, stop/start, freeze, revoke,
cross-course denial and delete cleanup. It must not capture terminal content in
screenshots, traces, video, reports or logs. Fixture behavior is never accepted
as connected evidence.

Tests are grouped by the boundary they actually exercise. Reports must name the
source identity and must not promote Fixture or static evidence to a connected
runtime claim.

Issue #131 local validation uses the existing deployment entrypoints with
`--env ex3 --infra`; it must use the digest-pinned kind node image and must not
invoke the Issue #130 Fixture build or Fixture Playwright suite. A missing kind
binary, unavailable Docker daemon, missing private bundle, or failed local
readback is a blocking diagnostic rather than a fallback to Fixture mode.

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

## EX3 Fixture Demo replay (#130)

The EX3 Fixture Demo is a local, deterministic browser demonstration. It is
not connected Container evidence and cannot satisfy a Release Gate input. The
same existing Fixture specifications are used for the visible replay and the
full regression suite; no second mock backend or Playwright suite exists.

Build and run the Docker fixture image from a clean build cache. The image is
deliberately bound to loopback only:

```sh
export SOURCE_COMMIT="$(git rev-parse HEAD)"
export SOURCE_DATE_EPOCH="$(git show -s --format=%ct HEAD)"
export SOURCE_TAG="$(printf '%.12s' "$SOURCE_COMMIT")"
docker build --no-cache --file containers/Containerfile.web-fixture \
  --build-arg SOURCE_COMMIT="$SOURCE_COMMIT" \
  --build-arg SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
  --tag "labweaver-web-fixture:ex3-$SOURCE_TAG" .
docker run --rm --name labweaver-web-fixture-ex3 \
  --publish 127.0.0.1:4173:8080 \
  "labweaver-web-fixture:ex3-$SOURCE_TAG"
```

If Docker is unavailable, the operator must explicitly choose the existing
Host fixture path; no command performs this fallback automatically:

```sh
pnpm --dir web build:fixture
pnpm --dir web preview:fixture
```

For a visible, automatic P0 replay, run the existing tests in this order. Use
the first environment block for Host preview, or the second for the running
Docker fixture. `--headed --workers=1` is the only presentation control; the
test bodies, Fixture identity and reports remain unchanged.

```sh
export LABWEAVER_DATA_MODE=fixture
export VITE_DATA_MODE=fixture
pnpm --dir web exec playwright test e2e/teacher/material-upload.fixture.spec.mjs --config=playwright.config.mjs --project=teacher --headed --workers=1 --grep 'material upload package, agent run succeeds' && \
pnpm --dir web exec playwright test e2e/teacher/candidate-approval.fixture.spec.mjs --config=playwright.config.mjs --project=teacher --headed --workers=1 --grep 'candidate approval flow publishes release' && \
pnpm --dir web exec playwright test e2e/student/environment-console.fixture.spec.mjs --config=playwright.config.mjs --project=student --headed --workers=1 --grep 'environment console create, lifecycle, grant and revoke' && \
pnpm --dir web exec playwright test e2e/student/environment-runtime-access.fixture.spec.mjs --config=playwright.config.mjs --project=student --headed --workers=1 --grep 'student container HTTPS entry'
```

```sh
export LABWEAVER_DATA_MODE=fixture
export LABWEAVER_EXTERNAL_WEB_SERVER=true
export LABWEAVER_BASE_URL=http://localhost:4173
pnpm --dir web exec playwright test e2e/teacher/material-upload.fixture.spec.mjs --config=playwright.config.mjs --project=teacher --headed --workers=1 --grep 'material upload package, agent run succeeds' && \
pnpm --dir web exec playwright test e2e/teacher/candidate-approval.fixture.spec.mjs --config=playwright.config.mjs --project=teacher --headed --workers=1 --grep 'candidate approval flow publishes release' && \
pnpm --dir web exec playwright test e2e/student/environment-console.fixture.spec.mjs --config=playwright.config.mjs --project=student --headed --workers=1 --grep 'environment console create, lifecycle, grant and revoke' && \
pnpm --dir web exec playwright test e2e/student/environment-runtime-access.fixture.spec.mjs --config=playwright.config.mjs --project=student --headed --workers=1 --grep 'student container HTTPS entry'
```

After either replay, run the complete existing Fixture suite for the report,
HTML result, and retained failure artifacts:

```sh
pnpm --dir web test:e2e:fixture
LABWEAVER_EXTERNAL_WEB_SERVER=true pnpm --dir web test:e2e:fixture
```

The second command requires the Docker fixture to be reachable at the explicit
`LABWEAVER_BASE_URL`; if it is not reachable, Playwright fails without starting
a Host preview. Evidence remains under `web/playwright-report-fixture/` and
`web/test-results/fixture/` and is referenced only by relative locator and
image digest.

Fixture visual baselines are pinned to the repository's Linux Playwright image
(`mcr.microsoft.com/playwright:v1.61.1-noble`). Native Windows browser runs may
produce font-rasterization differences and must not rewrite those baselines.
Use the existing `playwright-fixture-runtime` CI environment, or that same
image, for an authoritative complete 112-test Host or Docker Fixture result.
