# Sprint 2 Release Gate

`cargo xtask release-gate` is the only command that can produce a passing
Sprint 2 report. It does not run a Fixture and it does not upgrade partial
evidence. `cargo xtask demo replay` first runs the allowlisted infrastructure
Verify and live Playwright command, then invokes the same gate.

## Inputs

The deployment controller writes a private, ignored JSON input conforming to
`schemas/results/sprint2-release-gate-input.v1.schema.json` and exports its
project-relative locator:

```sh
export LABWEAVER_RELEASE_GATE_INPUT=artifacts/release-gate/input.json
export LABWEAVER_DEMO_ENV=demo
cargo xtask demo replay
```

The input must bind:

- the current clean Git commit and one Run ID;
- the hashed platform deployment manifest;
- the checked-in `migrations/catalog.yaml` and its hash;
- all six immutable Harbor platform image references;
- Container and KubeVirt runtime artifact digests;
- the exact ten connected checks required by the input Schema.

Every check repeats the same commit and Run ID and points to a project-relative,
non-symlink evidence file with a SHA-256 digest. The gate rereads and hashes each
file. Missing, changed, Fixture, local-only, failed or cross-identity evidence
blocks without writing a passing report.

## Output

On success the gate writes exactly one ignored report at
`artifacts/release-gate/<run-id>.json` and validates it against
`schemas/results/release-gate-report.schema.json`. The report is evidence for
the bound deployment only; changing source, migration catalog, image set,
runtime artifact or any referenced evidence requires a new Run ID and replay.
