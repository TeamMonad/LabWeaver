# Active Blockers

This file contains only active blockers for Draft PR #121 on
`release/sprint2`.

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
