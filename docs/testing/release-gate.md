# Sprint 2 Release Gate v2

`cargo xtask release-gate` is the only command that can produce a passing
Sprint 2 report. It does not run a Fixture and it does not upgrade partial
evidence. `cargo xtask demo replay` first runs the allowlisted,
non-destructive Sprint 2 application adoption for the exact package, then live
the Resource public replay, live Playwright, and finally invokes the same gate. It does not reconcile Harbor or
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
export LABWEAVER_RESOURCE_REPLAY_PROFILE=.private/resource-acceptance-profile.json
export LABWEAVER_RESOURCE_REPLAY_AUTHENTICATION=.private/resource-replay-auth.json
export LABWEAVER_RESOURCE_DEPLOYMENT_MANIFEST=.private/resource-deployment-manifest.json
export LABWEAVER_RESOURCE_PACKAGE_MANIFEST=.private/resource-package-manifest.json
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

`cargo xtask resource replay` accepts only those four private locators and its
Run ID. It uploads the reviewed material, uses the separate Work AgentRun
endpoint, and drives Resource only through the authenticated BFF/public API.
It never accepts SQL, service credentials or a Secret value as a command-line
argument.

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

## Docker Desktop local boundary

`cargo xtask local preflight --profile local-hostpath` performs only read-only
Docker and Kubernetes probes. It writes an ignored
`artifacts/local-replay/local-connected-non-release-<run-id>.json` report with
the source commit, context, node/storage capabilities, explicit capability
gaps and stable blockers. A local Resource replay report also contains a
sanitized identity envelope: repository-relative profile, authentication,
deployment and package locators with SHA-256 hashes, the immutable
`resource-service` reference and configuration-bundle hash. It never contains
cookie, token, JWT, key or user-content values.
`cargo xtask resource replay --mode local --preflight ...` additionally checks
the private Resource profile, authentication locator and package/deployment
identity before running that same read-only probe.

The local profile intentionally maps the single-node Docker Desktop
`hostpath` provisioner to a Container-only test adapter. It is not `nfs-rwx`,
does not provide KubeVirt/CDI and is never accepted by the formal Release Gate.
No local report may be promoted, renamed or copied into the connected evidence
set. A successful local preflight therefore remains development evidence and
does not close Issue #142.

The local profile does not build or publish a release package by itself. When a
controller image is needed for the read-only probe, use the pinned
`containers/Containerfile.controller` through `tools/docker_controller.py` and
mount only the Docker Desktop kubeconfig plus the explicit ignored `.env`
locator. The controller uses a read-only repository mount and writes only to
the ignored `artifacts/` and `target/` mounts; it never applies a namespace,
PVC, Secret, Stream, Bucket, Realm or workload.

## Output

On success the gate writes exactly one ignored report at
`artifacts/release-gate/<run-id>.json` and validates it against
`schemas/results/release-gate-report.v2.schema.json`. The report is evidence for
the bound deployment only; changing source, migration catalog, image set,
runtime artifact or any referenced evidence requires a new Run ID and replay.
