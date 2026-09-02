# VM-01a Local Preflight Evidence — 2026-07-12

## Run metadata

- Source commit under test: `27c457b0ca9ee246a5bb79cd991fe32ddedb3804`
- Evidence time: `2026-07-12T08:12:41.6909117Z`
- Operator: D=@Nova-Lciop-J
- Context: `<not-set>`
- Namespace: `<all-namespaces>` read-only scope; no test namespace was provided
- Evidence level: E1 only; no E2 or E3 claimed.
- Raw log: `docs/testing/evidence/local/vm-01a-preflight-20260712T081241Z.raw.log` (gitignored)

## Local tools

| Check | Status | Sanitized result |
| --- | --- | --- |
| `kubectl version --client` | PASS | Client v1.34.1; Kustomize v5.7.1 |
| `kubectl config current-context` | BLOCKED | `error: current-context is not set` |
| `virtctl version` | BLOCKED | `virtctl` is not available on PATH |
| `helm version` | BLOCKED | `helm` is not available on PATH |

## Read-only preflight summary

| Area | Status | Sanitized result |
| --- | --- | --- |
| Kubernetes API | BLOCKED | Local prerequisites are incomplete; no cluster query was run by the script |
| Nodes | NOT VERIFIED | No current context |
| KVM capacity and allocatable | NOT VERIFIED | No current context |
| KubeVirt | NOT VERIFIED | No current context |
| CDI | NOT VERIFIED | No current context |
| StorageClass | NOT VERIFIED | No current context |
| Ingress | NOT VERIFIED | No current context |
| Headscale | NOT VERIFIED | No current context |

## Verification

- PowerShell AST parse: PASS (0 errors)
- Pester: PASS (9/9)
- Script exit code: `2` (BLOCKED)
- Command log rendering: PASS
  - `Command: kubectl version --client`
  - `Command: kubectl config current-context`
  - No `Command: kubectl System.String[]` entry was present.
- Cluster query sections in this raw log: `0`
- Raw log ignored by Git: PASS

## Blocked

- Failing prerequisite: No current kube context; compatible `virtctl` and `helm` are unavailable; no approved test cluster, test namespace, VM, image, StorageClass, network, cleanup parameters, minimum permissions, or VM lifecycle write authorization were provided.
- Original diagnostic: `error: current-context is not set`
- Evidence time: `2026-07-12T08:12:41.6909117Z`
- Current Git commit: `27c457b0ca9ee246a5bb79cd991fe32ddedb3804`
- Unblock owner: D=@Nova-Lciop-J for local tools and kubeconfig; B=@zeyi2 for the test cluster, VM, image, KubeVirt, storage, network, cleanup, and minimum-permission parameters; A=@2018wzh for repository, scope, or permission decisions.
- Exit condition: Configure an approved test context, make compatible local tools available, provide every gated VM parameter, and obtain explicit authorization before any VM lifecycle write operation.
- What was verified locally: Source commit identity, PowerShell syntax, Pester safety contract, fail-fast exit code, command rendering, raw-log ignore behavior, and the absence of cluster queries after prerequisite failure.
- What was not verified: Kubernetes API reachability, Nodes, KVM capacity or allocatable resources, KubeVirt, CDI, StorageClass, Ingress, Headscale, and VM Start-Stop-Start lifecycle.
- Evidence level: E1 only; no E2 or E3 claimed.
