# LabWeaver sqlx-postgres patch

This directory contains the crates.io `sqlx-postgres` 0.8.6 package.
The MIT and Apache-2.0 licenses remain in this directory.
The registry archive SHA-256 is
`db58fcd5a53cf07c184b154801ff91347e4c30d17a3562a635ff028ad5deda46`.
It matches the original Cargo.lock entry.

Only `src/transaction.rs` changes from that archive.
The patch adapts the official SQLx [commit f1e94ec34dd5f11ad6488a3a8ac7988439a3f757](https://github.com/transact-rs/sqlx/commit/f1e94ec34dd5f11ad6488a3a8ac7988439a3f757),
merged in [PR #4394](https://github.com/transact-rs/sqlx/pull/4394).
The 0.8.6 query argument remains `&statement`.
The registry manifest and all other source files remain unchanged.

The driver claims the new transaction depth after it queues BEGIN or SAVEPOINT,
before it awaits the response.
Cancellation then queues rollback for that new level.
A response with no active transaction unwinds the claimed depth.
Normal BEGIN, commit and connection return add no round trip.
A rejected BEGIN can queue a harmless ROLLBACK and produce PostgreSQL's
"no transaction in progress" warning.

[Issue #4445](https://github.com/transact-rs/sqlx/issues/4445) reports another
rollback from the sqlx-core Transaction guard in newer SQLx versions.
The 0.8.6 core creates that guard only after the driver finishes BEGIN.
This backport keeps sqlx-core 0.8.6 and does not add the newer guard.
Failed savepoint creation can still produce a missing-savepoint error during cleanup.
The regression checks that the outer transaction stays aborted and commits no partial writes.

Run the regression with Docker available:

```sh
cargo test -p agent-service --test cancelled_begin_postgres --locked -- --nocapture
cargo test -p agent-service --test platform_image_jobs_postgres --locked
cargo test -p persistence-sqlx --locked
```

The first target uses an isolated PostgreSQL and a test-only protocol relay.
It holds a real BEGIN response until the caller or TLS/h2 handler is cancelled.
It checks the same backend PID, independent business state, nested savepoints and pool close.
HTTP identity comes from a fixture. The production TLS transport and Agent router execute the request.
This test does not identify the cause of a historical CI failure or a production incident.

Remove this path override only after an official release passes these regressions.
Reverting the override restores the cancelled-BEGIN defect.
