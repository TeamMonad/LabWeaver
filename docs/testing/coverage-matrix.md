# Coverage Matrix

Issue/PR merge evidence is the local integration gate from
`cargo xtask test --suite integration`. Connected cluster evidence is collected
only by the dedicated Sprint-end acceptance Issue; the rows below describe the
full product evidence expected there and must not be used to require a cluster
deployment for every PR.

| Product path | Positive coverage | Required negative coverage | Connected exit condition |
| --- | --- | --- | --- |
| Teacher authoring | Keycloak login, material upload, AgentRun, two candidates, independent approval | invalid hash/schema, refusal, cancellation, cross-course or incomplete build context, unapproved release | real browser and backend share one identity |
| Container build | private Harbor project, BuildKit build, Trivy scan, immutable digest | mutable/tag-only reference, Critical finding, scanner drift, stale command, failed cleanup | real image is built, scanned and launched by digest |
| Container environment | create, Ready, protected endpoint, stop/start, freeze, delete | RBAC denial, public route, stale generation, withdrawn release, partial cleanup | no residual namespace resources after freeze/delete |
| KubeVirt environment | CDI disk, VM/VMI Ready, host key, SSH, stop/start, freeze, delete | wrong base disk, guest/host-key mismatch, forwarding, stale generation, failed cleanup | real VM preserves disk across stop/start and leaves no residual resource |
| Access | key registration, Grant activation, one endpoint, session lifecycle | weak key, alias injection, cross-course access, expiry, revoke, token replay, dependency outage | new access fails closed and active session closes within 60 seconds |
| Resource request and Lease authority (#142) | private UUIDv7 course bootstrap, Work-only AgentRun, independent candidate approval/release, owner-scoped request list/get/create, approve/resize-approve, cancel, reject, retry, Lease renew/revoke; revision/idempotency fences; exact Environment Lease sync; Access-first expiry cleanup; capacity absence readback | profile/Access membership mismatch, experiment/missing Work candidate, missing/invalid caller, cross-owner read/cancel/retry, duplicate/conflicting idempotency, stale revision, expired approval, zero/unsupported GPU, Lease renewal/revoke replay, Environment cleanup failure, capacity residue | connected PostgreSQL/NATS/Kubernetes quota-shell adoption, approved Work handoff, renewal, revoke/expiry and residue-free release on one deployment identity, bound into Release Gate v2 |
| ConsoleCapability contract | AccessGrant-scoped xterm/noVNC discovery and 30-second one-time issuance | duplicate kind, stale Grant/Environment/Lease fence, Work without Lease, Experiment with Lease, malformed locator, subprotocol mismatch, consumed/expired handoff | E2 Schema/OpenAPI/Web SDK and cross-consumer checks only; #131/#124/#126 own runtime E3/E4 |
| Submission freeze | PVC and certificate-bound SFTP sources, exact object version/hash | traversal, symlink, over-limit, changed-between-reads, missing required file, partial publish | both runtime sources create immutable FrozenSubmission records |
| Evaluation control plane (#123) | PostgreSQL-authoritative EvaluationRelease, EvaluationRun, StepRun and attempt lifecycle, internal mTLS routes, Outbox events, hash-only identity | idempotency conflict, release withdrawal, frozen-submission mismatch, runtime hash mismatch, course mismatch, lease-token loss, stale revision, cancel, retry, expired lease and cleanup-not-verified | local E2 passes against real disposable PostgreSQL; E3/E4 requires real Control caller, runner image, provider binding and Release Gate |
| C++17 OJ execution (#140) | approved digest-pinned compiler, exact/token checker, deterministic case aggregation, hash-bound evidence receipt | mutable image, High/Critical finding, invalid limits/path/profile, oversized label, missing namespace default-deny, compile error, wrong answer, TLE, MLE, OLE, self-SIGKILL, daemon/process-group escape, PID exhaustion, duplicate/missing/forged evidence, egress, cancellation, replacement race and cleanup residue | after #123 is merged and deployed, a #123-owned attempt runs a real isolated Job and publishes immutable evidence without private payload |
| Read-only Ansible Probe execution (#141) | approved digest-pinned probe image, pinned linux-nginx-probe-v1 profile, bounded typed facts, deterministic assertion evaluation, hash-bound evidence receipt, real KubeVirt VM positive path | mutable image, High/Critical finding, invalid profile/allowlist/assertion/limit/target/username, missing ansible-probe-default-deny, egress beyond VM:22, unreachable host, host-key mismatch, stale certificate, timeout, output overflow, malformed/unknown facts, forged evidence, stopped-service and site-mismatch negatives, cancellation and cleanup residue | after #123 is merged and deployed, a #123-owned attempt runs a real probe Job against a real KubeVirt VM and publishes immutable evidence without payload |
| Deployment | read-only retained-infrastructure inventory, empty-domain baseline migrations, reviewed configuration bundle, ordered deploy, second idempotent deploy | non-empty business schema, dependency identity conflict, extra Secret/ConfigMap, invalid digest, readiness failure, cleanup residue | sanitized adoption manifest and readback bind the deployed identity without infrastructure deletion |
| Web and Release Gate | teacher/student Playwright, demo replay, report generation | fixed sleep, Fixture bundle, stale/cross-identity report, missing cleanup or skipped gate | one passing report references all required connected evidence |

WorkConfig, Resource approval, Tailnet, OpenAI Runtime,
multi-provider routing, Sigstore, Kyverno and Packer are outside this matrix.
The #123 row is local E2 only until D Verify runs the connected identity.
The #140 row is planned connected coverage; its current evidence is local E1
and remains blocked by #123 merge/deploy plus D Verify.
The #141 row is planned connected coverage; its current evidence is local E1
and remains blocked by #123 deploy plus D Verify.
The #142 row has connected deployment/readiness, PostgreSQL request/approval,
Kubernetes ResourceQuota-shell and NATS Lease request/reply evidence for the
earlier identity. Forward rotation reissued ten JWT/mTLS identities, rejected
the preceding credentials, retained seven streams and five consumers, and
drained the Resource Outbox. The same-namespace quota adoption and
renewal/revoke/expiry saga are implemented locally; approved Work handoff and
terminal cleanup remain delegated to Sprint-end acceptance Issue #126 until the
new package and migration are deployed and replayed. The #142 development PR
does not launch that connected path.
