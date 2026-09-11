# Platform images

The image workflow builds the enabled LabWeaver workloads, publishes immutable
Harbor references, and deploys the references through the existing Helm chart.
The package manifest is `PlatformImagePackageManifest.json` and follows
`schemas/results/platform-image-package-manifest.v2.schema.json`.

The platform profile can contain the currently enabled platform components:
`control-service`, `access-service`, `agent-service`, `environment-service`,
`evaluation-service`, `openssh-gateway`, and `web`. Resource is packaged by a
separate profile. The manifest records the source commit, component lock hash,
builder versions, registry host, and each image's digest reference. It does not
record scanner output or a fixed component count.

Package the selected profile on the Linux build host:

```sh
export LABWEAVER_PLATFORM_REGISTRY=harbor.example.internal
cargo xtask package --env demo --release platform --yes
```

The command requires a clean source tree, a matching locked Rust toolchain,
and digest-pinned base images. Each component is built twice with reproducible
timestamps and the resulting `linux/amd64` image digest is recorded. A build
failure or missing digest stops the command.

Before packaging, mirror the digest-pinned base images and BuildKit image into
the private Harbor project. The package command passes those private mirrors to
BuildKit so a build cannot silently use a mutable public tag.

Validate and deploy a package with the existing Helm release:

```sh
cargo xtask package-validate \
  --manifest artifacts/package/<run-id>/PlatformImagePackageManifest.json \
  --mode static

LABWEAVER_CONFIGURATION_BUNDLE_SHA256=sha256:<configuration-bundle-sha256> \
  cargo xtask deploy --env demo \
  --manifest artifacts/package/<run-id>/PlatformImagePackageManifest.json
```

Connected validation rechecks the component lock and the digest currently
served by Harbor. Deployment passes the immutable references to Helm and writes
the deployment manifest with the cluster UID and Helm revision. Rollback uses
the package manifest selected by `LABWEAVER_PLATFORM_ROLLBACK_MANIFEST`:

```sh
export LABWEAVER_PLATFORM_ROLLBACK_MANIFEST=artifacts/package/<previous-run>/PlatformImagePackageManifest.json
cargo xtask rollback --env demo --revision <helm-revision> --yes
```

The package and deployment commands do not replace runtime checks. Container,
KubeVirt, GPU, and external identity behavior still require their respective
runtime environments.
