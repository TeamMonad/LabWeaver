# Runnable Environment Demo contract

This document freezes the only accepted Sprint 3 runnable-environment Demo for
Issues #122 through #126. It specializes ADR 0011 without adding a new product
contract. REST, NATS, Rust contracts, OpenAPI, Web SDK, JSON Schema and database
migrations remain unchanged.

The document is an execution and evidence contract, not proof that the Demo has
run. A pass exists only after the connected checks described below complete
against one immutable identity and `cargo xtask release-gate` writes a passing
report.

## Fixed journey

The manual walkthrough, connected Playwright projects and `cargo xtask demo
replay` must exercise the same ordered product path:

1. A teacher starts a fresh browser session and authenticates through the real
   Keycloak realm.
2. The teacher uploads the reviewed material and starts the explicitly bound
   ECNU Anthropic-compatible Claude Code `AgentRun`.
3. Environment and freeze-only Evaluation candidates complete independently;
   the teacher reviews and approves each candidate independently. No candidate,
   LLM output or browser action may approve the other candidate or produce a
   score.
4. The fixed build executor sends the approved Container build to BuildKit,
   publishes it to the private Harbor project, runs the digest-bound Trivy gate
   and returns one immutable digest. A tag, public registry or provider
   fallback is invalid.
5. A student starts the Container environment, reconciles the resource
   readback below, obtains an authorized Access path, stops and starts the
   environment, freezes a submission, revokes access and deletes the
   environment without residue.
6. The student repeats the same lifecycle with a real hardware-KVM KubeVirt VM
   and the deployment-owned base disk. A Container, Mock provider or static VM
   report cannot substitute.
7. The run proves cross-course, expired and revoked grants fail closed and that
   the OpenSSH Gateway rejects a gateway shell, SCP/SFTP and every forwarding
   mode.
8. D runs real connected Playwright, two complete `cargo xtask demo replay`
   invocations and the machine-readable Release Gate, then reconciles the final
   cleanup inventory.

There is no alternate demonstration page, manual-only bypass, Fixture project
or reduced single-runtime script.

## Immutable identity ledger

The operator records only sanitized identifiers, hashes, digests, revisions and
project-relative evidence locators. Secrets, tokens, material bodies, student
content, private configuration values and raw Kubernetes objects are excluded.

| Identity | Required value and authority | Binding rule |
| --- | --- | --- |
| Source | The 40-character `develop` squash commit with a clean tracked tree | Packaging, deployment, every connected check and the Release Gate use this exact commit. A PR head is not Demo evidence. |
| Run | One operator-supplied UUID | The deployment manifest, every Release Gate check and both replay evidence indexes use the same UUID. Tools must not invent a replacement. |
| Package | `PlatformImagePackageManifest.json`, its SHA-256 and source commit | The deployment manifest's `package_manifest_sha256` equals the rehashed package manifest. |
| Configuration | Application configuration manifest SHA-256 and rendered bundle SHA-256 | Both values come from the schema-valid application adoption report for the same source and Run. No Secret value is copied into evidence. |
| Migration | Project-relative `migrations/catalog.yaml` and its SHA-256 | The deployment manifest, adoption report and Release Gate input bind the same rehashed catalog. |
| Deployment | Deployment manifest path/SHA-256, environment, cluster UID and Helm revision | The manifest is schema-valid and produced by the non-destructive application adoption for the same source, Run, package and migration identities. |
| Platform images | Exactly seven immutable Harbor references | Components are `access-service`, `agent-service`, `control-service`, `environment-service`, `evaluation-service`, `openssh-gateway` and `web`; every reference ends in `@sha256:<digest>`. |
| Runtime artifacts | `container-runtime` and `kubevirt-runtime` SHA-256 values | The values are derived from authoritative runtime plans/readback, not copied from candidate input or Fixture data. |
| Product objects | Keycloak realm/actor, course, package, AgentRun, candidate/approval, build, release, Environment, AccessGrant and FrozenSubmission identities and revisions | Each evidence file carries the subset it observes and links it to the common source, deployment and Run. Cross-course or stale-revision joins are invalid. |
| Trace | A non-empty trace ID for each teacher, build, Container, KubeVirt, Access, freeze and cleanup path | Evidence indexes map each trace to the common Run. A trace from another deployment or Run is invalid; trace IDs need not be identical to each other. |
| Evidence | Project-relative regular-file locator and rehashed SHA-256 | Symlinks, traversal, absolute paths, missing files, changed bytes, Fixture mode, local-only mode or a non-passed connected result are invalid. |

Changing any source, package, configuration, migration, deployment, platform
image, runtime artifact or evidence hash invalidates the entire run. The
operator must package and deploy again, allocate a new Run UUID and replay all
connected checks. Old deployments and reports remain diagnostic history only.

## Runtime resource readback

Every resource record includes `sourceCommit`, `runId`, `traceId`,
`environmentId`, Environment revision/generation, provider binding, plan hash,
runtime artifact digest and observation time. Requested values come from the
approved `EnvironmentSpec`; observed values come from the Kubernetes or
KubeVirt API. UI values are projections and never an authority.

### Container

| Resource | Required requested and observed fields | Pass rule |
| --- | --- | --- |
| CPU | `cpuMillicores`; Pod container request and limit in normalized millicores | Request and limit both equal the approved value. Missing, unparsable or unequal values fail. |
| Memory | `memoryBytes`; Pod container request and limit in bytes | Request and limit both equal the approved value. Missing, unparsable or unequal values fail. |
| Storage | `storageBytes`; PVC name, UID, StorageClass, requested bytes, bound PV identity and observed capacity bytes | PVC is `Bound`, belongs to the environment namespace and has requested and observed capacity at least the approved value. UID and PV identity remain stable across stop/start. |
| Workload identity | Namespace UID, Pod/Deployment generation and observed generation, immutable image digest, provider plan SHA-256 | The observed generation catches up to the expected generation and the digest/plan match the released runtime artifact. |
| Lifecycle | Ready, stopped, restarted, frozen, grant revoked and deleted observations | Stop removes active workload access without losing retained storage; restart preserves the approved identity; deletion proves the namespace, workload, PVC, grant/session, temporary credentials and freeze worker inputs are absent. |

### KubeVirt

| Resource | Required requested and observed fields | Pass rule |
| --- | --- | --- |
| CPU | Approved `cpuMillicores`; VM/VMI domain CPU allocation and Kubernetes CPU request/limit in normalized millicores | The observed domain allocation, request and limit implement the approved value exactly; any topology or rounding policy must already be represented by the immutable provider plan. |
| Memory | Approved `memoryBytes`; guest memory bytes, VMI request bytes, limit bytes and the deployment-owned overhead annotation | Guest memory and request equal the approved value. The limit equals the checked sum of approved memory and the recorded overhead. |
| Storage | Approved `storageBytes`; DataVolume name/UID/source, PVC name/UID, StorageClass, requested bytes, observed capacity and bound volume identity | DataVolume source, base-disk identity and StorageClass match the deployment lock; the PVC is bound and its requested/observed capacity is at least the approved value. |
| VM identity | VM UID/generation, observed generation, VMI UID, root-disk UID, base OCI digest, disk SHA-256/format/capacity and provider plan SHA-256 | VM and disk identities remain stable across stop/start; a new VMI UID is allowed only after the lifecycle operation while the VM and root disk remain bound. |
| Readiness | guest-agent connection, SSH readiness, SSH host-key SHA-256 and service endpoint identity | All readiness checks are true and use the Access-authorized Gateway path. A running Pod or VMI phase alone is insufficient. |
| Lifecycle | Ready, VMI absent on stop, restarted, frozen, grant revoked and deleted observations | Stop preserves VM/root disk identity; restart re-establishes guest-agent/SSH readiness; deletion proves VM, VMI, DataVolume, PVC, service, grant/session, temporary certificate and freeze worker inputs are absent. |

The evidence must retain the approved, planned and observed values separately;
it must not overwrite a requested value with observed data to manufacture a
match.

## Failure and No-Go rules

The boundary that owns the failure records its existing stable diagnostic once,
with the common Run, trace, Owner, sanitized root cause and exit condition.

| Failure class | Required disposition |
| --- | --- |
| Source, deployment or image drift | Block with `LW_RELEASE_GATE_SOURCE_IDENTITY_MISMATCH`, `LW_RELEASE_GATE_DEPLOYMENT_IDENTITY_MISMATCH`, `LW_RELEASE_GATE_IMAGE_IDENTITY_INVALID` or `LW_RELEASE_GATE_RUNTIME_IDENTITY_INVALID` as applicable; allocate a new Run after rebuilding/redeploying. |
| Missing, stale, changed or non-connected evidence | Block with `LW_RELEASE_GATE_EVIDENCE_UNREADABLE`, `LW_RELEASE_GATE_EVIDENCE_HASH_MISMATCH`, `LW_RELEASE_GATE_CHECK_FAILED` or the exact input diagnostic as applicable; never rewrite the report by hand. |
| Missing or inconsistent runtime resource observation | Environment fails with `LW_ENVIRONMENT_PROVIDER_OBSERVATION_INVALID`; retain approved/planned/observed values and the owning object identities. |
| Provider, runtime artifact or real KubeVirt capability unavailable | Preserve the provider diagnostic and declare No-Go. Do not select another provider, Container or Mock. |
| Access denial, expiry or revocation failure | Preserve the Access/Gateway diagnostic, terminate the affected session and declare No-Go if unauthorized access remained possible. |
| Freeze or cleanup incomplete | Preserve `LW_ENVIRONMENT_PROVIDER_CLEANUP_FAILED`, `LW_ENV_CLEANUP_INCOMPLETE` or the owning freeze/object diagnostic; no passing artifact or report may be published. |
| Secret or protected payload exposure | Stop evidence publication, preserve only a sanitized diagnostic and open a security blocker. The exposed artifact cannot be used after redaction-in-place. |

Any failed real Gate results in `outcome:no-go`. The team does not move the
checkpoint, remove a runtime, weaken an assertion, reuse old evidence or add a
fallback to turn the result into Go.

## Delivery and Verify DAG

```text
#122 architecture and evidence contract ───────────────┐
#63 application/Gateway deployment ──> #123 Agent/build ──> #124 dual runtime ──> #125 Web
#63 + #123 + #124 + #125 ───────────────────────────────────────────────> #64 same-identity E3
#64 + #125 ─────────────────────────────────────────────────────────────> #126 live Demo
#126 ───────────────────────────────────────────────────────────────────> #3 Go/No-Go
```

- A owns this contract, cross-domain identity reconciliation and the final
  release judgment. A does not substitute for human review or Verify.
- B owns Agent, build, Container/KubeVirt and freeze implementation evidence,
  and performs the required high-risk review of this contract.
- C owns the Web projection and connected browser flow; UI evidence must point
  to backend/runtime authority.
- D independently runs deployment verification, Playwright, both replays,
  evidence rehash, cleanup readback and the final Demo procedure.

#123 may develop against this frozen contract once it is available on
`release/sprint2`, but remains blocked by #63 until the real application and
Gateway deployment is ready. #124, #125 and #126 retain their declared upstream
dependencies.

## Evidence index template

The #126 evidence comment lists sanitized values and project-relative artifact
locators. It must not embed raw reports, credentials or user content.

```markdown
## Demo evidence index

- Source commit:
- Clean tracked tree: yes / no
- Run ID:
- Package manifest locator / SHA-256:
- Configuration manifest SHA-256:
- Configuration bundle SHA-256:
- Migration catalog locator / SHA-256:
- Deployment manifest locator / SHA-256 / cluster UID / Helm revision:
- Seven platform image references:
- Container runtime artifact digest:
- KubeVirt runtime artifact digest:
- Teacher/Agent/build trace IDs and evidence locators:
- Container trace ID, resource readback and cleanup locator:
- KubeVirt trace ID, resource readback and cleanup locator:
- Access/freeze trace IDs and evidence locators:
- Connected Playwright Trace, screenshot and video locators:
- Replay 1 result and evidence locator:
- Replay 2 result and evidence locator:
- Release Gate report locator / SHA-256:
- Known limitations:
```

## Go/No-Go comment template

```markdown
## Sprint 3 Go/No-Go

- Outcome: `outcome:go` / `outcome:no-go`
- Source commit:
- Deployment identity:
- Run ID:
- D Verify: passed / failed
- Evidence index:

### Gate results

| Gate | Result | Trace/evidence | Diagnostic |
| --- | --- | --- | --- |
| Teacher AgentRun and approval |  |  |  |
| BuildKit, Harbor and Trivy |  |  |  |
| Container lifecycle/readback |  |  |  |
| KubeVirt lifecycle/readback |  |  |  |
| Access negatives |  |  |  |
| FrozenSubmission |  |  |  |
| Cleanup |  |  |  |
| Connected Playwright |  |  |  |
| Idempotent adoption |  |  |  |
| Rollback drill |  |  |  |
| Replay 1 / Replay 2 |  |  |  |
| Release Gate |  |  |  |

### Blockers for No-Go

| Owner | Root cause | Diagnostic | Exit condition | Sprint Goal impact |
| --- | --- | --- | --- | --- |
|  |  |  |  |  |
```

`outcome:go` is permitted only when every row is passed under the immutable
identity ledger, B has approved the high-risk contract/implementation scope and
D has independently completed Verify. Otherwise D publishes
`outcome:no-go` with the blockers and the Sprint event ends on schedule.
