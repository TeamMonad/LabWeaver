# Development Guide

## Planned Rust workflow (pending PR #21)

The current branch does not contain the Rust workspace, `Makefile`, service crates, or service processes. Rust stable with rustfmt and clippy, GNU Make, and the commands below are requirements for PR #21 after it merges; they are not current checkout prerequisites or runnable entrypoints.

## Rust verification

Planned only; validate after PR #21 merges:

```sh
make check
```

## Service shell

Planned only; PR #21 introduces service shells with `/health/live` and `/health/ready`. After it merges, start a service with an explicit binding:

```sh
LABWEAVER_BIND_ADDR=127.0.0.1:8080 cargo run -p control-service
```

The missing/invalid binding failure behavior is pending PR #21 validation on the merged target commit. Business API, persistence, messaging, provider, and deployment configuration remain planned.
