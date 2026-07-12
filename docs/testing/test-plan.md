# Test Plan

## API-01a gates (planned, pending PR #21)

The current branch does not contain the workspace, service crates, or `Makefile` required by these Cargo checks. They are PR #21 validation commands and must be rerun from its merged target commit:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked
```

`git diff --check` remains applicable to the current documentation change. Liveness, readiness, correlation headers, a stable not-found diagnostic, and fail-fast binding configuration are planned capabilities pending PR #21; they are not current-branch test evidence. Database, JetStream, OIDC, KubeVirt, MinIO, authorization, and business behavior also remain unproven.

## Governance verification

Read back Milestones, Labels, branch protection, Sprint parents, sub-issues and Project fields through GitHub APIs. The verified governance result is 20 Project items and 15 P0 items with `Workflow Status=Ready`, plus Owner, Review Role, Sprint, Area, Codex Mode, Risk, SP and Evidence metadata.
