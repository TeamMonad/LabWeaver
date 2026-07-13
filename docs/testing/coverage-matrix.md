# Coverage Matrix

| Path | Positive | Negative | Current evidence | Required next evidence |
| --- | --- | --- | --- | --- |
| Service process | listener starts with explicit address | missing/invalid address fails | E1: `cargo xtask check`; Control service smoke | E2 process integration |
| Health contract | live/ready return versioned response | unknown route returns stable diagnostic | E1: unit tests and Control service smoke | E2 HTTP integration for all services |
| Request correlation | response contains request ID | malformed external ID policy not yet frozen | E1 unit | Contract decision and negative tests |
| Telemetry | JSON subscriber initializes | invalid filter fails | E1 build and code review | captured structured-log tests |
| EvaluationSpec v1alpha1 | OJ/Linux examples pass generated Schema and semantic validation | duplicate/missing/cyclic dependencies, unsafe paths, scoring boundary and tool allowlist are rejected | E1 contract tests in current worktree | Runner contract suite and E2 Evaluation Service integration |
| GitHub governance | Milestones/Labels/Issues/branch rules and 15 P0 Ready items read back | insufficient scope was diagnosed and resolved in Issue #20 | E0 metadata | human review and continued Project maintenance |
| KubeVirt/Access/Evaluation | none | none | planned/blocked | E3/E4 evidence required by release gates |

## Infrastructure automation

| Capability | Owner | Entry point | Target evidence | Blocking condition |
| --- | --- | --- | --- | --- |
| Cluster preflight | A/D | `python tools/ansible.py preflight` | E1 then E3 replay | unresolved private config or network prerequisite |
| Kubernetes baseline | A | `python tools/ansible.py deploy` | E3 | package/version/bootstrap failure |
| Storage | A/D | deploy + verify | E3 RWO/RWX | unsafe formatting or mount/provision failure |
| KubeVirt/CDI | A/B/D | deploy + verify | E3 real VM lifecycle | missing KVM/CDI/scratch class |
| Gateway and policy | A/D | deploy + verify | E3 internal route | VIP not programmed or controller unavailable |
| Backup | A | `python tools/ansible.py backup` | E2 snapshot status | snapshot integrity failure |
