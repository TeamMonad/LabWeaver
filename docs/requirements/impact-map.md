# Impact Map

## Goal

Deliver teaching experiments and research work environments whose creation,
access, evaluation, evidence, and recovery are observable, authorized, and
repeatable. An Agent may propose a candidate, but deterministic validation and
human approval control every production path.

## Actors, outcomes, and constraints

| Actor | Desired outcome | Platform capability | Dependency | Risk if absent or incorrect |
| --- | --- | --- | --- | --- |
| Teacher | Publish a reviewed OJ or Linux experiment without hidden configuration drift. | Candidate EnvironmentSpec/EvaluationSpec, deterministic validation, approval, immutable published version. | Agent, Environment, Evaluation, policy and artifact owners. | Unsafe or incomplete material is published, or an unreviewed candidate reaches students. |
| Student | Use the assigned environment, submit allowed artifacts, and understand deterministic results. | Ready-state progress, authorized endpoint, frozen submission, result timeline and evidence. | Environment, Access, Collector, Evaluation and artifact storage. | Inconsistent environments, lost submission files, inaccessible evidence, or misleading scores. |
| Research user | Obtain and configure a bounded Work environment and request resources. | Lease-bound work lifecycle, approved configuration plan, capacity request and audit trail. | Environment, Resource, Access, BuildKit/Ansible policy. | Unbounded configuration, retained access after expiry, or unaccounted resource usage. |
| Platform administrator | Keep capacity, identity, policy, and deployment safe and recoverable. | OIDC/Headscale integration, AccessGrant policy, fixed-version deployment, preflight, verify, rollback. | Keycloak, Headscale, Kubernetes/KubeVirt, Ansible and observability. | Public exposure, privilege expansion, unrecoverable deployment, or unaudited policy changes. |
| Test and demonstration operator | Replay the same supported flow and preserve failure evidence. | Playwright golden paths, fixed fixtures/seeds, trace, screenshot, video and `cargo xtask demo replay`. | Web, services, deployment fixture and Playwright. | A demo-only bypass diverges from tested behavior or failures cannot be diagnosed. |

## Outcome map

| Outcome | Stories | Observable success | Mandatory failure behavior |
| --- | --- | --- | --- |
| Reviewed teaching definition is published | US-01, US-03, US-04, US-05 | Approved immutable version references validated inputs and a recorded approver. | Invalid schema, unsafe tool/path, missing dependency, failed validation, or absent approval blocks publication with a stable diagnostic. |
| Assigned environment is usable only while authorized | US-02, US-06, US-09 | Same template revision reaches `Ready`; an authorized subject can reach the registered healthy endpoint through the approved network path. | Non-Ready, unhealthy, expired, revoked, untrusted, or unauthorized access is denied fail closed; no public endpoint is introduced. |
| Submission and result are reproducible | US-03, US-04, US-05 | Immutable submission/evaluation identity is bound to deterministic results and sanitized evidence. | Hash, schema, tool, fixture, version, or input mismatch blocks execution or result publication; advisory output cannot alter numeric score. |
| Resource and operations are governed | US-07, US-08 | Approved request, lease/capacity state, deployment verification, and recovery record are observable. | Missing approval, invalid preflight, failed policy, expiry, or failed verify blocks the next state and records the cause. |
| Release evidence can be replayed | US-10 | Golden path finishes with a traceable run identity and retained failure artifacts. | A failed replay preserves trace, screenshot, and video and cannot be represented as a passed release check. |

## Cross-cutting guardrails

- PostgreSQL is authoritative business state; messages, logs, UI state, and
  cache do not replace it.
- Provider selection is explicit and versioned. A registry order or an
  implicitly available test provider is never a production fallback.
- LLM feedback is advisory only. It cannot write deterministic scores, change
  Gate results, or bypass teacher approval.
- Inputs, artifacts, evaluations, and evidence retain their applicable schema
  version, SHA-256, tool/fixture version, and build identity.
- Failure diagnostics are stable, structured, and sanitized; secrets, raw
  submissions, private paths, and credentials are never evidence payloads.
