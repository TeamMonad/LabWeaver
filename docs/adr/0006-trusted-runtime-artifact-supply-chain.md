# ADR 0006: Trusted Runtime Artifact Supply Chain

Status: Superseded by ADR 0011

The Private Sigstore, transparency, attestation, and Kyverno admission design
below is retained only as historical context. It is not part of the current
Sprint 2 product or deployment contract. ADR 0011 defines the active
Harbor/digest/Trivy policy.

The remainder of this file records the superseded proposal and must not be used
as an active implementation or deployment requirement.

## Context

A mutable tag, scanner summary or local path cannot prove what Container or VM a release executes. Publication needs a closed identity chain from approved candidate through build inputs, artifact, SBOM, provenance, vulnerability database and signing trust.

## Decision

`BuildRequest` binds the exact approved candidate hash, immutable context object version, fixed base digest, explicit BuildKit binding, restricted network policy, resource limits, timeout and approval identity. No builder or base-image fallback is permitted.

Container artifacts bind an OCI `sha256:` digest. VM artifacts bind an immutable base-disk locator, object version and SHA-256. Both require non-empty SBOM, provenance, certificate/signature, a signature subject digest equal to the artifact digest, Rekor inclusion proof and CT/SCT evidence. `ImagePolicyEvaluation` binds policy revision, scanner version, vulnerability database digest, trust bundle, evaluation time and maximum evidence age.

Critical findings block publication. High findings remain explicit warnings for the policy owner; they are never silently downgraded. Missing, stale or mismatched evidence fails closed. A release binds exactly one runtime kind. Withdrawal is append-only; rollback publishes a new monotonically increasing release that references an older still-verified artifact.

## Consequences and rollback

This historical decision was replaced before release. The current v1 contract
is documented in `docs/contracts/container-supply-chain-v1.md`; it retains
Harbor digest identity and Trivy policy while removing the private trust plane.

Remote executor calls carry the database attempt or Environment operation generation together with an exact stage/step request ID and deadline. Executors persist the highest accepted generation and cleanup/delete tombstones; an older completion cannot recreate or remove resources owned by a newer generation.

Before any runtime publication, rollback is whole-PR reversion. After a v1 release exists, records are immutable; remediation is withdrawal plus a new release, never mutation or subject reinterpretation.
