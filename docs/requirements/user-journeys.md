# User Journeys

Each journey defines target observability, not current runtime completion. A
stage succeeds only when its business state and required evidence are
observable; accepting a request is not the same as completing it.

## Teacher: define and publish an OJ or Linux experiment

| Stage | User action | Owner | Observable success | Failure signal and disposition |
| --- | --- | --- | --- | --- |
| Prepare | Upload statement, starter, samples, and constraints. | Control/Agent | Material manifest identifies supplied and missing inputs. | Missing or unsafe input is recorded; candidate generation is blocked rather than guessed. |
| Generate | Request candidate environment and evaluation definitions. | Agent | Candidate has schema/version identity, assumptions, risks, and explicit provider bindings. | Invalid candidate, forbidden tool/path, or missing binding returns a stable blocking diagnostic. |
| Validate | Review deterministic schema, policy, smoke, differential, mutation, or probe evidence. | Environment/Evaluation | Validation record identifies inputs, tool/fixture versions, outcome, and evidence locator. | Any failed check prevents approval and preserves only sanitized diagnostic evidence. |
| Approve and publish | Teacher approves a validated candidate. | Control | Immutable published version records approver, source hashes, revision, and approval time. | Missing approval or identity mismatch rejects publication; no implicit latest version is selected. |
| Review results | Inspect deterministic results and advisory feedback. | Evaluation | Result timeline distinguishes deterministic fragments from advisory feedback. | Missing/invalid evidence or protected score field in advisory output blocks result finalization. |

## Student: enter, submit, and inspect an experiment

| Stage | User action | Owner | Observable success | Failure signal and disposition |
| --- | --- | --- | --- | --- |
| Enter | Start assigned Container or VM through the authorized access path. | Environment/Access | Requested revision reaches `Ready`; registered endpoint health and grant decision are visible. | Non-Ready, unhealthy, unauthorized, expired, revoked, or untrusted-device state denies new access. |
| Work | Use code-server, SSH, or VNC as allowed by the experiment. | Environment | Template revision, lease state, and endpoint ownership are visible. | Lease expiry, requested stop, or failure revokes grants before cleanup; no direct public exposure is used. |
| Submit | Freeze the allowed files and requested evidence. | Collector/Evaluation | Submission manifest, SHA-256, schema version, and source identity are stored as immutable input. | Unsafe/missing path, hash conflict, or collection failure produces no publishable submission. |
| Inspect result | View tests, assertions, evidence, and advisory guidance. | Evaluation/Web | Timeline joins run/step identity to sanitized deterministic evidence and separately labeled advice. | Failed/cancelled/invalid execution shows its terminal state and diagnostic; no score is invented. |

## Research user: request and operate a Work environment

| Stage | User action | Owner | Observable success | Failure signal and disposition |
| --- | --- | --- | --- | --- |
| Request | Request a Work environment and capacity. | Resource/Environment | Request has actor, course/project scope, requested capacity, and approval state. | Invalid quota, absent approval, or unsupported capacity blocks provisioning. |
| Configure | Request a software change in natural language. | Agent/Environment | Candidate BuildKit/Ansible plan is diffable, policy-checked, and tied to the environment revision. | Arbitrary shell, prohibited source, privilege escalation, or policy failure is rejected before execution. |
| Use and renew | Use granted endpoint during an active Lease. | Resource/Access | Lease, grant revision, endpoint health, and renewal outcome are observable. | Expiry/revocation fails closed for new access and triggers ordered cleanup. |
| Recover | Rebuild or roll back an approved configuration. | Environment | Recovery target and resulting revision are auditable. | Failed recovery reaches an explicit failed state with cleanup/audit evidence. |

## Platform administrator: deploy and govern

| Stage | User action | Owner | Observable success | Failure signal and disposition |
| --- | --- | --- | --- | --- |
| Preflight | Validate cluster, KubeVirt, storage, identity, and network prerequisites. | DevOps/Ansible | Versioned preflight report identifies each prerequisite and build/deployment identity. | Missing or incompatible prerequisite blocks deploy without partial success. |
| Deploy | Apply fixed-version Ansible/Helm/Kubernetes configuration. | DevOps/Ansible | Deployment records controlled inputs, revisions, and outcome. | Failed policy, migration, or apply is surfaced; no unknown schema is repaired on startup. |
| Verify | Run controlled service, security, and recovery checks. | DevOps/Ansible | Verify report is machine-readable and tied to deployed identity. | Invalid, stale, or incomplete report cannot satisfy Release Gate. |
| Recover | Upgrade, roll back, or destroy through the controlled entrypoint. | DevOps/Ansible | Selected recovery operation and result are auditable. | Failed recovery is a blocker with original diagnostic and named exit condition. |

## Test and demonstration operator: replay evidence

| Stage | User action | Owner | Observable success | Failure signal and disposition |
| --- | --- | --- | --- | --- |
| Prepare | Select fixed seed, fixture, role states, and target build. | Test/DevOps | Run input identity is recorded before execution. | Missing fixture, role state, or build identity blocks replay. |
| Replay | Run the supported Playwright golden path or demo command. | Test/DevOps | Business-state waits complete without fixed sleeps; run produces an identifiable result. | Failure retains trace, screenshot, and video according to policy. |
| Review | Compare result with requirement and release evidence target. | Test/Release Gate | Evidence names requirement IDs, test identity, and build identity. | Fixture-only or stale evidence remains below the required tier and cannot close a P0 gate. |
