# Service Boundaries

## Sprint 2 deployment profile

The six logical service and PostgreSQL ownership boundaries remain. The Sprint 2
runtime enables Control, Access, Agent, Environment and Web. Evaluation and
Resource remain disabled until they own a real product path; a health-only
Deployment is not evidence of a capability.

Agent and Environment each own a separate executor process built from their
existing image. API processes do not hold BuildKit/Harbor/Trivy or
Kubernetes/KubeVirt credentials. The executor processes communicate through
the explicitly bound NATS subjects and keep domain-specific credentials and
fencing state separate.

Private Sigstore, Kyverno, Packer and Headscale/Tailnet are not
Sprint 2 deployment units. See ADR 0011.

| Owner | Authoritative responsibility | Explicit non-responsibility |
| --- | --- | --- |
| Control Service | courses, projects, lab packages, template versions, publication approvals | workload creation, scoring, device policy |
| Access Service | AccessGrant, EndpointGrant, device mapping, policy revisions, revocation | identity provider, environment scheduling |
| Environment Service | environment instance, revisioned desired/observed lifecycle state, lifecycle operations, endpoint metadata and CRD submission | user identity, Work Lease authority, grant issuance/revocation, evaluation score |
| Access Service | AccessGrant, DirectAccessGrant, EndpointGrant, device mapping, policy/firewall revisions, enrollment eligibility and revocation | identity provider, environment scheduling, direct endpoint exposure implementation |
| Environment Service | environment request, lifecycle intent, endpoint metadata, scoped SSH/VNC credentials, CRD submission | user identity, evaluation score, business authorization |
| Agent Service | candidate specs, checkpoints, allowlisted tool calls | publication, deterministic scoring, cluster-admin shell |
| Evaluation Service | EvaluationSpec, EvaluationRun, StepRun, deterministic aggregation and evidence | LLM-derived numeric score |
| Resource Service | resource requests, approval, Lease and Capacity binding | environment internals and evaluation execution |

Cross-domain changes use a versioned REST contract, NATS event, immutable artifact reference, or controlled service call. A service must not directly mutate another domain's tables. PostgreSQL is authoritative for durable business state; JetStream is a reliable delivery mechanism, not a state store. Database role boundaries, domain-local Outboxes and the temporary Control-owned audit projection are defined in [ADR 0002](../adr/0002-postgresql-schema-and-migration-policy.md). The [NATS Event Contract v1](../contracts/nats-event-contract-v1.md) assigns every public Subject to a state Owner and handling purpose; controlled workers do not become independent state owners.

Provider selection is always an explicit manifest/configuration binding. Registration order and “first available” behavior are prohibited.

`Experiment` / `Work` and `Container` / `VirtualMachine` are independent
environment dimensions. The Environment Service may accept an Experiment from a
published release, while a Work create/start requires an Active Resource Lease.
Resource Service publishes Lease state; it does not mutate environment records.
Access Service issues or renews grants only for Environment-observed `Ready`
instances with healthy registered endpoints, and revokes relevant grants before
the Environment Service stops or cleans up an expired, failed or deleting
instance. The proposed detailed contract is
[`EnvironmentLifecycle v1`](../contracts/environment-lifecycle-v1.md).
For the P0 external-access path, Keycloak authenticates users and Access Service is the authorization truth. HTTP endpoints use Access Gateway. Browser xterm/noVNC uses the AccessGrant-scoped ConsoleCapability handoff, then the Access proxy and Environment bridge defined by ADR 0012; native SSH/VNC remains a deferred DirectAccessGrant proposal. Network reachability is not authorization, and no derived policy or proxy state may independently allow an endpoint. See [Access Trust Boundary](access-trust-boundary.md).
