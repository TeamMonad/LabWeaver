# VM-01a KubeVirt and Storage Evidence Template

Use this template for a single preflight run. Raw command output belongs in the
gitignored `docs/testing/evidence/local/` directory and must never be committed.
Replace every placeholder from one run only; do not combine multiple executions.

## Run metadata

- Source commit under test: `<full-git-sha>`
- Evidence time: `<ISO-8601-with-timezone>`
- Operator: `<github-handle>`
- Context: `<redacted-context-or-not-set>`
- Namespace: `<namespace-or-all-namespaces>`
- Evidence level: `<E1/E2/E3; E3 requires every lifecycle and cleanup check>`
- Raw log: `docs/testing/evidence/local/<raw-log-filename>` (gitignored)

## Local tools

| Check | Status | Sanitized result |
| --- | --- | --- |
| Python verifier syntax and unit tests | `<PASS/FAIL>` | `<sanitized-result>` |
| Kubernetes API | `<PASS/FAIL/BLOCKED>` | `<sanitized-result>` |
| VM/PVC permission | `<PASS/FAIL/BLOCKED>` | `<sanitized-result>` |
| `virtctl` lifecycle client | `<PASS/FAIL/BLOCKED>` | `<sanitized-result>` |

## Read-only preflight summary

| Area | Status | Sanitized result |
| --- | --- | --- |
| Kubernetes API | `<PASS/FAIL/BLOCKED/NOT VERIFIED>` | `<summary>` |
| Nodes | `<PASS/FAIL/BLOCKED/NOT VERIFIED>` | `<count-and-ready-summary>` |
| KVM capacity and allocatable | `<PASS/FAIL/BLOCKED/NOT VERIFIED>` | `<non-zero-node-summary>` |
| KubeVirt | `<PASS/FAIL/BLOCKED/NOT VERIFIED>` | `<CRD-CR-components-summary>` |
| CDI | `<PASS/FAIL/BLOCKED/NOT VERIFIED>` | `<summary>` |
| StorageClass | `<PASS/FAIL/BLOCKED/NOT VERIFIED>` | `<summary>` |
| Cilium Gateway and HTTPRoute | `<PASS/FAIL/BLOCKED/NOT VERIFIED>` | `<summary>` |

## StorageClass summary

Retain only these fields from real read-only cluster output:

| Name | Provisioner | Default | VolumeBindingMode | AllowVolumeExpansion | ReclaimPolicy |
| --- | --- | --- | --- | --- | --- |
| `<name>` | `<provisioner>` | `<true/false>` | `<mode>` | `<true/false>` | `<policy>` |

## Verification

- `python -m py_compile scripts/preflight/kubevirt_preflight.py`: `<PASS/FAIL>`
- `python -m unittest discover -s tests/preflight`: `<passed>/<total>`
- Verifier exit code: `<integer>`
- Raw log/report ignored by Git: `<PASS/FAIL>`

## Blocked

- Failing prerequisite: `<prerequisite>`
- Original diagnostic: `<sanitized-verbatim-diagnostic>`
- Evidence time: `<ISO-8601-with-timezone>`
- Current Git commit: `<full-git-sha>`
- Unblock owner: `<owner>`
- Exit condition: `<objective-exit-condition>`
- What was verified locally: `<verified-items>`
- What was not verified: `<unverified-items>`
- Evidence level: `<E1/E2 only when blocked>`

## VM lifecycle gate

Do not run E3 until every field is provided and the operator has received
explicit Issue #15 authorization for real lifecycle testing.

| Required input | Value |
| --- | --- |
| Issue authorization | `#15` |
| Run ID | `<required>` |
| Test namespace | `labweaver-verify-<run-id>` only |
| VM image | `<immutable-digest>` |
| Workload image | `<immutable-digest>` |
| StorageClass | `local-path` and `nfs-rwx` |
| Network evidence | Existing Cilium Gateway/HTTPRoute request only |
| Cleanup method | Namespace deletion with wait and recorded exit code |
| Minimum permissions | Namespace, PVC, Pod, VM/VMI create/delete only |
| Explicit write authorization | `<recorded PR/Issue authorization>` |

## E3 lifecycle evidence

Leave this section unclaimed until an authorized real test completes.

- Evidence time: `<required>`
- Git commit: `<required>`
- Redacted cluster identifier: `<required>`
- Namespace: `<required>`
- VM name: `<required>`
- RWO write/read: `<required>`
- Cross-worker RWX write/read: `<required>`
- Gateway request: `<required>`
- Start phase and Ready condition: `<required>`
- Stop phase and VMI deletion: `<required>`
- Second start phase and Ready condition: `<required>`
- Event summary: `<required>`
- Original sanitized failure diagnostic: `<required-if-failed>`
- Operator: `<required>`
- Final cleanup state: `<required>`
- Evidence level: `<E3 only after all fields are complete>`
