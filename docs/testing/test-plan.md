# Test Plan

## API-01a gates

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked
git diff --check
```

The initial test surface covers liveness, readiness, correlation headers, a stable not-found diagnostic and fail-fast binding configuration. It does not prove database, JetStream, OIDC, KubeVirt, MinIO, authorization or business behavior.

## Governance verification

Read back Milestones, Labels, branch protection, Sprint parents and sub-issues through GitHub APIs. Project field verification remains blocked until the credential has write `project` scope.
