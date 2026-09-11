#!/usr/bin/env python3
"""Render the catalog-controlled PostgreSQL migration script.

The migration catalog is the single source of truth for bootstrap and domain
SQL ordering.  This renderer is intentionally usable from both the local Kind
entry point and the Ansible application deployment: callers provide the
execution identity and the output is a psql script with the same verification,
owner-role execution, and history recording rules.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
from pathlib import Path
from typing import Any, Iterable

try:
    import yaml
except ImportError:  # pragma: no cover - reported as a renderer error below
    yaml = None  # type: ignore[assignment]


CATALOG_VERSION = 1
CATALOG_TOOL_VERSION = "0.1.0"
DOMAIN_ORDER = ("control", "access", "environment", "agent", "evaluation", "resource")
IDENTIFIER = re.compile(r"^[a-z_][a-z0-9_]*$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
MIGRATION_NAME = re.compile(r"^[0-9]+_[a-z0-9][a-z0-9_.-]*\.sql$")


class MigrationRenderError(ValueError):
    """Raised when the catalog or its SQL files cannot be rendered safely."""


def _load_yaml(path: Path) -> dict[str, Any]:
    if yaml is None:
        raise MigrationRenderError("PyYAML is required to read the migration catalog")
    try:
        value = yaml.safe_load(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, yaml.YAMLError) as error:
        raise MigrationRenderError(f"cannot read migration catalog: {path}") from error
    if not isinstance(value, dict):
        raise MigrationRenderError("migration catalog must be a mapping")
    return value


def _read_sql(path: Path, filename: str) -> tuple[bytes, str]:
    try:
        payload = path.read_bytes()
        sql = payload.decode("utf-8")
    except (OSError, UnicodeError) as error:
        raise MigrationRenderError(f"cannot read migration SQL: {path}") from error
    normalized = sql.upper()
    for forbidden in (
        "BEGIN;",
        "COMMIT;",
        "ROLLBACK;",
        "CREATE INDEX CONCURRENTLY",
        "DROP INDEX CONCURRENTLY",
        "VACUUM ",
    ):
        if forbidden in normalized:
            raise MigrationRenderError(
                f"{filename} contains unsupported non-transactional statement: {forbidden}"
            )
    return payload, sql


def _safe_relative_sql(root: Path, filename: Any) -> Path:
    if not isinstance(filename, str) or not filename or Path(filename).is_absolute():
        raise MigrationRenderError(f"unsafe migration filename: {filename!r}")
    relative = Path(filename)
    if relative.suffix != ".sql" or any(part in ("", ".", "..") for part in relative.parts):
        raise MigrationRenderError(f"unsafe migration filename: {filename!r}")
    candidate = root / relative
    if not candidate.is_file() or candidate.is_symlink():
        raise MigrationRenderError(f"migration SQL file is missing or not regular: {filename}")
    path = candidate.resolve()
    try:
        path.relative_to(root.resolve())
    except ValueError as error:
        raise MigrationRenderError(f"migration filename escapes catalog root: {filename!r}") from error
    if not path.is_file():
        raise MigrationRenderError(f"migration SQL file is missing or not regular: {filename}")
    return path


def _migration(root: Path, value: Any, *, location: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) - {"id", "file", "sha256"}:
        raise MigrationRenderError(f"invalid migration entry: {location}")
    migration_id = value.get("id")
    filename = value.get("file")
    if isinstance(migration_id, bool) or not isinstance(migration_id, int) or migration_id <= 0:
        raise MigrationRenderError(f"invalid migration ID: {location}")
    if not isinstance(filename, str) or not MIGRATION_NAME.fullmatch(Path(filename).name):
        raise MigrationRenderError(f"invalid migration filename: {location}")
    path = _safe_relative_sql(root, filename)
    payload, _sql = _read_sql(path, filename)
    observed = hashlib.sha256(payload).hexdigest()
    expected = value.get("sha256")
    if expected is not None and (not isinstance(expected, str) or not SHA256.fullmatch(expected)):
        raise MigrationRenderError(f"invalid migration SHA-256: {location}")
    if expected is not None and expected != observed:
        raise MigrationRenderError(
            f"migration SHA-256 mismatch for {filename}: expected {expected}, observed {observed}"
        )
    return {"id": migration_id, "file": filename, "sha256": observed, "path": path}


def load_catalog(catalog_path: Path) -> tuple[dict[str, Any], Path, str]:
    """Load and fully verify a catalog, returning normalized entries and its file hash."""
    catalog_path = catalog_path.resolve()
    root = catalog_path.parent
    raw = _load_yaml(catalog_path)
    if set(raw) != {"catalogVersion", "toolVersion", "bootstrap", "domains"}:
        raise MigrationRenderError("migration catalog has unexpected top-level fields")
    if raw.get("catalogVersion") != CATALOG_VERSION or raw.get("toolVersion") != CATALOG_TOOL_VERSION:
        raise MigrationRenderError("unsupported migration catalog version")
    bootstrap = _migration(root, raw.get("bootstrap"), location="bootstrap")
    if bootstrap["id"] != 1 or bootstrap["file"] != "bootstrap/0001_roles_and_schemas.sql":
        raise MigrationRenderError("bootstrap must be bootstrap/0001_roles_and_schemas.sql with ID 1")
    domains = raw.get("domains")
    if (not isinstance(domains, list) or len(domains) != len(DOMAIN_ORDER)
            or [entry.get("name") for entry in domains if isinstance(entry, dict)] != list(DOMAIN_ORDER)):
        raise MigrationRenderError("migration domains must use the fixed release order")
    normalized_domains: list[dict[str, Any]] = []
    all_files = {bootstrap["file"]}
    for domain_name, entry in zip(DOMAIN_ORDER, domains, strict=True):
        if not isinstance(entry, dict) or set(entry) != {"name", "migrations"} or entry.get("name") != domain_name:
            raise MigrationRenderError(f"invalid migration domain: {domain_name}")
        migrations = entry.get("migrations")
        if not isinstance(migrations, list) or not migrations:
            raise MigrationRenderError(f"domain has no migrations: {domain_name}")
        normalized: list[dict[str, Any]] = []
        previous = 0
        ids: set[int] = set()
        for index, value in enumerate(migrations):
            item = _migration(root, value, location=f"{domain_name}[{index}]")
            if item["id"] <= previous or item["id"] in ids:
                raise MigrationRenderError(f"migration IDs are not strictly ordered: {domain_name}")
            if item["file"] in all_files:
                raise MigrationRenderError(f"migration file is declared more than once: {item['file']}")
            previous = item["id"]
            ids.add(item["id"])
            all_files.add(item["file"])
            normalized.append(item)
        normalized_domains.append({"name": domain_name, "migrations": normalized})
    normalized_catalog = {"catalogVersion": CATALOG_VERSION, "toolVersion": CATALOG_TOOL_VERSION,
                          "bootstrap": {"id": 1, "file": bootstrap["file"], "sha256": bootstrap["sha256"]},
                          "domains": [{"name": item["name"], "migrations": [
                              {"id": migration["id"], "file": migration["file"], "sha256": migration["sha256"]}
                              for migration in item["migrations"]]} for item in normalized_domains]}
    # The deployment SQL has historically recorded the exact catalog file
    # identity.  Keep that identity stable across YAML parsers and renderers;
    # each SQL file gets its own verified byte hash above.
    catalog_hash = hashlib.sha256(catalog_path.read_bytes()).hexdigest()
    normalized_catalog["bootstrap"]["path"] = bootstrap["path"]
    for domain in normalized_catalog["domains"]:
        for migration in domain["migrations"]:
            migration["path"] = next(
                item["path"] for source in normalized_domains if source["name"] == domain["name"]
                for item in source["migrations"] if item["file"] == migration["file"]
            )
    return normalized_catalog, root, catalog_hash


def _sql_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def _parse_assignments(values: Iterable[str], *, option: str) -> dict[str, str]:
    result: dict[str, str] = {}
    for value in values:
        domain, separator, variable = value.partition("=")
        if not separator or domain not in DOMAIN_ORDER or not IDENTIFIER.fullmatch(variable):
            raise MigrationRenderError(f"{option} must use domain=psql_variable: {value!r}")
        if domain in result:
            raise MigrationRenderError(f"duplicate {option} for domain: {domain}")
        result[domain] = variable
    return result


def render(
    catalog_path: Path,
    *,
    executor_identity: str,
    release_id: str,
    include_bootstrap: bool = False,
    runtime_password_variables: Iterable[str] = (),
) -> tuple[str, str]:
    """Return the psql script and verified catalog hash."""
    if not executor_identity or len(executor_identity) > 256 or "\n" in executor_identity or "\r" in executor_identity:
        raise MigrationRenderError("executor identity is empty or contains a newline")
    if not release_id or len(release_id) > 256 or "\n" in release_id or "\r" in release_id:
        raise MigrationRenderError("release ID is empty or contains a newline")
    catalog, root, catalog_hash = load_catalog(catalog_path)
    password_variables = _parse_assignments(runtime_password_variables, option="--runtime-password-variable")
    lines = ["\\set ON_ERROR_STOP on", "BEGIN;"]
    if include_bootstrap:
        lines.append(Path(catalog["bootstrap"]["path"]).read_text(encoding="utf-8"))
    for domain in DOMAIN_ORDER:
        variable = password_variables.get(domain)
        if variable:
            lines.append(f"ALTER ROLE lw_{domain}_runtime PASSWORD :'{variable}';")

    for domain_entry in catalog["domains"]:
        domain = domain_entry["name"]
        migrations = domain_entry["migrations"]
        expected_rows = []
        for migration in migrations:
            expected_rows.append(
                f"(migration_id = {migration['id']} AND filename = {_sql_literal(Path(migration['file']).name)} "
                f"AND sha256 = {_sql_literal(migration['sha256'])} AND outcome = 'applied')"
            )
        expected = " OR\n".join("            " + row for row in expected_rows)
        lines.extend([
            f"SET ROLE lw_{domain}_migration;",
            f"DO $verify_{domain}$",
            "DECLARE",
            "    history_count bigint;",
            "    maximum_migration_id bigint;",
            "BEGIN",
            f"    SELECT count(*), COALESCE(max(migration_id), 0) INTO history_count, maximum_migration_id FROM {domain}.schema_migrations;",
            f"    IF history_count = 0 AND EXISTS (SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = {_sql_literal(domain)} AND c.relkind IN ('r', 'p', 'v', 'm', 'S', 'f') AND c.relname <> 'schema_migrations') THEN",
            f"        RAISE EXCEPTION 'PLATFORM_{domain.upper()}_UNTRACKED_SCHEMA';",
            "    END IF;",
            "    IF EXISTS (SELECT 1 FROM " + domain + ".schema_migrations WHERE NOT (",
            expected,
            "    )) THEN",
            f"        RAISE EXCEPTION 'PLATFORM_{domain.upper()}_MIGRATION_IDENTITY_MISMATCH';",
            "    END IF;",
        ])
        for migration in migrations:
            row = (
                f"migration_id = {migration['id']} AND filename = {_sql_literal(Path(migration['file']).name)} "
                f"AND sha256 = {_sql_literal(migration['sha256'])} AND outcome = 'applied'"
            )
            lines.extend([
                f"    IF {migration['id']} <= maximum_migration_id AND NOT EXISTS (SELECT 1 FROM {domain}.schema_migrations WHERE {row}) THEN",
                f"        RAISE EXCEPTION 'PLATFORM_{domain.upper()}_MIGRATION_PREFIX_INVALID';",
                "    END IF;",
            ])
        lines.extend(["END", f"$verify_{domain}$;", "RESET ROLE;"])

        for migration in migrations:
            variable_name = f"apply_{domain}_{migration['id']}"
            lines.extend([
                f"SET ROLE lw_{domain}_migration;",
                f"SELECT NOT EXISTS (SELECT 1 FROM {domain}.schema_migrations WHERE migration_id = {migration['id']}) AS {variable_name} \\gset",
                "RESET ROLE;",
                f"\\if :{variable_name}",
                f"SET ROLE lw_{domain}_owner;",
                f"SET search_path TO {domain}, pg_catalog;",
                _read_sql(Path(migration["path"]), migration["file"])[1],
                "RESET ROLE;",
                f"SET ROLE lw_{domain}_migration;",
                f"INSERT INTO {domain}.schema_migrations (migration_id, filename, sha256, outcome, executor_identity, release_id, catalog_sha256) VALUES ({migration['id']}, {_sql_literal(Path(migration['file']).name)}, {_sql_literal(migration['sha256'])}, 'applied', {_sql_literal(executor_identity)}, {_sql_literal(release_id)}, {_sql_literal(catalog_hash)});",
                "RESET ROLE;",
                "\\endif",
            ])
        lines.extend([
            f"SET ROLE lw_{domain}_migration;",
            f"DO $complete_{domain}$",
            "BEGIN",
            f"    IF (SELECT count(*) FROM {domain}.schema_migrations) <> {len(migrations)}",
        ])
        for migration in migrations:
            row = (
                f"migration_id = {migration['id']} AND filename = {_sql_literal(Path(migration['file']).name)} "
                f"AND sha256 = {_sql_literal(migration['sha256'])} AND outcome = 'applied'"
            )
            lines.append(f"       OR NOT EXISTS (SELECT 1 FROM {domain}.schema_migrations WHERE {row})")
        lines.extend([
            "    THEN",
            f"        RAISE EXCEPTION 'PLATFORM_{domain.upper()}_MIGRATION_SET_INCOMPLETE';",
            "    END IF;",
            "END",
            f"$complete_{domain}$;",
            "RESET ROLE;",
        ])
    lines.extend(["COMMIT;", ""])
    return "\n".join(lines), catalog_hash


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--catalog", type=Path, required=True)
    parser.add_argument("--output", type=Path, default=None)
    parser.add_argument("--executor-identity", required=True)
    parser.add_argument("--release-id", required=True)
    parser.add_argument("--include-bootstrap", action="store_true")
    parser.add_argument("--runtime-password-variable", action="append", default=[])
    args = parser.parse_args(argv)
    try:
        script, catalog_hash = render(
            args.catalog,
            executor_identity=args.executor_identity,
            release_id=args.release_id,
            include_bootstrap=args.include_bootstrap,
            runtime_password_variables=args.runtime_password_variable,
        )
        if args.output is None:
            sys.stdout.write(script)
        else:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(script, encoding="utf-8", newline="\n")
            os.chmod(args.output, 0o600)
        print(json.dumps({"catalogSha256": catalog_hash, "output": str(args.output) if args.output else "-"}), file=sys.stderr)
    except (MigrationRenderError, OSError, UnicodeError) as error:
        print(f"migration SQL render failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
