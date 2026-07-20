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
export LABWEAVER_KUBECONFIG="$HOME/.config/labweaver/package/kubeconfig"
cargo xtask package --env demo --release sprint2 --yes
```

Before packaging, run the non-destructive `sprint2-buildkit` adoption with
`sprint2_buildkit_controller_enabled=true`, an exact router-local kubeconfig source, and explicit
control-plane API URL and address bindings. The role installs a persistent,
loopback-only `kubectl port-forward` and the reviewed BuildKit mTLS identity on the retained edge
router. The role creates the `labweaver-package` buildx context only when it is absent, and always
requires it to resolve to `tcp://127.0.0.1:1234`; a conflicting context, missing tunnel, mismatched
mTLS binding, or stale BuildKit identity is a blocking error.

The packaging host uses the checksum-verified standalone `docker-buildx`
v0.35.0 executable. A Docker daemon is not required: configure a named Buildx
`remote` driver against the mTLS BuildKit endpoint and make it current before
running the command. This keeps build storage on the reviewed BuildKit PVC
instead of the infrastructure controller.

Before packaging, copy every digest in `platform_images.bases`, the locked
BuildKit tools image, and the locked Trivy image into the private
`labweaver-system/base-<build-arg>` repositories with `skopeo copy --all`.
The copy must preserve each source index digest. Packaging overrides every
Containerfile base argument with the corresponding Harbor digest and does not
permit BuildKit to reach a public registry; a missing mirror is therefore a
blocking error rather than a network fallback.

The command fails unless the source tree is clean, the explicit Rust toolchain
matches both digest-locked Rust builder images, BuildKit/Buildx/Trivy match
`deploy/versions.lock.yml`, the Trivy database is digest pinned, both
reproducibility builds resolve to the same `linux/amd64` manifest digest, no
secret or critical vulnerability is found, and every image is recorded as a
Harbor digest reference. A moving `stable` Rust channel is rejected so a build
cannot silently download a different compiler. When Buildx uses the
cluster-owned remote driver, `LABWEAVER_KUBECONFIG` is mandatory and packaging
also reads back the `labweaver-build/buildkit` Deployment: its rootless image
must match the locked digest, exactly one replica must be ready and updated,
and its reviewed configuration hash annotation must be present. High
vulnerabilities remain visible in the report but do not silently alter the
gate.

The package manifest records the source commit, component lock hash, builder versions, Harbor host, image digests, Trivy version/database identity, vulnerability counts, and content hash of each retained scan report. Sprint 2 intentionally does not generate or validate Sigstore, Fulcio, Rekor, CT, TUF, SBOM, provenance, or Kyverno evidence.

## Connected validation and deployment

```sh
cargo xtask package-validate --manifest artifacts/package/<run-id>/PlatformImagePackageManifest.json --mode connected --env demo
LABWEAVER_CONFIGURATION_BUNDLE_SHA256=sha256:<configuration-bundle-sha256> \
  cargo xtask deploy --env demo --manifest artifacts/package/<run-id>/PlatformImagePackageManifest.json
```

Connected validation rereads the version lock, Trivy database identity, and every Harbor digest before Helm rollout. Deployment uses the existing chart and environment values, writes a deployment manifest bound to the cluster UID and Helm revision, and never substitutes mutable tags.

Rollback requires the exact previously verified package manifest:

```sh
export LABWEAVER_PLATFORM_ROLLBACK_MANIFEST=artifacts/package/<previous-run>/PlatformImagePackageManifest.json
cargo xtask rollback --env demo --revision <helm-revision> --yes
```

Package, connected validation, deployment, rollback, and real runtime verification are distinct evidence boundaries. A static manifest or successful image build is not evidence that Container or KubeVirt flows work in the cluster.
