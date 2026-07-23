# Coverage Matrix

| Product path | Positive coverage | Required negative coverage | Connected exit condition |
| --- | --- | --- | --- |
| Teacher authoring | Keycloak login, material upload, AgentRun, two candidates, independent approval | invalid hash/schema, refusal, cancellation, cross-course or incomplete build context, unapproved release | real browser and backend share one identity |
| Container build | private Harbor project, BuildKit build, Trivy scan, immutable digest | mutable/tag-only reference, Critical finding, scanner drift, stale command, failed cleanup | real image is built, scanned and launched by digest |
| Container environment | create, Ready, protected endpoint, stop/start, freeze, delete | RBAC denial, public route, stale generation, withdrawn release, partial cleanup | no residual namespace resources after freeze/delete |
| KubeVirt environment | CDI disk, VM/VMI Ready, host key, SSH, stop/start, freeze, delete | wrong base disk, guest/host-key mismatch, forwarding, stale generation, failed cleanup | real VM preserves disk across stop/start and leaves no residual resource |
| Access | key registration, Grant activation, one endpoint, session lifecycle | weak key, alias injection, cross-course access, expiry, revoke, token replay, dependency outage | new access fails closed and active session closes within 60 seconds |
| Submission freeze | PVC and certificate-bound SFTP sources, exact object version/hash | traversal, symlink, over-limit, changed-between-reads, missing required file, partial publish | both runtime sources create immutable FrozenSubmission records |
| Deployment | read-only retained-infrastructure inventory, empty-domain baseline migrations, reviewed configuration bundle, ordered deploy, second idempotent deploy | non-empty business schema, dependency identity conflict, extra Secret/ConfigMap, invalid digest, readiness failure, cleanup residue | sanitized adoption manifest and readback bind the deployed identity without infrastructure deletion |
| Web and Release Gate | teacher/student Playwright, demo replay, report generation | fixed sleep, Fixture bundle, stale/cross-identity report, missing cleanup or skipped gate | one passing report references all required connected evidence |

Evaluation execution/scoring, WorkConfig, Resource approval, Tailnet, Guacamole,
OpenAI Runtime, multi-provider routing, Sigstore, Kyverno and Packer are outside
this matrix.
# Issue #131 terminal coverage

| Boundary | Positive coverage | Required negative coverage | Connected status |
| --- | --- | --- | --- |
| Contract | approved Container `TerminalSpec` and capability propagation | relative/unnormalized executable, control characters, argument bounds, non-`/workspace`, VM/no-spec capability | local implementation; connected N/A |
| Access | same-origin authenticated Upgrade and durable bounded session | Origin/subprotocol, cross-course, revoked/expired/stale revision, capacity, orphan recovery | connected mixed-source revoke and post-revoke denial pass; remaining negatives and same-source replay pending |
| Environment | exact instance/release/revision and terminal binding | stopped/deleted/withdrawn/wrong runtime/wrong endpoint | connected authoritative Container bridge pass; lifecycle negatives and same-source replay pending |
| container-executor | unique Ready owned runtime Pod, exec PTY, resize and exit | zero/multiple/not-Ready/deleting/wrong-label Pod, exec failure, disconnect cleanup | real Kubernetes PTY/write/exit and mTLS pass at source `96d9ca3d`; same-source package pending |
| Web | connect/disconnect/manual reconnect/resize/fullscreen/status | cross-origin URL, protocol mismatch, transport close without automatic retry | real browser login, xterm write, reconnect, fullscreen and revoke pass; no transcript evidence captured |
