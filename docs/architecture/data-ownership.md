# Data Ownership

The domain model uses one PostgreSQL cluster with independent schemas and
least-privilege logins. `platform_meta` is deployment metadata, not a business
domain; no runtime service login can access it. The production rules are defined by
[ADR 0002](../adr/0002-postgresql-schema-and-migration-policy.md); no schema,
role or Migration has been implemented by this documentation work.

| PostgreSQL schema | Business owner | Initial planned entities | Write boundary |
| --- | --- | --- | --- |
| `platform_meta` | deployment release coordinator | release ledger, lock-attempt and report identities | short-lived provisioner and restricted release-coordinator only; no runtime service access |
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
The short-lived provisioner owns initial schema creation, `PUBLIC` revocation
and owner-specific default privileges. Each domain Migration login owns its
Migration history, while runtime has only its own schema DML plus read-only
history validation access. Connection pools cannot share identities or widen
their fixed `search_path`.
| PostgreSQL schema | Owner | Initial planned entities |
| --- | --- | --- |
| `control` | Control Service | courses, projects, lab_packages, template_versions, publication_approvals |
| `access` | Access Service | devices, access_grants, endpoint_grants, policy_revisions, preauth_issuances |
| `environment` | Environment Service | environment_instances (desired/observed state and revision), environment_operations, endpoints, configuration_requests, configuration_runs, cleanup_evidence |
| `agent` | Agent Service | agent_runs, checkpoints, tool_calls, generated_artifacts |
| `evaluation` | Evaluation Service | evaluation_specs, runs, step_runs, fragments, review_reports |
| `resource` | Resource Service | resource_requests, approvals, leases, capacity_claims |
| `shared_audit` | Append-only audit boundary | audit_log, outbox_events, event_projection |

All writes use a transaction plus Outbox or an equivalent atomic boundary. Consumers must reject or idempotently handle duplicates, stale events, unsupported versions and replay. No schema or Migration is implemented by ARC-01a; the table records the frozen ownership decision only.

For the proposed environment lifecycle, `environment_instances` references a
Resource-owned Lease and Access-owned grants but never owns or writes them. A
future deleted instance remains an Environment-owned audit tombstone with
sanitized cleanup evidence; it is not a cross-service cache or a recoverable
replacement for Resource or Access history.
