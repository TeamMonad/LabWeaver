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
| ConsoleCapability contract | AccessGrant-scoped xterm/noVNC discovery and 30-second one-time issuance | duplicate kind, stale Grant/Environment/Lease fence, Work without Lease, Experiment with Lease, malformed locator, subprotocol mismatch, consumed/expired handoff | E2 Schema/OpenAPI/Web SDK and cross-consumer checks only; #131/#124/#126 own runtime E3/E4 |
| Submission freeze | PVC and certificate-bound SFTP sources, exact object version/hash | traversal, symlink, over-limit, changed-between-reads, missing required file, partial publish | both runtime sources create immutable FrozenSubmission records |
| C++17 OJ execution (#140) | approved digest-pinned compiler, exact/token checker, deterministic case aggregation, hash-bound evidence receipt | mutable image, High/Critical finding, invalid limits/path/profile, oversized label, missing namespace default-deny, compile error, wrong answer, TLE, MLE, OLE, self-SIGKILL, daemon/process-group escape, PID exhaustion, duplicate/missing/forged evidence, egress, cancellation, replacement race and cleanup residue | #123-owned attempt runs a real isolated Job and publishes immutable evidence without private payload |
| Deployment | read-only retained-infrastructure inventory, empty-domain baseline migrations, reviewed configuration bundle, ordered deploy, second idempotent deploy | non-empty business schema, dependency identity conflict, extra Secret/ConfigMap, invalid digest, readiness failure, cleanup residue | sanitized adoption manifest and readback bind the deployed identity without infrastructure deletion |
| Web and Release Gate | teacher/student Playwright, demo replay, report generation | fixed sleep, Fixture bundle, stale/cross-identity report, missing cleanup or skipped gate | one passing report references all required connected evidence |

WorkConfig, Resource approval, Tailnet, OpenAI Runtime,
multi-provider routing, Sigstore, Kyverno and Packer are outside this matrix.
The #140 row is planned connected coverage; its current evidence is local E1
and remains blocked by #123 and D Verify.
