# User Stories (3C)

The stories below retain the v2.1 product baseline. Their confirmations are
requirements, not claims that the feature is already implemented. `P0` is a
current release commitment; `P1` and `P2` do not enter the current Release
Gate.

| ID | Priority | Card | Conversation | Confirmation |
| --- | --- | --- | --- | --- |
| US-01 | P0 | As a teacher, I want an Agent to propose an environment from teaching material so that I reduce manual setup. | How are missing versions, dangerous dependencies, and Container/VM selection handled? | Candidate lists assumptions, YAML, dependencies, and risks; schema, policy, and smoke checks pass; teacher approval is required before publication. |
| US-02 | P0 | As a student, I want a consistent environment so that local differences do not block learning. | How are data retained and environment lifetime controlled? | Same template revision has identical declared key versions; stops retain only allowed PVC state; experiment completion follows cleanup policy. |
| US-03 | P0 | As a teacher, I want an Agent to propose unified evaluation so that manual marking is reduced. | How can OJ and Linux experiments share a model without allowing score manipulation? | EvaluationSpec validates; deterministic steps provide evidence; LLM output is advisory only and cannot change numeric results. |
| US-04 | P0 | As an OJ teacher, I want tests and a Checker proposal so that authoring is safer and cheaper. | How are incorrect reference solutions and weak data detected? | Reference and brute-force Oracle agree; fixed-seed mutations are killed; teacher approves the package. |
| US-05 | P0 | As a systems-lab teacher, I want VM state checked without logging in to every VM. | Does a check need root and may it modify the system? | Probe is read-only by default; elevated checks are declared separately; each assertion has command/fact evidence conforming to its schema. |
| US-06 | P0 | As a research user, I want to request software installation in natural language so that I can start work quickly. | Are arbitrary repositories, root, and durable changes allowed? | Only Work environments accept the request; BuildKit/Ansible diff and policy result are visible; high-risk changes require approval; the result can be rebuilt or rolled back. |
| US-07 | P0 / P1 | As a research user, I want to request CPU/GPU and view status so that I can schedule work. | Who approves, what is reclaimable, and when is capacity returned? | P0: approval, quota, queue, lease, Mock capacity allocation, and expiry cleanup are visible. P1: low-priority GPU preemption is outside the current gate. |
| US-08 | P0 | As an administrator, I want one controlled deployment and upgrade path so that operations remain repeatable. | How are dependency differences and rollback handled? | Ansible preflight, idempotent roles, fixed Helm versions, verify, upgrade, rollback, and destroy procedures are available and evidenced. |
| US-09 | P0 / P1 | As an external user, I want secure access to my assigned environment without public ports. | How do identity, device, course permission, and ownership combine? | P0: Keycloak/OIDC, Headscale/Tailscale reachability, and AccessGrant authorization are all required; expiry revokes new access. P1: project-specific policy generation is outside the current gate. |
| US-10 | P0 / P1 | As a test and demonstration operator, I want one command to replay the supported flow so that live demonstrations are repeatable. | How are differences and intermittent failures diagnosed? | P0: fixed seed, fixtures, role states, golden paths, traces, screenshots, videos, and `cargo xtask demo replay` are tied together. P1: cross-browser visual regression is outside the current gate. |

## Dependencies and non-goals

| Story group | Required dependency | Explicit non-goal |
| --- | --- | --- |
| US-01 to US-05 | Approved schema/contract, deterministic validator, immutable artifact identity, teacher approval. | LLM as correctness Oracle or direct score writer. |
| US-02, US-06, US-09 | Environment lifecycle, Lease/AccessGrant authority, explicit provider binding, Headscale/Tailscale policy. | Public environment endpoints, implicit provider fallback, or arbitrary shell execution. |
| US-07 | Resource approval, capacity provider binding, Lease lifecycle. | P0 real GPU/cloud expansion, P1 preemption, or unbounded reservation. |
| US-08 | Controlled Ansible entrypoint, fixed component versions, verified migration/policy behavior. | Ad-hoc administrator scripts or startup-time repair of unknown schema. |
| US-10 | Playwright, supported role fixtures, deployable target build, retained artifacts. | A separate demo-only flow, fixed waiting sleeps, or fixture evidence represented as real VM proof. |

P2 production-evolution capabilities (for example multi-cluster operation,
real GPU passthrough, and cross-region active-active deployment) remain outside
these ten confirmations and outside the current release commitment.
