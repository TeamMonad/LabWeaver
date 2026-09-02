# Sprint 2 NATS Event Contract v1

## Scope

`crates/contracts/src/events.rs` and the generated
`schemas/contracts/v1/events/` files are the semantic source of truth. Sprint 2
has one event version only. It carries Agent, Build, Environment lifecycle,
Access expiry/revocation/session, release publication and Submission freeze
messages. Evaluation execution/scoring and Resource approval events are not in
the active catalog.

Ordinary queries, authorization decisions, candidate validation and owner
lookups use direct APIs. They do not create event projections merely to cross a
service boundary.

## Envelope and delivery

Every message is a strict CloudEvents 1.0 JSON object. Its `type`, `subject`,
`source`, `dataschema`, course, aggregate revision/sequence and trace identity
must match the registered `EventContract`. The canonical deduplication key is
the event ID. Missing or mismatched metadata, an unsupported version, a stale
generation or protected payload field is rejected before a business write.

Long-running commands use explicit durable consumers, finite `AckWait`, bounded
backoff and `MaxDeliver`. A consumer acknowledges only after its transaction or
durable fence commits. Duplicate delivery is idempotent; stale/out-of-order
delivery, deadline, cancellation and exhausted retry budget produce a stable
diagnostic and no speculative success. Terminal invalid messages are
quarantined with bounded metadata, never silently skipped.

Control consumes AgentRun and build results from `LABWEAVER_AGENT_EVENTS`.
Its sanitized quarantine records therefore use distinct
`labweaver.agent.quarantine.control_*` subjects covered by that retained
stream. Deployment fails before rollout if the configured quarantine subjects
fall outside the stream or the mounted Control user JWT cannot publish both
subjects. Foundation authoring grants Control only the bounded
`labweaver.agent.quarantine.>` family in addition to its existing subjects; a
quarantine publish failure is never treated as a successful terminal
acknowledgement.

Payloads contain IDs, immutable references, SHA-256 values and bounded status
summaries. They never contain credentials, raw submissions, full logs, direct
endpoints or arbitrary executable text. Large files and logs remain artifacts;
messages carry only their locator, hash, type, size and safe summary.

## Active subject families

| Family | Purpose |
| --- | --- |
| `labweaver.agent.run.*.v1` | request and terminal result of a Claude Code AgentRun |
| `labweaver.control.agent_build.requested.v1` and `labweaver.agent.build.*.v1` | approved fixed BuildRequest and BuildKit/Harbor/Trivy terminal result |
| `labweaver.control.lab_release.approved.v1` | approved Draft publication input |
| `labweaver.control.environment_template_release.*.v1` | immutable release publish/withdraw facts |
| `labweaver.environment.instance.*.v1` | provision, lifecycle, observation and deletion |
| `labweaver.access.grant.*.v1` | grant decision, activation, expiry and revocation |
| `labweaver.access.ssh_key.revoked.v1` | key revocation without key body |
| `labweaver.access.session.*.v1` | termination request, close receipt and overdue failure |
| `labweaver.access.console_session.state_changed.v1` | metadata-only console open, active, termination, overdue and close lifecycle |
| `labweaver.evaluation.submission.*.v1` | freeze request and immutable FrozenSubmission fact; no evaluation is scheduled |

The exact subjects and schemas are generated from `EVENT_CONTRACTS`; this table
is explanatory and must not be extended independently.

## Worker boundaries

Build Executor and the Container/KubeVirt executors are worker modes of their
owning services, not new business services. A deployment binds each worker to
fixed subjects and one owner-approved operation set. Workers do not discover a
provider by registration order, accept arbitrary shell text, or create a second
source of lifecycle truth.

The active build-completed event contains the immutable Harbor digest, Trivy
scanner/database identity, vulnerability counts and gate result. It contains no
Sigstore, attestation, SBOM or provenance evidence.

## Verification

Contract generation/check must have zero byte drift. Integration tests cover
duplicate, reordered, stale generation, restart replay, cancellation, deadline,
quarantine and dependency failure. Connected Release Gate evidence additionally
binds the deployed stream/consumer configuration to the same commit,
deployment manifest, migration catalog, image digests and Run ID.
