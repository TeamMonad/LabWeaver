# LabWeaver jwt-authorizer patch

This directory contains the published `jwt-authorizer` 0.15.0 source under its
MIT license. LabWeaver changes only JWKS transport ownership: the configured
`reqwest::Client` is retained by `KeyStoreManager` and reused for every
unknown-`kid` refresh. Upstream 0.15.0 otherwise creates a new default client
inside the refresh path, which loses the deployment-provided private CA.
The patch also changes unknown-`kid` refresh throttling to the configured retry
interval instead of the periodic refresh interval. The manager's mutex merges
concurrent misses, while the retry interval prevents a stream of attacker-
controlled key IDs from forcing an unbounded JWKS request rate.

The behavioral gate is the ignored real-Keycloak test in
`crates/auth/tests/keycloak_discovery.rs`: it rotates signing keys twice and
requires a successful custom-CA refresh followed by fail-closed behavior while
the realm is unavailable.
