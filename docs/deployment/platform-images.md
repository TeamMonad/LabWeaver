# Sprint 2 platform images

Sprint 2 packages the enabled platform workloads into Harbor and deploys only immutable digest references. The authoritative package evidence is `PlatformImagePackageManifest.json`, validated by `schemas/results/platform-image-package-manifest.v1.schema.json`.

## Included images

- `control-service`
- `access-service`
- `agent-service` (also runs `build-executor`)
- `environment-service` (also runs separate `container-executor` and
  `kubevirt-executor` processes with independent identities)
- `openssh-gateway`
- `web`

Evaluation and Resource remain separate code domains but are not packaged or deployed by the Sprint 2 profile.

## Package gate

Run on the approved Linux packaging host:

```sh
export LABWEAVER_PLATFORM_REGISTRY=harbor.example.internal
export LABWEAVER_TRIVY_DATABASE_REFERENCE=harbor.example.internal/cache/trivy-db@sha256:<digest>
export LABWEAVER_TRIVY_DATABASE_DIGEST=sha256:<digest>
cargo xtask package --env demo --release sprint2 --yes
```

The command fails unless the source tree is clean, BuildKit/Buildx/Trivy match `deploy/versions.lock.yml`, the Trivy database is digest pinned, both reproducibility builds resolve to the same `linux/amd64` manifest digest, no secret or critical vulnerability is found, and every image is recorded as a Harbor digest reference. When Buildx uses the cluster-owned remote driver, `LABWEAVER_KUBECONFIG` is mandatory and packaging also reads back the `labweaver-build/buildkit` Deployment: its rootless image must match the locked digest, exactly one replica must be ready and updated, and its reviewed configuration hash annotation must be present. High vulnerabilities remain visible in the report but do not silently alter the gate.

The package manifest records the source commit, component lock hash, builder versions, Harbor host, image digests, Trivy version/database identity, vulnerability counts, and content hash of each retained scan report. Sprint 2 intentionally does not generate or validate Sigstore, Fulcio, Rekor, CT, TUF, SBOM, provenance, or Kyverno evidence.

## Connected validation and deployment

```sh
cargo xtask package-validate --manifest artifacts/package/<run-id>/PlatformImagePackageManifest.json --mode connected --env demo
cargo xtask deploy --env demo --manifest artifacts/package/<run-id>/PlatformImagePackageManifest.json
```

Connected validation rereads the version lock, Trivy database identity, and every Harbor digest before Helm rollout. Deployment uses the existing chart and environment values, writes a deployment manifest bound to the cluster UID and Helm revision, and never substitutes mutable tags.

Rollback requires the exact previously verified package manifest:

```sh
export LABWEAVER_PLATFORM_ROLLBACK_MANIFEST=artifacts/package/<previous-run>/PlatformImagePackageManifest.json
cargo xtask rollback --env demo --revision <helm-revision> --yes
```

Package, connected validation, deployment, rollback, and real runtime verification are distinct evidence boundaries. A static manifest or successful image build is not evidence that Container or KubeVirt flows work in the cluster.
