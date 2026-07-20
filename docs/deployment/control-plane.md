# Issue #48 Control-plane deployment and rollback

## Configuration boundary

Control and Agent read one reviewed YAML document from
`LABWEAVER_CONTROL_CONFIG_FILE` and `LABWEAVER_AGENT_CONFIG_FILE`. The repository examples are
`deploy/config/control-plane.yaml.example` and
`deploy/config/agent-control-plane.yaml.example`. Database passwords, object-store credentials,
NATS credentials and private keys are file locators; secret values do not belong in YAML, logs,
reports or Git.

The Sprint 2 Claude Code worker has one deployment-owned provider binding: the reviewed ECNU
Anthropic-compatible base URL from `agent-service-config/anthropic-base-url`, and the
operator-provided `ECNU_API_KEY` mounted as
`agent-service-secrets/anthropic-auth-token`. The env-cleared child receives these as
`ANTHROPIC_BASE_URL` and `ANTHROPIC_AUTH_TOKEN`. Missing or empty input blocks startup; the worker
does not inherit an ambient provider credential and has no fallback endpoint.

The deployment must provision these identities before either service starts:

- Gateway client CA and exact URI SAN accepted by Control;
- Control client certificate accepted by Access and Agent;
- Agent and Control server certificates whose DNS SAN matches each configured HTTPS base URL;
- versioned MinIO bucket with an allowlist restricted to the configured object prefix;
- JetStream stream and durable consumers named by configuration;
- Control and Agent runtime database roles with only their own schema privileges.

The reviewed Control configuration pins the active image-policy ID and revision.
Publication accepts only an approved private Harbor repository, immutable digest
and matching Trivy scanner/database identity and vulnerability gate. There is no
signing trust-plane configuration in the Sprint 2 contract.

Control publishes both Release publication and withdrawal facts from its PostgreSQL Outbox. The
publisher uses the configured bounded ACK timeout and poll interval and marks `published_at` only
after JetStream persistence acknowledgement. A restart therefore retries an unacknowledged fact
with the same CloudEvent ID. Consumers must process aggregate sequence `1` (publication) before
sequence `2` (withdrawal), and must reject new Environment creation from a withdrawn release.

For ProblemPackage completion, the client manifest hash is the canonical hash of the sorted
original upload declaration. Control separately computes the immutable completed-package manifest
hash after freezing every exact MinIO object version. This keeps the client-verifiable upload
contract distinct from the server-owned object-version identity.

After the non-destructive retained-infrastructure inventory confirms that each
domain has no business relations and an empty migration ledger, apply the Sprint
2 baseline catalog through the controlled entry point before starting the new
processes:

```sh
cargo xtask migrate --yes
cargo xtask contracts check
```

Service startup verifies required tables and exits with a stable diagnostic when schema,
certificate, secret locator, provider binding or durable consumer configuration is absent.
Startup never repairs an unknown schema.

## Rollback

1. Stop admission of new Control mutations at the trusted Gateway.
2. Stop new Agent dispatch claims, then allow bounded work to finish or request cancellation.
3. Confirm immutable packages and the current baseline identity remain present.
4. Roll back only to an image set verified against the same Sprint 2 baseline.
5. After Sprint 2 publication, schema corrections use reviewed forward Migrations;
   rollback never drops or rewrites retained infrastructure state.

Rollback does not withdraw an EnvironmentTemplateRelease. A withdrawal is a separate append-only
fact. A functional rollback to older material creates a higher release version referencing a
still-valid verified candidate and authoritative artifact evidence.

## Current production blocker

The local v1 build and release path is implemented. Connected BuildKit, Harbor,
Trivy, Container and KubeVirt replay under one deployment identity is still
required before Sprint 2 can claim verified deployment. Fixtures and static
reports are not production publication evidence.
