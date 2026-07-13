# Architecture Decision Records

ADRs record accepted cross-domain or high-risk decisions. A draft design statement is not an accepted ADR.

Each ADR must include status, context, decision, alternatives, consequences, security/data implications, compatibility, evidence and rollback or replacement conditions. Number new records sequentially as `NNNN-short-title.md`.

ADR 0001 records the proposed ACCESS-01a dual-path decision; it is not accepted until the required human reviews are complete.
No ADR is accepted in ARC-01a; this directory establishes the review process without inventing approvals.

## Proposed records

- [ADR 0003: NATS Subject and Delivery Contract](0003-nats-subject-and-delivery-contract.md)
  resolves Issue #18 at E0 design level and requires A/B review before runtime
  implementation.
