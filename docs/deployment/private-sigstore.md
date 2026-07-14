# Private Sigstore trust plane

Issue #61 adds a repository-side, fail-closed deployment contract for a private
Fulcio, Rekor, CT log, Trillian and TUF trust plane. It does not claim that the
production Keycloak client, offline root ceremony, cluster deployment, restore
drill, Kyverno consumer or Release Gate consumer has been executed.

## Identity foundation prerequisite

The adopted cluster uses a separately managed Keycloak Stage 0 before Private
Sigstore. The steady-state router allowlisted entries are:

```sh
cargo xtask identity-foundation --infra --env <environment> --action deploy --yes
cargo xtask identity-foundation --infra --env <environment> --action verify --yes
```

The private-lab identity is fixed to
`https://keycloak.labweaver.internal/realms/workloads`, Gateway VIP
`10.20.0.222`, and confidential service-account client
`labweaver-buildkit`. A namespace-scoped self-signed bootstrap Issuer creates a
private CA; a separate namespace-scoped CA Issuer signs the Gateway
certificate. This is a private-lab bootstrap trust root, not a public PKI or an
offline Sigstore root.

Keycloak and PostgreSQL images are digest-pinned. PostgreSQL uses persistent
RWO storage, Keycloak runs two replicas, and interactive/direct-access grants
are disabled for the workload client. The root-managed bootstrap locator is
supplied through `LABWEAVER_IDENTITY_SECRET_LOCATOR`, must be root-owned mode
`0600`, and has its SHA-256 pinned in private inventory. Secret values and
tokens never enter reports or Git.

An initial interactive bootstrap may be performed from an approved WSL/Linux
controller with separate A and D TokenRequest kubeconfigs. A may reconcile
cluster infrastructure; D receives cluster observation plus run-scoped test
resource permissions and cannot read Secrets or mutate cluster-scoped RBAC.
These kubeconfigs are private, time-limited inputs and are never committed. The
interactive result must still be replayed through the allowlisted Ansible entry
before Issue closure.

## Controller boundary

The only repository entry is action-scoped and allowlisted:

```sh
cargo xtask private-sigstore --infra --env <environment> --action deploy --yes
cargo xtask private-sigstore --infra --env <environment> --action backup --yes
cargo xtask private-sigstore --infra --env <environment> --action restore --yes
cargo xtask private-sigstore --infra --env <environment> --action rotate --yes
cargo xtask private-sigstore --infra --env <environment> --action verify --yes
cargo xtask private-sigstore --infra --env <environment> --action cleanup --yes
cargo xtask private-sigstore --infra --env <environment> --action disaster-recovery --yes
```

It uses the same Linux-router identity, private inventory, Vault password file,
explicit run IDs, commit, inventory hash and component-lock hash as the adopted-cluster
Ansible controller. Windows returns `XTASK_INFRA_UNSUPPORTED_PLATFORM`.
Each action maps to one playbook compiled into `xtask`; callers cannot supply an
arbitrary path or playbook. Missing run identity or locator fails before Ansible.

The Private Sigstore inventory is deliberately narrower than the adopted-cluster
bootstrap inventory: it contains exactly the approved router and one
`k8s-cp1` control-plane target. Playbooks 96--102 select the fixed
`private-sigstore` preflight scope, which rejects Worker or NFS inventory
targets while retaining read-only control-plane checks for the existing storage
endpoint. The normal cluster and Harbor paths keep the full-cluster preflight
contract. Placeholder validation reports a stable task diagnostic without
printing private inventory values.

The controller requires three root-managed locators, never secret values. The
namespace and named Kubernetes Secrets must already be provisioned by the C0
secret ceremony; the deployment role never generates keys or imports raw key
material:

- `LABWEAVER_SIGSTORE_SECRET_LOCATOR` identifies the externally provisioned
  signing/TLS secret inventory identity, whose SHA-256 is pinned in private
  inventory;
- `LABWEAVER_SIGSTORE_TUF_ROOT_LOCATOR` identifies public offline-root metadata;
- `LABWEAVER_SIGSTORE_BACKUP_LOCATOR` identifies an existing-state backup and
  is mandatory before reconciling an existing namespace.

Missing namespace/Secret, placeholder, public-Sigstore, wildcard identity, chart-hash, TUF-root,
backup-identity or component-digest input stops before Helm mutation. Locators
and their content use `no_log`; neither a private key nor a client secret is
represented by a public contract or report.

## Locked deployment

`deploy/versions.lock.yml` pins scaffold `0.6.111`, Cosign `3.0.6`, the official
chart archive SHA-256 and all selected images by digest. The chart checksum was
computed from the official `sigstore/helm-charts` GitHub release asset. The
Cosign Linux AMD64 checksum comes from the official `sigstore/cosign` v3.0.6
release asset metadata. Deployment uses the local verified archive and cannot
fall back to a chart repository.

The chart's automatic certificate/key and tree creation jobs are disabled.
Signing keys and TUF root private keys must be created by reviewed C0 procedures
outside Git and injected through approved Secret locators. Rekor and CT tree IDs
are explicit pre-provisioned inputs; the CT config job consumes the external key
and tree identity but does not generate either one. Trillian consumes an external
MySQL Secret and never uses the chart's generated credential path. A default-deny
NetworkPolicy, internal-only allowlist, restricted namespace, ClusterIP
services, RFC1918 Gateway address, TLS Secret reference, digest verification,
resources and PodDisruptionBudget are applied. No public Fulcio/Rekor endpoint,
public Sigstore service or unsigned fallback is configured.

## Trust and recovery contracts

The Rust source of truth generates versioned schemas for workload identity,
the trust bundle and `PrivateSigstoreTestFlightReport`. Workload identity uses
exact issuer, audience/client ID and one Keycloak service-account certificate
subject; wildcards, human-user subjects and public Sigstore issuers are rejected.
The Keycloak client must disable interactive/direct-access grants and expose
only its service-account token; that runtime setting is an E3 prerequisite.
Trust bundles bind commit,
run, cluster, inventory, deployment manifest, component lock, Fulcio, Rekor,
CT and TUF identities. TUF validation rejects expiry and non-monotonic root or
metadata versions.

Before migration, rotation, upgrade or root replacement, operators must produce
an external `private-sigstore-backup.v1` artifact binding the same commit,
inventory, component lock, trust bundle and artifact hashes. Helm rollback alone
is not a data recovery plan. Restore, rotation and disaster recovery first run
the fixed backup provider and validate its same-run manifest. Lifecycle providers
are fixed-name, pre-approved cluster CronJobs; a missing provider or report fails
closed. Restore must verify the backup identity and both old and current-root
verification windows before traffic resumes. Cleanup may target only Job, Pod and
ConfigMap resources carrying the current TestFlight identity and must never delete
retained PVCs, signing Secrets, TUF metadata, workloads or namespaces. These
workflows remain unproved until the same router E3 run closes each report.

## Evidence status

Repository tests and schema drift checks are E1. Checksum-pinned Helm 3.18.4
lint/template plus Ansible lint/syntax can provide
E1/E2 controller evidence on Linux. Real Keycloak keyless signing, SCT and Rekor
inclusion, second-deploy idempotency, backup/restore, root rotation and cleanup
must remain `blocked` or `not_run` in reports until one identity-bound router E3
run completes. Static fixtures cannot promote those checks.
