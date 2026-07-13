# Service Boundaries

| Owner | Authoritative responsibility | Explicit non-responsibility |
| --- | --- | --- |
| Control Service | courses, projects, lab packages, template versions, publication approvals | workload creation, scoring, device policy |
| Access Service | AccessGrant, EndpointGrant, device mapping, policy revisions, enrollment eligibility and revocation | identity provider, environment scheduling, direct endpoint exposure |
| Environment Service | environment request, lifecycle intent, endpoint metadata, CRD submission | user identity, evaluation score |
| Agent Service | candidate specs, checkpoints, allowlisted tool calls | publication, deterministic scoring, cluster-admin shell |
| Evaluation Service | EvaluationSpec, EvaluationRun, StepRun, deterministic aggregation and evidence | LLM-derived numeric score |
| Resource Service | resource requests, approval, Lease and Capacity binding | environment internals and evaluation execution |

Cross-domain changes use a versioned REST contract, NATS event, immutable artifact reference, or controlled service call. A service must not directly mutate another domain's tables. PostgreSQL is authoritative for durable business state; JetStream is a reliable delivery mechanism, not a state store.

Provider selection is always an explicit manifest/configuration binding. Registration order and “first available” behavior are prohibited.

For the P0 external-access path, Keycloak authenticates users, Headscale/Tailscale supplies device identity and coarse private reachability, and Access Service authorizes a scoped endpoint through the Access Gateway. Network reachability is not authorization: container and VM user traffic may not bypass the Gateway through a Subnet Router or direct endpoint route. See [Access Trust Boundary](access-trust-boundary.md).
