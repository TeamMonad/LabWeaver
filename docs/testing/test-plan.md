# Test Plan

The Sprint 2 terminal gate and its evidence contract are documented in
[`release-gate.md`](release-gate.md). A local or Fixture pass cannot satisfy a
check whose gate mode is `connected`.

## Issue #128 video acceptance

`tools/demo-video` has three fail-fast commands: `capture`, `render` and
`verify`. Contract tests cover unknown fields, illegal profile/cut, scoped
locator traversal, missing or duplicate scenes, duration overflow, Fixture
contamination and SRT structure. Runtime verification rehashes every indexed
file and uses FFprobe to require 1920×1080, 60 fps, H.264, zero audio streams,
180–300 seconds and seekable playback. Both SRT timelines must end before the
video. Rendering pins a 12 Mbit/s H.264 hardware encoder and four frame workers;
missing hardware acceleration fails instead of silently falling back to software.
Every source recording plays exactly once and then yields to its audited final
screenshot while distinct explanation beats continue. The composition and
renderer profile disable media looping so short source footage cannot silently
be repeated to fill a scene.

The three rehearsals are distinct: Fixture flow, Fixture full-film/playback,
and connected-final playback. The first two do not perform or imply a connected
deployment. The third is allowed only after #126 supplies a same-identity Gate
report and connected shots. Automated text privacy scanning is necessary but
insufficient; the public final additionally requires D's frame-by-frame human
review. Raw clips, VNC pixels, terminal pixels, traces and MP4 files remain in
ignored `artifacts/demo-video/` only.

## Issue and Sprint verification boundary

Every Issue and its PR use the local integration gate as the merge evidence:

```sh
cargo xtask test --suite integration --scope changed --base-ref origin/develop
cargo xtask test --suite integration --scope candidate --base-ref origin/develop
cargo xtask test --suite integration --scope candidate --kind-only
```

The changed scope selects only affected Docker/kind groups. The candidate scope
runs the Docker dependency and supply-chain gate and adds fresh kind when
requested or when Kubernetes/deployment paths changed. It records an ignored
JSON report with source identity, image digests, selected paths, phase timings,
diagnostic and cleanup status.

Cluster deployment, connected Playwright, Ansible Verify and Release Gate are
not ordinary Issue/PR merge gates. One dedicated Sprint-end acceptance Issue
freezes the release identity and owns those connected checks. Local, Fixture and
static evidence must never be promoted to the Sprint-end cluster conclusion.
For the current Sprint, Issue #126 is that sole acceptance owner; development
Issues and PRs, including #142/#147, must not start shared-cluster deployment,
Resource replay, connected E2E or Release Gate commands.

### Issue #126 v3 window result (2026-08-11)

The v3 window froze source `68cb6f15f27d542747f967d7175498ba0f8eb31c`, Run ID
`019fef14-1cd0-70bd-8b5a-e8bcf43cdff3` and testflight ID
`019fef14-3922-784d-9e9e-858ad7d6a983`. The two allowed platform package
attempts both failed before build/scan/publish with the controller Buildx
Trivy DB manifest read diagnostic `EOF`. No connected deployment or replay was
started after that blocker, and the later six-read diagnostic probe is not
release evidence. The window is `Blocked`; the next operation requires a
separate Platform/DevOps repair and explicit Owner authorization.

### Issue #165 observability verification

The #165 local gate validates `labweaver.log.v1` against
`schemas/contracts/v1/internal/labweaver-log-v1.schema.json`; service identity and required
fields; INFO/DEBUG filtering; W3C extraction and outbound propagation; UUIDv7
request IDs; fail-closed RFC 9457 responses for malformed `traceparent` and
`x-request-id`; and absence of token, path, URL, object-key, locator, payload,
command, terminal, and raw-error sentinels from serialized output.

Focused runtime tests cover an accepted transition, retryable failure, and
terminal failure across HTTP, Outbox/NATS, Build, Container, KubeVirt, Freeze,
and Access Gateway boundaries. Assertions require one continuous trace
identity, an explicit `failure_stage`, DEBUG-only idle polling, and one final
ERROR owner. The PR runs targeted tests before the normal format, Clippy,
workspace, contract, and local candidate integration gates. It does not run a
shared-cluster deploy, replay, connected Playwright, or Release Gate; #126 owns
the real-chain verification.

### Issue #126 connected console matrix

The Container xterm and KubeVirt noVNC checks reuse
`container-linux-clone-real-e4` and `kubevirt-linux-clone-real-e4`; they do not
create a fourth acceptance model. Each runtime uses six isolated environment
IDs for positive, revoke, short AccessGrant expiry, stop, delete and
control-channel-loss. Every destructive phase reissues a capability and must
reject the already consumed locator. Browser waits are tied to API state,
WebSocket closure or controller coordination markers, never a fixed sleep.

Control-channel loss is injected only by
`deploy/ansible/playbooks/98-connected-console-control-loss.yml`. The playbook
requires an isolated namespace, Run label and exact Access Service Pod UID,
denies only TCP 4222, removes the Cilium policy in an `always` block, and reads
back its absence before allowing the browser to reconnect. Reports conforming
to `connected-console-evidence.v1` contain identities, counts, diagnostics and
hashes only; locator, Cookie, token, PTY transcript, VNC frame and absolute path
fields are forbidden.

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
- #131 local verification covers PostgreSQL one-time consumption and rollback,
  Environment SAN/revision/Lease/`TerminalSpec` admission, unique Ready Pod
  selection, binary/control frame contracts, bounded resize/output and abort
  behavior, RBAC/NetworkPolicy/Helm rendering, and Sprint 3 Web
  state/visual/a11y reuse. A real Kubernetes PTY is not local evidence.
  It deliberately does not deploy to the shared cluster; #126 owns connected
  Container PTY, revoke/expiry/control-channel-loss and Release Gate evidence.
- #124 local verification covers Container-to-xterm and KubeVirt-to-noVNC
  binding, kind/subprotocol mismatch without handoff consumption, Environment
  SAN/revision/Lease/runtime fencing, exact VMI namespace/name/UID/label and
  Running/Ready checks, bounded bidirectional RFB relay, disconnect handling,
  dedicated RBAC/NetworkPolicy and fail-closed executor configuration. It does
  not claim a real browser-to-VMI stream or connected revoke/expiry evidence;
  #126 owns those checks for the frozen candidate.
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
- The Work authoring route is separate from the Experiment route: Control binds
  `EnvironmentClass::Work` before dispatch and Agent rejects an experiment,
  absent or contradictory candidate class without conversion.
- Empty deployment bootstrap reads only a private Resource acceptance profile
  and Access membership seed. Both must agree on UUIDv7 course/actor identities,
  issuer and exactly teacher, student and platform-admin roles; no SQL may
  create candidates, approvals, releases, requests or leases.
- Release Gate v3 requires the Resource deployment manifest, immutable
  `resource-service` identity and same-identity `resource-lease` evidence.
- `cargo xtask resource replay` must run from an allowlisted Linux controller
  using private profile/authentication/deployment/package locators. It must
  demonstrate Work AgentRun through the public API; SQL, direct service mTLS
  and Secret values are prohibited replay inputs.
- `cargo xtask resource auth --infra --env demo --yes` is the only supported
  controller entry for producing those browser-session locators. It invokes
  `deploy/ansible/playbooks/94-resource-replay-auth.yml`, obtains fresh
  teacher, student and platform-admin BFF sessions through the real OIDC form
  flow with a configured private CA, and writes each state and locator with
  mode `0600`. OIDC or trust failures block replay; cached or copied session
  material is not a substitute. This authentication setup is separate from
  the required browser Playwright evidence.

## Issue #123 Evaluation control-plane gate

The local gate proves the authoritative PostgreSQL lifecycle and schema
surface, but not a connected runner/provider deployment:

```sh
cargo test -p contracts --all-targets --all-features
DOCKER_HOST=unix:///Users/zeyi2/.colima/default/docker.sock \
  cargo test -p evaluation-service --test control_plane -- --nocapture
cargo check -p evaluation-service --all-targets --all-features
cargo xtask contracts check
```

The `control_plane` integration test applies both evaluation migrations to a
disposable PostgreSQL instance and checks idempotent EvaluationRelease and
EvaluationRun creation, closed hash/runtime/frozen-submission identity,
duplicate replay, lease-token fencing, cancel before claim, failed StepRun
retry, expired lease recovery and cleanup verification before completed
failure.

D connected Verify must additionally prove the same source, migration catalog,
configuration, runner image digest, frozen submission, provider binding and
trace identity through the real Control-to-Evaluation caller and real
Evaluation worker path. Missing binding, permission, image digest, provider or
environment conditions must fail closed with the same diagnostic family and
must not be represented by this local test.

## Issue #160 Evaluation release and student-result API gate

The local merge gate extends the #123 disposable PostgreSQL proof and adds the
public Access/Control contract without starting shared-cluster work:

```sh
cargo xtask contracts check
cargo test -p contracts --all-targets --all-features
cargo test -p control-service --lib --test postgres --test mtls
cargo test -p access-service --lib
cargo test -p evaluation-service --test control_plane -- --nocapture
cargo xtask test --suite integration
```

Required assertions cover strict request fields, invalid revision/hash and
approval binding, duplicate-key replay and key/payload conflict, revision-fenced
withdrawal, one append-only withdrawal audit row, course/actor isolation,
terminal-only visibility, score suppression for failed/cancelled runs, stable
cursor paging, sensitive-field absence, mTLS caller identity and downstream
failure propagation. On Windows, Evaluation worker compilation remains a
platform limitation because those workers intentionally use Unix process and
resource primitives; Linux CI/local integration owns that executable evidence.

Fixture Web tests in #161 cover teacher publish/withdraw, idempotent client
submission, read-only runtime identity, student success/failure/cancelled
projections, empty/error/role-denied states, mobile layout, light/dark themes and
WCAG A/AA scanning. They prove browser state handling only and are not connected
evidence. #126 alone owns shared-cluster deployment, real teacher/student
Playwright, real runner/provider identity and Release Gate closure.

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

After #123 is merged and deployed, connected D Verify must use
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

## Issue #141 read-only Ansible Probe gate

The local gate proves only strict semantics and resource construction:

```sh
cargo test -p evaluation-service --lib --test ansible_probe
cargo clippy -p evaluation-service --all-targets --all-features -- -D warnings
docker buildx build --file containers/Containerfile.ansible-probe \
  --platform linux/amd64 \
  --build-arg SOURCE_COMMIT="$(git rev-parse HEAD)" \
  --build-arg SOURCE_DATE_EPOCH="$(git show -s --format=%ct HEAD)" \
  --provenance=false .
```

The image command requires a working BuildKit daemon. A skipped or unavailable
build remains a blocker rather than a pass. CI must build the image twice,
compare OCI archive identities, reject High/Critical vulnerabilities and
secrets for the probe image, and retain the Trivy JSON artifact.

After #123 supplies the authoritative attempt path, connected D Verify must use
one immutable VM, certificate and image identity to exercise the positive
Nginx path (service active, default site and document root observed) and the
stopped-service and site-mismatch negative paths. It must also prove the
preinstalled `ansible-probe-default-deny`, attempt egress limited to the VM
address on TCP/22, no service-account token, read-only SSH Secret mounts,
stale-certificate, unreachable-host, host-key-mismatch, timeout,
output-overflow and malformed-fact negatives, cancel/retry fencing, exact
terminal cleanup and the absence of file contents, command output and raw
logs from evidence, receipt and machine-readable report.

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

## Docker Desktop local preflight

The native Windows Docker Desktop path is read-only during ordinary Issue/PR
work. Run `cargo xtask local preflight --profile local-hostpath` or invoke
`tools/docker_controller.py` with an explicit kubeconfig and private `.env`
locator. The command records a sanitized
`local-connected-non-release` report and fails with stable blockers for missing
KubeVirt/CDI, `nfs-rwx`, a ready single node, or `ECNU_API_KEY`.
The report also binds its source/run identity; Resource local replay adds
repository-relative, hashed input locators and the immutable Resource image
reference without exposing authentication material.

The local overlay may render a Container-only hostpath adapter, but it cannot
prove RWX semantics, VM lifecycle, GPU capacity, or the formal Resource Gate.
It must not apply namespaces, PVCs, Secrets, streams, buckets, realms or
workloads as part of a normal PR check.

The companion `local-hostpath-stack-plan.yml` validates the dependency overlay,
renders the application Helm chart against isolated local namespace names and
prints a teardown order only. It is deliberately not an install or cleanup
playbook; no Kubernetes write operation is permitted in this local phase.

## Connected Sprint 2 verification

The adopted-cluster run uses one commit, deployment manifest, migration
catalog, image digest set and Run ID. It must execute:

1. a real Container journey through freeze and cleanup;
2. a real KubeVirt/CDI VM journey through SSH, freeze and cleanup;
3. invalid, expired, revoked and cross-course AccessGrant cases;
4. a second idempotent Ansible deploy;
5. real teacher and student Playwright projects without fixed sleeps;
6. `cargo xtask demo replay` twice, each time using the exact connected package,
   Resource profile/authentication/deployment/package inputs and the
   non-destructive `platform-application` adoption path;
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
