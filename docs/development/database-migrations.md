# Sprint 2 Database Baseline

## Current contract

Sprint 2 is a pre-release destructive reset. The six service/data ownership
boundaries remain fixed, but each domain now has exactly one current migration:

```text
migrations/<domain>/0001_sprint2_baseline.sql
```

The domains are `control`, `access`, `environment`, `agent`, `evaluation`, and
`resource`. Evaluation and Resource retain schema ownership even though their
services are disabled in the Sprint 2 deployment profile. There is no supported
upgrade, compatibility, backfill, down-migration, or recovery path for the old
pre-release schema history.

`migrations/catalog.yaml` is the checked-in source of migration identity. Its
domain order, filename and SHA-256 values are deterministic. After an approved
baseline change, update the catalog hashes and verify them with:

```sh
cargo test -p xtask migration_catalog
```

Changing a baseline requires the same A+B review as any Migration change and a
new clean reset. Editing an already deployed baseline without resetting the
target is a blocking identity mismatch.

## Ownership and execution

Bootstrap creates the six schemas and their existing owner/runtime role
boundaries. A service may read and write only its own domain. Cross-domain
foreign keys, triggers, direct writes and shared business tables remain
forbidden.

The only Sprint 2 deployment execution path is the allowlisted Ansible reset:

```sh
cargo xtask demo reset --infra --env <environment> --yes
```

The playbook requires a root-controlled `PGSERVICEFILE`; database URLs and
passwords are not command-line arguments or report fields. It binds destructive
confirmation to the exact cluster UID and Run ID, drops/recreates the six
schemas, and applies all six baseline files with `ON_ERROR_STOP`. A missing
role, failed statement or unavailable database stops the run before product
deployment and no passing reset report is written.

## Runtime and Release Gate

Services never apply or repair migrations at startup. Readiness fails when the
expected schema or migration identity is absent or mismatched. The Sprint 2
deployment manifest and Release Gate both bind the SHA-256 of
`migrations/catalog.yaml`; reports from another catalog, commit or Run ID are
invalid.

Future post-v1 data evolution must introduce a new ADR and forward migrations.
It must not restore the deleted pre-release v1/v2 compatibility machinery or
reinterpret this destructive baseline as production upgrade evidence.
