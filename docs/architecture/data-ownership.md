# Data Ownership

| PostgreSQL schema | Owner | Initial planned entities |
| --- | --- | --- |
| `control` | Control Service | courses, projects, lab_packages, template_versions, publication_approvals |
| `access` | Access Service | devices, access_grants, endpoint_grants, policy_revisions, preauth_issuances |
| `environment` | Environment Service | environment_instances, endpoints, configuration_requests, configuration_runs |
| `agent` | Agent Service | agent_runs, checkpoints, tool_calls, generated_artifacts |
| `evaluation` | Evaluation Service | evaluation_specs, runs, step_runs, fragments, review_reports |
| `resource` | Resource Service | resource_requests, approvals, leases, capacity_claims |
| `shared_audit` | Append-only audit boundary | audit_log, outbox_events, event_projection |

All writes use a transaction plus Outbox or an equivalent atomic boundary. Consumers must reject or idempotently handle duplicates, stale events, unsupported versions and replay. No schema or Migration is implemented by ARC-01a; the table records the frozen ownership decision only.
