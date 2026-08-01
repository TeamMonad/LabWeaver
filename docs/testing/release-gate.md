# Sprint 2 Release Gate v2

`cargo xtask release-gate` is the only command that can produce a passing
Sprint 2 report. It does not run a Fixture and it does not upgrade partial
evidence. `cargo xtask demo replay` first runs the allowlisted,
non-destructive Sprint 2 application adoption for the exact package, then live
Playwright, and finally invokes the same gate. It does not reconcile Harbor or
rebind the retained infrastructure installation to the application commit.

The Sprint 3 manual Demo, downstream implementation and evidence payloads use
the identity, resource-readback and No-Go contract in
[`runnable-environment-demo.md`](../architecture/runnable-environment-demo.md).
That contract does not create another gate or change the v1 JSON shapes.

## Inputs

The deployment controller writes a private, ignored JSON input conforming to
`schemas/results/sprint2-release-gate-input.v2.schema.json` and exports its
project-relative locator:

```sh
export LABWEAVER_RELEASE_GATE_INPUT=artifacts/release-gate/input.json
export LABWEAVER_DEMO_ENV=demo
export LABWEAVER_DEMO_PACKAGE_MANIFEST=artifacts/package/pkg-demo-sprint2/PlatformImagePackageManifest.json
cargo xtask demo replay
```

The input must bind:

- the current clean Git commit and one Run ID;
- the hashed platform deployment manifest and hashed Resource deployment manifest;
- all seven platform images plus the immutable `resource-service` image;
- the checked-in `migrations/catalog.yaml` and its hash;
- all seven immutable Harbor platform image references (`access-service`,
  `agent-service`, `control-service`, `environment-service`,
  `evaluation-service`, `openssh-gateway` and `web`);
- Container and KubeVirt runtime artifact digests;
- the exact eleven connected checks required by the input Schema, including
  `resource-lease`.

The deployment manifest also binds the package-manifest SHA-256. The
`ansible-idempotent` evidence binds the schema-valid application adoption
report, including its package, configuration-manifest, configuration-bundle,
migration, source and Run identities. This transitive binding is mandatory for
the Demo even though those fields are not duplicated in the Release Gate v1
input.

Every check repeats the same commit and Run ID and points to a project-relative,
non-symlink evidence file with a SHA-256 digest. The gate rereads and hashes each
file. Missing, changed, Fixture, local-only, failed or cross-identity evidence
blocks without writing a passing report.

The eleven evidence files retain their authoritative product/runtime identity and
readback rather than a boolean-only summary:

| Check | Required evidence boundary |
| --- | --- |
| `teacher-agent-approval` | real Keycloak actor/course, package, AgentRun, both candidate/approval revisions and trace identity |
| `build-supply-chain` | BuildKit operation, Harbor immutable digest, digest-bound Trivy result and published release identity |
| `container-lifecycle` | approved/planned/observed CPU, memory and PVC values plus create/stop/start/freeze/delete identity |
| `kubevirt-lifecycle` | approved/planned/observed vCPU, guest memory, DataVolume/PVC/disk values plus VM/VMI/readiness lifecycle identity |
| `access-negative` | cross-course, expiry, revocation, gateway-shell, SCP/SFTP and forwarding denials |
| `submission-freeze` | Environment binding, immutable FrozenSubmission identity and bounded worker cleanup |
| `cleanup-readback` | absence of runtime objects, grants/sessions, temporary credentials and worker inputs for both runtimes |
| `keycloak-playwright` | fresh teacher/student sessions and sanitized Trace, screenshot and video locators |
| `ansible-idempotent` | same-identity application adoption reports; second replay has no conflicting or destructive change |
| `rollback-drill` | reviewed Helm atomic rollback identity and restored immutable image set |
| `resource-lease` | Work AgentRun/release, Resource approval, quota shell, Environment handoff, renewal/revocation and capacity cleanup readback |

The Release Gate validates the v2 envelope and rehashes these files. v1 inputs
and reports are legacy evidence and cannot satisfy Issue #142. Producers
and D Verify remain responsible for the inner evidence semantics frozen by the
Demo contract; an empty or fabricated evidence file is not acceptance evidence
even if its hash is internally consistent.

## Output

On success the gate writes exactly one ignored report at
`artifacts/release-gate/<run-id>.json` and validates it against
`schemas/results/release-gate-report.v2.schema.json`. The report is evidence for
the bound deployment only; changing source, migration catalog, image set,
runtime artifact or any referenced evidence requires a new Run ID and replay.
