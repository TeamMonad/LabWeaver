# Active Blockers

This file contains only blockers for Draft PR #121 on `release/sprint2`.
Historical evidence cannot satisfy a current same-identity gate.

## Current evidence identity

- Current PR head: `bdedb772`; this source-only commit aligns the deterministic
  browser Fixture with the production `FrozenSubmission` readback contract.
- Connected application source: `ec6587cbb1639451540aacdae1402f8002f4d20f`.
- Connected package: `pkg-demo-sprint2-ec6587cbb163`.
- Ansible application run: `deploy-sprint2-ec6587cb`; testflight:
  `testflight-sprint2-ec6587cb`.
- The package passed static and connected validation. Both application
  reconciles and testflight passed without resetting retained infrastructure.
- Real Keycloak teacher/student Playwright on that deployment passed three
  Container-slice tests; the VM test was explicitly skipped.

## Same-identity KubeVirt replay

The current deployment has no complete same-identity VM create, SSH,
stop/start, freeze and cleanup replay. An older source identity completed that
real lifecycle, but it cannot be joined to the current Container evidence.

Exit:

1. deploy the final reviewed package identity;
2. complete the real KubeVirt lifecycle without fallback;
3. bind resource readback, frozen submission and cleanup evidence to the same
   commit, deployment manifest, migration catalog, image set and Run ID.

Owner: B implementation review; D connected Verify; A release judgment.

## Access and Gateway negative matrix

The current Container reached `ready`, exposed a healthy HTTP endpoint and was
used by a real student session. The final identity still lacks the complete
illegal-key, cross-course, expired, revoked, target-injection, SCP/SFTP,
forwarding and Access-outage replay.

Exit: every denial remains fail closed and carries the current AccessGrant,
session, endpoint and trace identity. Revocation/expiry must reject new
connections and terminate affected sessions within the declared bound.

Owner: A authorization semantics; B security review; D Verify.

## Container lifecycle closure

The `ec6587cb` deployment has verified Agent generation, independent approval,
BuildKit/Harbor/Trivy publication, Container readiness, HTTP endpoint health and
immutable freeze. The final same-identity evidence set does not yet include
stop/start and application-owned delete/cleanup readback.

The older replay workspace file was seeded with an administrative `kubectl
exec` and remains only historical demo evidence. The current resource plan
initializes an empty PVC from the approved image's fixed
`/opt/labweaver/workspace-seed` directory without overwriting retained data.
Exit additionally requires connected proof that this initializer populated the
PVC and that the resulting submission froze without administrative mutation.

Exit: complete stop/start/delete and prove no application-owned runtime
resources remain. Do not substitute older lifecycle evidence.

Owner: B implementation review; D connected Verify.

## Infrastructure identity and rollback

Application-specific non-destructive adoption and testflight pass. Generic
`cargo xtask verify --infra` correctly rejects the retained historical
deployment manifest because its commit, inventory hash and component-lock hash
do not match the current private inventory. This fail-closed result must not be
bypassed by rewriting evidence.

Exit:

- produce a reviewed infrastructure deployment identity for the retained
  target, or explicitly revise the release contract through an ADR;
- run the bounded rollback drill and read back the recovered application;
- retain the non-destructive adoption rule for PostgreSQL, NATS, MinIO, Harbor,
  Keycloak, BuildKit, Kubernetes and KubeVirt.

Owner: D connected Verify; A release judgment.

## Release Gate and human review

`cargo xtask release-gate` has not produced a passing Sprint 2 report for the
current identity. It remains blocked by the same-identity VM, Access/Gateway
negative matrix, current Container lifecycle cleanup, infrastructure identity
and rollback items above.

PR #121 is `risk:high`, remains Draft and must not use auto-merge. A and B must
approve the high-risk Contract/Schema/Migration/security paths, C must review
the Web changes, and D must complete connected Verify. The author cannot
replace those human approvals or merge the PR.

Exit: all required CI checks pass, review threads are resolved, B/C/D reviews
are recorded, and the one schema-valid report binds the exact commit,
deployment manifest, migration catalog, platform/runtime image digests, Run ID
and test results.
