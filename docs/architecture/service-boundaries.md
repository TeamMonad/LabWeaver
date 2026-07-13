# Service Boundaries

| Owner | Authoritative responsibility | Explicit non-responsibility |
| --- | --- | --- |
| Control Service | courses, projects, lab packages, template versions, publication approvals | workload creation, scoring, device policy |
| Access Service | AccessGrant, EndpointGrant, device mapping, policy revisions, revocation | identity provider, environment scheduling |
| Environment Service | environment request, lifecycle intent, endpoint metadata, CRD submission | user identity, evaluation score |
| Agent Service | candidate specs, checkpoints, allowlisted tool calls | publication, deterministic scoring, cluster-admin shell |
| Evaluation Service | EvaluationSpec, EvaluationRun, StepRun, deterministic aggregation and evidence | LLM-derived numeric score |
| Resource Service | resource requests, approval, Lease and Capacity binding | environment internals and evaluation execution |

Cross-domain changes use a versioned REST contract, NATS event, immutable artifact reference, or controlled service call. A service must not directly mutate another domain's tables. PostgreSQL is authoritative for durable business state; JetStream is a reliable delivery mechanism, not a state store. Database role boundaries, domain-local Outboxes and the temporary Control-owned audit projection are defined in [ADR 0002](../adr/0002-postgresql-schema-and-migration-policy.md). The [NATS Event Contract v1](../contracts/nats-event-contract-v1.md) assigns every public Subject to a state Owner and handling purpose; controlled workers do not become independent state owners.

Provider selection is always an explicit manifest/configuration binding. Registration order and “first available” behavior are prohibited.
