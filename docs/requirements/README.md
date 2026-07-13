# Requirements Baseline

This directory is the formal, testable requirements baseline for LabWeaver. It
converts the v2.1 design baseline into reviewable requirements; it is not
runtime evidence and does not change service ownership, public contracts, or
release policy.

## Reading order

1. [Impact map](impact-map.md) identifies affected roles, outcomes,
   dependencies, and risks.
2. [User journeys](user-journeys.md) turns role flows into observable stages
   and failure signals.
3. [User stories](user-stories.md) records US-01 through US-10 using 3C.
4. [Acceptance criteria](acceptance-criteria.md) assigns stable requirement
   IDs, expected evidence, and current verification state.

## Evidence rule

The `Current evidence` column in the acceptance matrix is a fact about the
current repository state. `planned`, `blocked`, E0 documentation, and E1
contract tests are not evidence that a production path is complete. The
implementation status, test plan, and coverage matrix remain the authorities
for current capability state and required next proof.

## Scope and priorities

All ten stories are retained. P0 items are current release commitments; P1
items are explicitly out of the current release gate; P2 is reserved for the
production-evolution items in the v2.1 scope baseline. A story may have P0
and P1 acceptance criteria when its core flow is P0 but an enhancement is not.
