# Data Ownership

The domain model uses one PostgreSQL cluster with independent schemas and
least-privilege logins. The production rules are defined by
[ADR 0002](../adr/0002-postgresql-schema-and-migration-policy.md); no schema,
role or Migration has been implemented by this documentation work.

| PostgreSQL schema | Business owner | Initial planned entities | Write boundary |
| --- | --- | --- | --- |
| `control` | Control Service | courses, projects, lab_packages, template_versions, publication_approvals | Control runtime and Migration identities only |
| `access` | Access Service | devices, access_grants, endpoint_grants, policy_revisions, preauth_issuances | Access runtime and Migration identities only |
| `environment` | Environment Service | environment_instances, endpoints, configuration_requests, configuration_runs | Environment runtime and Migration identities only |
| `agent` | Agent Service | agent_runs, checkpoints, tool_calls, generated_artifacts | Agent runtime and Migration identities only |
| `evaluation` | Evaluation Service | evaluation_specs, runs, step_runs, fragments, review_reports | Evaluation runtime and Migration identities only |
| `resource` | Resource Service | resource_requests, approvals, leases, capacity_claims | Resource runtime and Migration identities only |
| `shared_audit` | Control Service audit projection | sanitized audit_log, projection progress | restricted Control projection identity only; no business writes |

Each business domain owns a local Outbox and commits its business write,
idempotency record and Outbox row in one transaction. `shared_audit` is not a
shared Outbox and not a cross-domain business-write exception. Control Service
temporarily owns the append-only audit projection; it consumes versioned events
and may not write another domain's business schema.

Cross-domain records use stable identifiers and immutable version/hash
references. Cross-schema foreign keys, cascades, triggers and functions are
prohibited. Consumers must reject or idempotently handle duplicate, stale,
unsupported and replayed events. Runtime roles cannot run DDL or write another
domain schema; all schema evolution is the separately controlled Migration Job.
