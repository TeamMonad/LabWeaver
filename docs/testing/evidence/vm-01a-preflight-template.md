# VM-01a KubeVirt and Storage Preflight Evidence Template

Use this template for a single preflight run. Raw command output belongs in the
gitignored `docs/testing/evidence/local/` directory and must never be committed.
Replace every placeholder from one run only; do not combine multiple executions.

## Run metadata

- Source commit under test: `<full-git-sha>`
- Evidence time: `<ISO-8601-with-timezone>`
- Operator: `<github-handle>`
- Context: `<redacted-context-or-not-set>`
- Namespace: `<namespace-or-all-namespaces>`
- Evidence level: `<E1-or-E2;-never-claim-E3-without-authorized-lifecycle-evidence>`
- Raw log: `docs/testing/evidence/local/<raw-log-filename>` (gitignored)

## Local tools

| Check | Status | Sanitized result |
| --- | --- | --- |
| `kubectl version --client` | `<PASS/FAIL/BLOCKED>` | `<version-or-diagnostic>` |
| `kubectl config current-context` | `<PASS/FAIL/BLOCKED>` | `<redacted-context-or-diagnostic>` |
| `kubectl config get-contexts` | `<PASS/FAIL/BLOCKED>` | `<sanitized-summary>` |
| `virtctl version` | `<PASS/FAIL/BLOCKED>` | `<version-or-diagnostic>` |
| `helm version` | `<PASS/FAIL/BLOCKED>` | `<version-or-diagnostic>` |

## Read-only preflight summary

| Area | Status | Sanitized result |
| --- | --- | --- |
| Kubernetes API | `<PASS/FAIL/BLOCKED/NOT VERIFIED>` | `<summary>` |
| Nodes | `<PASS/FAIL/BLOCKED/NOT VERIFIED>` | `<count-and-ready-summary>` |
| KVM capacity and allocatable | `<PASS/FAIL/BLOCKED/NOT VERIFIED>` | `<non-zero-node-summary>` |
| KubeVirt | `<PASS/FAIL/BLOCKED/NOT VERIFIED>` | `<CRD-CR-components-summary>` |
| CDI | `<PASS/FAIL/BLOCKED/NOT VERIFIED>` | `<summary>` |
| StorageClass | `<PASS/FAIL/BLOCKED/NOT VERIFIED>` | `<summary>` |
| Ingress | `<PASS/FAIL/BLOCKED/NOT VERIFIED>` | `<summary>` |
| Headscale | `<PASS/FAIL/BLOCKED/NOT VERIFIED>` | `<summary>` |

## StorageClass summary

Retain only these fields from real read-only cluster output:

| Name | Provisioner | Default | VolumeBindingMode | AllowVolumeExpansion | ReclaimPolicy |
| --- | --- | --- | --- | --- | --- |
| `<name>` | `<provisioner>` | `<true/false>` | `<mode>` | `<true/false>` | `<policy>` |

## Verification

- PowerShell AST parse: `<PASS/FAIL>`
- Pester: `<passed>/<total>`
- Script exit code: `<integer>`
- Command log rendering: `<PASS/FAIL>`
- Raw log ignored by Git: `<PASS/FAIL>`

## Blocked

- Failing prerequisite: `<prerequisite>`
- Original diagnostic: `<sanitized-verbatim-diagnostic>`
- Evidence time: `<ISO-8601-with-timezone>`
- Current Git commit: `<full-git-sha>`
- Unblock owner: `<owner>`
- Exit condition: `<objective-exit-condition>`
- What was verified locally: `<verified-items>`
- What was not verified: `<unverified-items>`
- Evidence level: E1/E2 only; no E3 claimed.

## VM lifecycle gate

Do not run lifecycle commands until every field is provided and the operator has
received explicit approval for real VM lifecycle testing.

| Required input | Value |
| --- | --- |
| Test cluster | `<required>` |
| Kube context | `<required>` |
| Test namespace | `<required>` |
| VM name | `<required>` |
| Approved VM manifest or existing VM | `<required>` |
| Image source | `<required>` |
| StorageClass | `<required>` |
| Network mode | `<required>` |
| Cleanup method | `<required>` |
| Minimum permissions | `<required>` |
| Explicit write authorization | `<required>` |

## E3 lifecycle evidence

Leave this section unclaimed until an authorized real test completes.

- Evidence time: `<required>`
- Git commit: `<required>`
- Redacted cluster identifier: `<required>`
- Namespace: `<required>`
- VM name: `<required>`
- Start phase and Ready condition: `<required>`
- Stop phase and VMI deletion: `<required>`
- Second start phase and Ready condition: `<required>`
- Event summary: `<required>`
- Original sanitized failure diagnostic: `<required-if-failed>`
- Operator: `<required>`
- Final cleanup state: `<required>`
- Evidence level: `<E3 only after all fields are complete>`
