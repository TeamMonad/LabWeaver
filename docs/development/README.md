# Development Guide

## Prerequisites

- Rust stable toolchain with rustfmt and clippy;
- Git and GitHub CLI for the Scrum workflow.

## Rust verification

```sh
cargo xtask check
```

## Database migration contract

[Database Migrations](database-migrations.md) defines the planned PostgreSQL
schema, role, Outbox and controlled-Migration policy. No database runtime is
implemented yet; `cargo xtask migrate` must fail with `XTASK_NOT_IMPLEMENTED`
until its dedicated Migration Job exists.

## Service shell

Each initial service exposes only `/health/live` and `/health/ready`. Start a service with an explicit binding:

```sh
LABWEAVER_BIND_ADDR=127.0.0.1:8080 cargo run -p control-service
```

The service must fail when the binding is missing or invalid. Business API, persistence, messaging, provider, and deployment configuration remain planned.
