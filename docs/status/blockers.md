# Active Blockers

This file records active release and acceptance blockers. A local or CI-only
result does not close a connected blocker.

## Issue #140 C++17 OJ execution

- Issue #123 must provide the authoritative EvaluationRun, StepRun, attempt,
  Outbox, persistence and immutable artifact integration.
- The dedicated runner image must build reproducibly and pass the digest-bound
  Critical/secret scan in CI.
- D must run the same source/image identity on real Kubernetes and verify
  compile error, accepted, wrong answer, time, memory and output limits,
  default-deny egress, cancel/retry, exact cleanup and private-payload absence.
- A+B human review is required for Runner, Checker, Aggregator and scoring;
  auto-merge is forbidden.

These blockers keep Issue #140 below E3 and outside the Release Gate.

## ConsoleCapability downstream delivery

ADR 0012 and Issue #122 freeze the xterm/noVNC Contract at E2 only. The
following remain release blockers and cannot be represented by generated
schemas, Fixture, historical PR #138 or mixed-source demonstrations:

- #131 must be reopened and reimplemented from the merged ConsoleCapability
  contract; the closed PR #138 is design reference only.
- #124 must implement the Access proxy, Environment mTLS bridge and
  least-privilege KubeVirt VMI `/vnc` stream without guest VNC passwords or
  public websockify.
- #126 must demonstrate same-identity shared-cluster multi-role E4, negative
  revocation/expiry/control-channel-loss behavior and Release Gate evidence.

## Human review and acceptance

The connected technical gate passed at source
`748c2470ad0f3fba848761f0113853a6870576d6`, Release Gate Run
`a2835d47-7f9b-48a3-b8a0-60d22f57d5e2`. Access/Gateway negatives,
dual-runtime lifecycle, real Keycloak Playwright, non-destructive application
idempotence, rollback and cleanup readback are closed for that identity.

PR #121 remains `risk:high` and Draft. It must not use auto-merge. The
remaining blockers are human governance gates:

- B `@zeyi2`: core implementation and security approval;
- C `@yingxvemiao`: Web review;
- D `@Nova-Lciop-J`: connected deployment/Release Gate Verify;
- all review threads resolved and final required CI checks green.

Owner A `@2018wzh` remains author and acceptance owner and cannot replace
those reviews or self-approve the merge.

## Explicit non-blocking limitation

The full `cargo xtask demo replay` wrapper was not run after the direct
connected checks because it repeats infrastructure Verify and live Playwright.
The fail-closed `cargo xtask release-gate` command itself passed and produced
the unique schema-valid report. This limitation must remain visible in the PR
and acceptance record; D may require the wrapper replay during Verify.
