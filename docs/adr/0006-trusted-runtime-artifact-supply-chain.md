# ADR 0006: Trusted Runtime Artifact Supply Chain

Status: proposed Issue #52 extension; requires A+B human approval, D Verify and
connected E3 evidence.

## Context

A mutable tag, scanner summary or local path cannot prove what Container or VM a release executes. Publication needs a closed identity chain from approved candidate through build inputs, artifact, SBOM, provenance, vulnerability database and signing trust.

## Decision

`BuildRequest` binds the exact approved candidate hash, immutable context object version, fixed base digest, explicit BuildKit binding, restricted network policy, resource limits, timeout and approval identity. No builder or base-image fallback is permitted.

Container artifacts bind an OCI `sha256:` digest. VM artifacts bind an immutable base-disk locator, object version and SHA-256. Both require non-empty SBOM, provenance, certificate/signature, Rekor inclusion proof and CT/SCT evidence. `ImagePolicyEvaluation` binds policy revision, scanner version, vulnerability database digest, trust bundle, evaluation time and maximum evidence age.

Critical findings block publication. High findings remain explicit warnings for the policy owner; they are never silently downgraded. Missing, stale or mismatched evidence fails closed. A release binds exactly one runtime kind. Withdrawal is append-only; rollback publishes a new monotonically increasing release that references an older still-verified artifact.

## Consequences and rollback

Runtime implementations must revalidate expired evidence and trust revisions before use and must not reconstruct identity from tags or filenames. Issue #52 adds the v2 Control-to-Agent build command, Agent-owned durable build pipeline, authoritative artifact projection and Environment Container Provider described in `docs/contracts/container-supply-chain-v2.md`. The provider seams are wired into production service processes, but the deployment-owned BuildKit/Harbor/Trivy/Private Sigstore and Kubernetes executors still require connected E3 replay.

Before any runtime publication, rollback is whole-PR reversion. After a v1 release exists, records are immutable; remediation is withdrawal plus a new release, never mutation or subject reinterpretation.
