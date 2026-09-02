# Identity foundation verifier observation — 2026-07-15

This record is a read-only observation made with the time-limited
`devops-verifier` kubeconfig supplied out of band. The kubeconfig, its token,
cluster endpoint, Secret data, and inventory values were not copied, printed,
or stored in this repository.

## Identity

| Field | Value |
| --- | --- |
| Source commit | `0a1309d6f0a719a7efa8edc5efb0f78b3570e0a5` |
| Controller | local Ubuntu 24.04 WSL observation client |
| Client | `kubectl` v1.34.1 (official release checksum verified) |
| Scope | adopted-cluster identity foundation, read-only |

## Observed checks

| Check | Result |
| --- | --- |
| Kubernetes API readiness | pass |
| Node inventory query | pass (three nodes observed) |
| Identity namespace and deployment query | pass (one deployment observed) |
| Keycloak deployment rollout | pass |
| Identity Gateway `Programmed` condition | pass |
| Internal root CA Certificate `Ready` condition | pass |
| Internal TLS Certificate `Ready` condition | pass |
| Gateway and Certificate read authorization | pass |
| Secret read authorization | denied/unavailable as required for verifier identity |
| RoleBinding mutation authorization | denied/unavailable as required for verifier identity |

## Boundary and conclusion

No Kubernetes mutation, Secret read, RBAC change, DNS change, credential
exchange, or reconcile was attempted. The verifier identity can support the
read-only portion of D verification, but it cannot execute the approved
controller replay or the Secret-backed OIDC token and DNS checks. Those remain
pending under the approved controller identity.

This is identity-foundation baseline evidence only. It is not a completion of
Private Sigstore E3, backup/restore, root or signer rotation, or Release Gate.
