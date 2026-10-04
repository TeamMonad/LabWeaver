# Platform images

Administrators can import OCI image archives and virtual-machine disk archives up to
5,000,000,000 bytes (decimal 5 GB). The application uploads the archive to the configured
object store, then completion returns HTTP `202` with a durable status. Poll
`GET /api/v1/admin/images/uploads/{uploadId}` for the current state and result; cancellation
uses `POST /api/v1/admin/images/uploads/{uploadId}/cancel` with the returned revision.
An accepted cancellation remains `cancelling` while Agent work or the first upload is unresolved.
Refresh and retry reuse the same upload and Agent import job.
The signed URL lifetime also bounds acceptance of completion. An abandoned upload
that never completes becomes `failed` with `LW_PLATFORM_IMAGE_UPLOAD_EXPIRED`; choose
the archive again to start a new upload. Already accepted imports continue after the URL expires.

Control freezes the archive's exact object version before enqueueing Agent work. The
Control credential needs `s3:ListBucketVersions` restricted to the configured bucket and
upload prefix, alongside its existing upload, read, and version-delete permissions.
After a terminal import's signed upload URL expires, Control enumerates versions only
under that upload's complete owned key and filters each result for exact key equality.
It records versions, including overwritten versions and delete markers, in its cleanup
ledger. Deletion retries use exact versions; temporary object-store failures remain
retryable. Terminal sessions are checked again every five minutes because a PUT that
started before URL expiry may finish later. This also covers signed URLs issued before
a service restart.

Configure both Control and Agent object-store limits to accept at least 5,000,000,000
bytes. Agent bounds expanded archive content to 8 GiB and streams archives, extracted
blobs, and VM containerdisk construction through temporary files. Budget 32 GiB for
Agent temporary storage during a large import, including the staged archive, extracted
content, and containerdisk layer. The deployment uses an 8 GiB ephemeral-storage request
and a 32 GiB limit with a 32 GiB temporary volume; the storage limits are separate from
the archive's public size limit.

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
and digest-pinned base images. Each component is built once with a reproducible
timestamp and the resulting `linux/amd64` image digest is recorded. A build
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
  --package-manifest artifacts/package/<run-id>/PlatformImagePackageManifest.json
```

Connected validation rechecks the component lock and the digest currently
served by Harbor. Deployment passes the immutable references to Helm and writes
the deployment manifest with the cluster UID and Helm revision.

The package and deployment commands do not replace runtime checks. Container,
KubeVirt, GPU, and external identity behavior still require their respective
runtime environments.
