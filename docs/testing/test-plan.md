# Test Plan

## API-01a gates

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked
```

The current test surface covers liveness, readiness, correlation headers, a stable not-found diagnostic and fail-fast binding configuration. It does not prove database, JetStream, OIDC, KubeVirt, MinIO, authorization or business behavior.

## Governance verification

Read back Milestones, Labels, branch protection, Sprint parents, sub-issues and Project fields through GitHub APIs. The verified governance result is 20 Project items and 15 P0 items with `Workflow Status=Ready`, plus Owner, Review Role, Sprint, Area, Codex Mode, Risk, SP and Evidence metadata.

## Infrastructure automation

| Layer | Required evidence | Failure condition |
| --- | --- | --- |
| Static | `ansible-lint`, syntax check, YAML parsing | invalid task, unresolved template, unpinned component |
| Preflight | private inventory, Vault, interfaces, SELinux, KVM, NFS reachability | any missing prerequisite blocks deploy |
| Idempotency | two consecutive `ansible-deploy` runs | second run has an unexpected change or error |
| Storage | Local Path RWO and NFS RWX cross-worker write/read | PVC is not Bound or data is not shared |
| Runtime | KubeVirt VM start, console, stop/start, cleanup | no hardware KVM-backed Running VMI |
| Network | Cilium connectivity and internal Gateway route | failed suite or unprogrammed Gateway |
| Recovery | etcd snapshot plus `etcdutl snapshot status` | snapshot cannot be validated |
