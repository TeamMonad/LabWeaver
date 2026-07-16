# Issue #48 Control-plane deployment and rollback

## Configuration boundary

Control and Agent read one reviewed YAML document from
`LABWEAVER_CONTROL_CONFIG_FILE` and `LABWEAVER_AGENT_CONFIG_FILE`. The repository examples are
`deploy/config/control-plane.yaml.example` and
`deploy/config/agent-control-plane.yaml.example`. Database passwords, object-store credentials,
NATS credentials and private keys are file locators; secret values do not belong in YAML, logs,
reports or Git.

The deployment must provision these identities before either service starts:

- Gateway client CA and exact URI SAN accepted by Control;
- Control client certificate accepted by Access and Agent;
- Agent and Control server certificates whose DNS SAN matches each configured HTTPS base URL;
- versioned MinIO bucket with an allowlist restricted to the configured object prefix;
- JetStream stream and durable consumers named by configuration;
- Control and Agent runtime database roles with only their own schema privileges.

The reviewed Control configuration also pins the active image-policy ID/revision, private
trust-bundle SHA-256, Fulcio issuer and workload certificate subject. Publication rejects evidence
that differs from any of these values even when the artifact payload is otherwise well formed.
Rotation therefore requires a reviewed configuration rollout and new authoritative evidence; no
historical trust policy is selected implicitly.

Control publishes both Release publication and withdrawal facts from its PostgreSQL Outbox. The
publisher uses the configured bounded ACK timeout and poll interval and marks `published_at` only
after JetStream persistence acknowledgement. A restart therefore retries an unacknowledged fact
with the same CloudEvent ID. Consumers must process aggregate sequence `1` (publication) before
sequence `2` (withdrawal), and must reject new Environment creation from a withdrawn release.

For ProblemPackage completion, the client manifest hash is the canonical hash of the sorted
original upload declaration. Control separately computes the immutable completed-package manifest
hash after freezing every exact MinIO object version. This keeps the client-verifiable upload
contract distinct from the server-owned object-version identity.

Apply the additive Migration catalog through the controlled entry point before starting the new
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
3. Confirm committed Outbox rows and immutable packages, decisions, releases and withdrawals
   remain present; do not delete or rewrite them.
4. Roll back the application images while retaining additive Control Migration `0002` and Agent
   Migration `0003`.
5. If a schema correction is required, ship a reviewed forward Migration. Do not edit a released
   Migration or its catalog hash.

Rollback does not withdraw an EnvironmentTemplateRelease. A withdrawal is a separate append-only
fact. A functional rollback to older material creates a higher release version referencing a
still-valid verified candidate and authoritative artifact evidence.

## Current production blocker

Issue #48 deliberately does not build or verify images. Until #52/#53 expose the authoritative
`ImageArtifact` and `ImagePolicyEvaluation` through Agent, Release creation fails closed. Fixture
artifacts and static reports are not production publication evidence and do not raise this path
to E3.
