# Service Boundaries

| Owner | Authoritative responsibility | Explicit non-responsibility |
| --- | --- | --- |
| Control Service | courses, projects, lab packages, template versions, publication approvals | workload creation, scoring, device policy |
| Access Service | AccessGrant, DirectAccessGrant, EndpointGrant, device mapping, policy/firewall revisions, enrollment eligibility and revocation | identity provider, environment scheduling, direct endpoint exposure implementation |
| Environment Service | environment request, lifecycle intent, endpoint metadata, scoped SSH/VNC credentials, CRD submission | user identity, evaluation score, business authorization |
| Agent Service | candidate specs, checkpoints, allowlisted tool calls | publication, deterministic scoring, cluster-admin shell |
| Evaluation Service | EvaluationSpec, EvaluationRun, StepRun, deterministic aggregation and evidence | LLM-derived numeric score |
| Resource Service | resource requests, approval, Lease and Capacity binding | environment internals and evaluation execution |

Cross-domain changes use a versioned REST contract, NATS event, immutable artifact reference, or controlled service call. A service must not directly mutate another domain's tables. PostgreSQL is authoritative for durable business state; JetStream is a reliable delivery mechanism, not a state store. The [NATS Event Contract v1](../contracts/nats-event-contract-v1.md) assigns every public Subject to a state Owner and handling purpose; controlled workers do not become independent state owners.

Provider selection is always an explicit manifest/configuration binding. Registration order and “first available” behavior are prohibited.

For the P0 external-access path, Keycloak authenticates users and Access Service is the authorization truth. HTTP endpoints use Access Gateway. Native SSH/VNC uses only an active DirectAccessGrant through exact Headscale Grants and Router enforcement; browser SSH/VNC uses the Guacamole handoff path. Network reachability is not authorization, and no derived policy, Router or Guacamole state may independently allow an endpoint. See [Access Trust Boundary](access-trust-boundary.md).
