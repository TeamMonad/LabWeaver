# MinIO integration-test image

The artifact-store integration test uses a local MinIO image built from the
official MinIO source release. The public community container reference is
not used. The source commit and the pinned Go builder and Alpine runtime
images are recorded under `platform_foundation.minio_test` in
`deploy/versions.lock.yml`.

Prepare the image from the repository root before running the artifact-store
test:

```sh
python3 tools/prepare_minio_test_image.py
LABWEAVER_TEST_MINIO_IMAGE=labweaver/minio-test:9e49d5e7a648 \
  cargo test -p artifact-store --locked minio_versioning_object_lock_and_cleanup_are_fail_closed
```

The preparation command loads the image into the local Docker engine and
inspects it before returning. The Rust test checks that this exact local image
exists before creating a container; a missing image fails the test instead of
pulling an unreviewed registry image. This test image is only for CI and local
tests and does not change the live MinIO image lock or deployment.
