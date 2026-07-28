# Active Blockers

This file contains only blockers for Draft PR #121 on `release/sprint2`.
Historical evidence cannot satisfy a current same-identity gate.

## Current evidence identity

- Current PR head is read from Git/PR metadata; the connected application
  source is `24928e8f06e1bc9709c4c493b7d95b2b007f522c`.
- Connected package: `pkg-demo-sprint2-24928e8f-24928e8f06e1`.
- Non-destructive application runs: `deploy-24928e8f` and
  `deploy-24928e8f-replay`.
- Both reconciles completed and all ten workloads read back ready on immutable
  Harbor digests. Retained infrastructure was not reset.
- Current-identity Container Agent, approval, build, publication, access,
  freeze, stop/start/delete and cluster absence readback pass.
- Current-identity Playwright timed out without a report and remains blocked.

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
