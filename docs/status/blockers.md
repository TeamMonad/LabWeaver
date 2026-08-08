# Active Blockers

This file records active release and acceptance blockers. A local or CI-only
result does not close a connected blocker.

## Issue #140 C++17 OJ execution

- Issue #123's local control-plane implementation must be reviewed, merged,
  deployed and connected-verified before #140 can claim a production
  EvaluationRun or StepRun path.
- The dedicated runner image must build reproducibly and pass the digest-bound
  High/Critical/secret scan in CI; its Trivy JSON must remain attached to the
  same workflow identity.
- D must run the same source/image identity on real Kubernetes and verify
  compile error, accepted, wrong answer, time, memory and output limits,
  preinstalled default-deny egress from Pod start, daemon/double-fork rejection,
  PID exhaustion, cancel/retry, preconditioned exact cleanup and private-payload
  absence.
- A+B human review is required for Runner, Checker, Aggregator and scoring;
  auto-merge is forbidden.

These blockers keep Issue #140 below E3 and outside the Release Gate.

## Issue #141 read-only Ansible Probe execution

- Issue #123 must provide the authoritative EvaluationRun, StepRun, attempt,
  Outbox, persistence and immutable artifact integration before the probe runs
  as a public Step.
- The dedicated runner image must build reproducibly and pass the digest-bound
  High/Critical/secret scan in CI; its Trivy JSON must remain attached to the
  same workflow identity.
- D must run the same source/image identity on a connected cluster with a real
  KubeVirt VM and verify the positive Nginx path plus the stopped-service and
  site-mismatch negative paths, preinstalled `ansible-probe-default-deny`,
  attempt egress limited to the VM address on TCP/22, stale-certificate,
  unreachable-host, host-key-mismatch, timeout, output-overflow and
  malformed-fact negatives, cancel/retry and preconditioned exact cleanup.
- TCP/80 and HTTP checks stay outside the frozen v1 module allowlist; widening
  requires a separate reviewed decision and must not be smuggled into this
  runner.
- A+B human review is required for Runner, security and scoring semantics;
  auto-merge is forbidden.

These blockers keep Issue #141 below E3 and outside the Release Gate.

## ConsoleCapability downstream delivery

ADR 0012 and Issue #122 freeze the xterm/noVNC Contract at E2 only. The
following remain release blockers and cannot be represented by generated
schemas, Fixture, historical PR #138 or mixed-source demonstrations:

- #131 implements the Container xterm path from the merged Sprint 3 contract;
  its evidence remains local until #126 runs the frozen connected candidate.
- #124 must implement the Access proxy, Environment mTLS bridge and
  least-privilege KubeVirt VMI `/vnc` stream without guest VNC passwords or
  public websockify.
- #126 must demonstrate same-identity shared-cluster multi-role E4, negative
  revocation/expiry/control-channel-loss behavior and Release Gate evidence.

## Issue #142 local implementation and acceptance

The candidate source identity is the full Git `HEAD` recorded by each local
report and package manifest. Native contract, Resource and local integration
checks are the only current evidence. No package, deployment, Resource replay
or Release Gate report is accepted unless it binds the same immutable `HEAD`.

PR #147 remains `risk:high` and Draft. The remaining blockers are:

- same-identity Resource package and deployment manifest;
- approved-environment Resource connected replay and real dual-runtime evidence;
- B `@zeyi2` core/security review;
- D `@Nova-Lciop-J` connected Verify;
- all review threads resolved and final required CI checks green.

Owner A `@2018wzh` remains author and acceptance owner and cannot replace
those reviews or self-approve the merge.

## Docker Desktop boundary

`cargo xtask local preflight` is read-only and records a
`local-connected-non-release` report. Docker Desktop's single-node `hostpath`
profile cannot satisfy the formal `nfs-rwx` or KubeVirt requirements, so its
report cannot be consumed by `cargo xtask release-gate` or close #142.
The dependency stack is represented by a schema-validated render-only overlay
and a plan-only teardown order; no local namespace, PVC, Secret, stream, bucket,
realm or workload is created or deleted in this phase.
