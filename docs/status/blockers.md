# Active Blockers

## GitHub Project write scope

- Diagnostic: `updateProjectV2Field` requires the `project` scope; the active credential has only `read:project` for Project access.
- Impact: Project fields cannot be configured and P0 items cannot be proven Ready in the Project.
- Owner: A.
- Exit condition: grant write `project` scope, configure and read back all fields, add Sprint Issues and verify at least 12 P0 items have `Status=Ready`.
- Tracking: GitHub Issue #20.

## Cross-role Day 1 gate

- Frontend build and B/C/D first branches or PRs are not role A deliverables.
- Owner: B, C and D respectively.
- Exit condition: each owner supplies its own reviewed PR and required evidence.

