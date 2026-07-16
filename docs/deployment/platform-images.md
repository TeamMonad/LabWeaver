# Platform Image Trusted Supply Chain

Issue #62 owns the seven production platform images: `control-service`,
`access-service`, `resource-service`, `environment-service`, `agent-service`,
`evaluation-service`, and `web`. The only supported P0 target is
`linux/amd64`. The package and deployment manifests reference Harbor images by
digest; tags are operational aliases and are never deployment authority.

## Locked inputs

`deploy/versions.lock.yml` is the source of truth for BuildKit/buildx, Trivy,
Cosign, Kyverno CLI, Helm, pnpm, and every builder/runtime image. Packaging
fails when an executable or base identity differs from the lock. Container
builds use `Cargo.lock`, `pnpm-lock.yaml`, `SOURCE_DATE_EPOCH`, BuildKit
provenance/SBOM generation, and timestamp rewriting. The final images run as a
fixed non-root user and contain no compiler or package manager.

The build context is governed by `.dockerignore`. Both the context and the
resulting images are secret-scanned. A secret or Critical vulnerability is a
hard failure. High vulnerabilities remain visible in the signed package
evidence and require an explicit human risk decision.

## Commands and trust inputs

```sh
cargo xtask package --env adopted --release <release-id> --yes
cargo xtask package-validate \
  --manifest artifacts/platform-images/<release-id>/PlatformImagePackageManifest.json \
  --mode static
cargo xtask package-validate \
  --manifest artifacts/platform-images/<release-id>/PlatformImagePackageManifest.json \
  --mode connected --env adopted
cargo xtask deploy --env adopted \
  --package-manifest artifacts/platform-images/<release-id>/PlatformImagePackageManifest.json \
  --yes
cargo xtask rollback --env adopted --release-revision <revision> --yes
```

Connected packaging and validation run only on the controlled Linux router.
They require explicit environment locators for the Harbor registry, Trivy DB
identity, private Sigstore trusted root, Fulcio and Rekor endpoints, workload
identity token, exact issuer/subject, and trust revision. Deployment additionally
requires private Helm values and kubeconfig locators. The repository and result
manifests contain only sanitized locators and hashes, never credentials.

Static validation proves only schema, exact component set, digest-only image
references, locked identity, Helm rendering, Kyverno fixtures, and evidence
shape. Connected validation rereads OCI digests, attestations, scanner DB
identity, certificate/SCT/Rekor proof, and the active trust bundle. It cannot
fall back to static mode.

## Atomic publication and rollback

Each component is built twice and the resulting digest must match. Packaging
then pushes the digest, records BuildKit SBOM/provenance, runs Trivy, signs with
the private Sigstore identity, verifies certificate and transparency proof, and
only then atomically renames the canonical package manifest into place. A failed
stage can leave an unreferenced Harbor digest, but never a deployable manifest;
the tool does not move tags or delete shared registry data.

Helm values must provide all seven digests, resources, existing Config/Secret
locators, and the current plus explicitly recorded previous trust identity.
Workloads use a read-only root filesystem, `RuntimeDefault` seccomp, no privilege
escalation, no capabilities, no ServiceAccount token, and HTTP live/ready probes.
Kyverno admits only `labweaver-system` Harbor digests with the exact registered
private Sigstore identity. Rollback consumes a previously connected-verified
package manifest and its Helm revision; it never moves tags or relaxes policy.

## Evidence and stop rules

PR CI may produce E1/E2 build, reproducibility, secret/SBOM/Trivy, Helm,
Kyverno, and static-manifest evidence. It has no Harbor or signing credentials.
E3 is valid only when the controlled run binds one source commit, cluster UID,
trust bundle revision, package/deployment manifests, and all seven OCI digests.

Before E3, record a read-only cluster baseline. Stop immediately if another
deployment may be affected. Missing post-merge Issue #61 identity replay,
production Config/Secret locators, Harbor/BuildKit/Kyverno readiness, or any
real dependency keeps Issue #62 blocked; fixtures must not substitute for them.
