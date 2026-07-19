# Sprint 2 Database Baseline

## Current contract

Sprint 2 adopts the retained infrastructure without destructive reset. The six service/data ownership
boundaries remain fixed, but each domain now has exactly one current migration:

```text
migrations/<domain>/0001_sprint2_baseline.sql
```

The domains are `control`, `access`, `environment`, `agent`, `evaluation`, and
`resource`. Evaluation and Resource retain schema ownership even though their
services are disabled in the Sprint 2 deployment profile. There is no supported
compatibility, backfill, or down-migration path for an older populated
pre-release schema. Such a schema is an explicit blocker requiring a separately
reviewed forward migration; it is never dropped implicitly.

`migrations/catalog.yaml` is the checked-in source of migration identity. Its
domain order, filename and SHA-256 values are deterministic. After an approved
baseline change, update the catalog hashes and verify them with:

```sh
cargo test -p xtask migration_catalog
```

Changing a baseline requires the same A+B review as any Migration change.
Editing an already applied baseline is a blocking identity mismatch.

## Ownership and execution

Bootstrap creates the six schemas and their existing owner/runtime role
boundaries. A service may read and write only its own domain. Cross-domain
foreign keys, triggers, direct writes and shared business tables remain
forbidden.

The Sprint 2 adoption path creates a domain schema and applies its baseline only
when both the schema and its migration ledger are absent. It accepts an existing
domain only when the recorded filename and SHA-256 exactly match the checked-in
catalog:

```sh
cargo xtask deploy --env <environment> --package-manifest <verified-manifest> --yes
```

The playbook requires a root-controlled `PGSERVICEFILE`; database URLs and
passwords are not command-line arguments or report fields. It binds the run
identity to the exact cluster UID and migration catalog. It never drops or
recreates a schema. A partial schema, unknown table, missing role, failed
statement, unavailable database, or catalog mismatch stops the run before
product deployment and no passing deployment report is written.

## Runtime and Release Gate

Services never apply or repair migrations at startup. Readiness fails when the
expected schema or migration identity is absent or mismatched. The Sprint 2
deployment manifest and Release Gate both bind the SHA-256 of
`migrations/catalog.yaml`; reports from another catalog, commit or Run ID are
invalid.

Future post-v1 data evolution must introduce a new ADR and forward migrations.
It must not restore the deleted pre-release v1/v2 compatibility machinery or
reinterpret first-install baseline evidence as production upgrade evidence.
