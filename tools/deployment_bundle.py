#!/usr/bin/env python3
"""Validate, derive and use one portable application configuration with age."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import signal
import stat
import subprocess
import sys
import tarfile
import tempfile
from typing import Any, Iterator

import yaml

try:
    import render_platform_bundle as objects
except ModuleNotFoundError as error:
    if error.name != "render_platform_bundle":
        raise
    from tools import render_platform_bundle as objects

API_VERSION = "deploy.labweaver.io/bundle/v1"
MODEL_FILE = "config/bundle.yaml"
MAX_FILES = 512
MAX_TOTAL_BYTES = 64 * 1024 * 1024
MAX_ARCHIVE_BYTES = MAX_TOTAL_BYTES + MAX_FILES * 10240
DEFAULT_MANIFEST = (
    Path(__file__).resolve().parents[1] / "deploy/config/platform-bundle-manifest.json"
)
RESERVED_VARIABLES = {
    "platform_application_namespace",
    "platform_application_release_name",
    "platform_application_configuration_bundle",
    "platform_application_configuration_manifest",
    "platform_application_values_files",
    "platform_application_versions_lock_file",
    "platform_application_package_manifest",
    "platform_application_lock",
    "platform_application_vars_file",
    "platform_application_reviewed_vars",
}
SECRET_FIELD = re.compile(
    r"(?:password|token|private_key|client_secret|secret_key|api_key|auth_token)$", re.I
)
PRIVATE_MARKERS = (
    b"PRIVATE KEY-----",
    b"AGE-SECRET-KEY-",
    b"BEGIN NATS USER NKEY SEED",
)


class BundleError(Exception):
    """A diagnostic that contains no input data or child-process output."""


class StrictLoader(yaml.SafeLoader):
    """Reject duplicate keys instead of silently replacing configuration."""


def _unique_mapping(loader: StrictLoader, node: yaml.MappingNode) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node)
        if not isinstance(key, str) or key in result:
            raise BundleError("LW_BUNDLE_DOCUMENT_INVALID")
        result[key] = loader.construct_object(value_node)
    return result


StrictLoader.add_constructor(
    yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, _unique_mapping
)


class SecretText(str):
    """Use Ansible's unsafe scalar tag so secret text cannot become a template."""


class AnsibleDumper(yaml.SafeDumper):
    """Emit secret values as literal, non-templated Ansible input."""


AnsibleDumper.add_representer(
    SecretText, lambda dumper, value: dumper.represent_scalar("!unsafe", value)
)


def _document(payload: bytes) -> Any:
    try:
        if any(
            isinstance(token, (yaml.AliasToken, yaml.AnchorToken))
            for token in yaml.scan(payload)
        ):
            raise BundleError("LW_BUNDLE_DOCUMENT_INVALID")
        return yaml.load(payload, Loader=StrictLoader)
    except (yaml.YAMLError, UnicodeError, RecursionError) as error:
        raise BundleError("LW_BUNDLE_DOCUMENT_INVALID") from error


def _relative(value: Any, prefix: str | None = None) -> str:
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9._/-]+", value):
        raise BundleError("LW_BUNDLE_REFERENCE_INVALID")
    path = PurePosixPath(value)
    if (
        path.is_absolute()
        or str(path) != value
        or any(p in (".", "..") for p in path.parts)
        or len(value) > 255
        or len(path.name) > 100
        or len(str(path.parent)) > 155
    ):
        raise BundleError("LW_BUNDLE_REFERENCE_INVALID")
    if len(path.parts) < 2 or path.parts[0] not in {
        "config",
        "secrets",
        "versions",
        "docs",
    }:
        raise BundleError("LW_BUNDLE_REFERENCE_INVALID")
    if prefix is not None and path.parts[0] != prefix:
        raise BundleError("LW_BUNDLE_REFERENCE_INVALID")
    return value


def _no_symlink_ancestors(path: Path) -> None:
    for part in (path, *path.parents):
        if part.is_symlink():
            raise BundleError("LW_BUNDLE_PATH_INVALID")


def _private_directory(path: Path, create: bool = False) -> Path:
    path = path.absolute()
    _no_symlink_ancestors(path)
    path = path.resolve()
    if ".private" not in path.parts:
        raise BundleError("LW_BUNDLE_PRIVATE_DIRECTORY_REQUIRED")
    private_root = next(p for p in (path, *path.parents) if p.name == ".private")
    # Walk from the private root. Never change permissions on an operator-owned directory.
    current = private_root
    for component in (None, *path.relative_to(private_root).parts):
        if component is not None:
            current /= component
        if create and not current.exists():
            current.mkdir(mode=0o700)
        info = current.stat()
        if (
            not stat.S_ISDIR(info.st_mode)
            or info.st_uid != os.getuid()
            or info.st_mode & 0o077
        ):
            raise BundleError("LW_BUNDLE_PERMISSIONS_INVALID")
    return path


def _files(root: Path) -> dict[str, bytes]:
    result: dict[str, bytes] = {}
    total = 0
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise BundleError("LW_BUNDLE_PATH_INVALID")
        info = path.stat()
        if info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise BundleError("LW_BUNDLE_PERMISSIONS_INVALID")
        if stat.S_ISDIR(info.st_mode):
            continue
        if (
            not stat.S_ISREG(info.st_mode)
            or not 0 < info.st_size <= objects.MAX_FILE_BYTES
        ):
            raise BundleError("LW_BUNDLE_FILE_INVALID")
        name = _relative(path.relative_to(root).as_posix())
        total += info.st_size
        if len(result) >= MAX_FILES or total > MAX_TOTAL_BYTES:
            raise BundleError("LW_BUNDLE_LIMIT_EXCEEDED")
        result[name] = path.read_bytes()
    return result


def _exact(value: Any, keys: set[str]) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != keys:
        raise BundleError("LW_BUNDLE_MODEL_INVALID")
    return value


def _value_references(value: Any, references: set[str]) -> None:
    if isinstance(value, dict):
        if set(value) in ({"file"}, {"secret"}):
            kind = next(iter(value))
            path = _relative(value[kind], "secrets" if kind == "secret" else None)
            references.add(path)
            return
        for key, item in value.items():
            if not isinstance(key, str) or (
                SECRET_FIELD.search(key)
                and not (isinstance(item, dict) and set(item) == {"secret"})
            ):
                raise BundleError("LW_BUNDLE_SECRET_REFERENCE_REQUIRED")
            _value_references(item, references)
    elif isinstance(value, list):
        for item in value:
            _value_references(item, references)
    elif isinstance(value, str) and ("{{" in value or "{%" in value):
        raise BundleError("LW_BUNDLE_TEMPLATE_FORBIDDEN")


def load_bundle(
    root: Path, manifest_path: Path = DEFAULT_MANIFEST
) -> tuple[dict[str, Any], dict[str, bytes]]:
    """Validate the complete source inventory and the existing application object contract."""
    files = _files(_private_directory(root))
    if MODEL_FILE not in files:
        raise BundleError("LW_BUNDLE_MODEL_MISSING")
    model = _exact(
        _document(files[MODEL_FILE]),
        {
            "apiVersion",
            "revision",
            "target",
            "versions",
            "docs",
            "application",
        },
    )
    if (
        model["apiVersion"] != API_VERSION
        or type(model["revision"]) is not int
        or not 1 <= model["revision"] <= 2**63 - 1
    ):
        raise BundleError("LW_BUNDLE_VERSION_INVALID")
    target = _exact(model["target"], {"namespace", "releaseName"})
    if any(
        not isinstance(v, str) or not objects.DNS_LABEL.fullmatch(v)
        for v in target.values()
    ):
        raise BundleError("LW_BUNDLE_TARGET_INVALID")
    versions = _exact(model["versions"], {"lockFile", "packageFile"})
    docs = _exact(model["docs"], {"operationsFile"})
    references = {
        MODEL_FILE,
        _relative(versions["lockFile"], "versions"),
        _relative(versions["packageFile"], "versions"),
        _relative(docs["operationsFile"], "docs"),
    }
    application = _exact(
        model["application"],
        {"configMaps", "secrets", "helmValues", "ansibleVariables"},
    )
    manifest = objects._load_manifest(manifest_path)
    for group, prefix in (("configMaps", "config"), ("secrets", "secrets")):
        bindings = _exact(application[group], set(manifest[group]))
        for name, keys in manifest[group].items():
            for ref in _exact(bindings[name], set(keys)).values():
                references.add(_relative(ref, prefix))
    if not isinstance(application["helmValues"], dict) or not isinstance(
        application["ansibleVariables"], dict
    ):
        raise BundleError("LW_BUNDLE_MODEL_INVALID")
    if {"deploymentIdentity", "images"} & set(application["helmValues"]):
        raise BundleError("LW_BUNDLE_DERIVED_OVERRIDE_FORBIDDEN")
    for key in application["ansibleVariables"]:
        if not key.startswith("platform_application_") or key in RESERVED_VARIABLES:
            raise BundleError("LW_BUNDLE_DERIVED_OVERRIDE_FORBIDDEN")
    _value_references(application["ansibleVariables"], references)
    helm_references: set[str] = set()
    _value_references(application["helmValues"], helm_references)
    if helm_references:
        raise BundleError("LW_BUNDLE_HELM_SECRET_FORBIDDEN")
    if set(files) != references:
        raise BundleError("LW_BUNDLE_INVENTORY_MISMATCH")
    lock = _document(files[versions["lockFile"]])
    package = _document(files[versions["packageFile"]])
    if (
        not isinstance(lock, dict)
        or lock.get("schema_version") != 1
        or not isinstance(package, dict)
        or not isinstance(package.get("images"), list)
    ):
        raise BundleError("LW_BUNDLE_VERSIONS_INVALID")
    components: set[str] = set()
    for image in package["images"]:
        if (
            not isinstance(image, dict)
            or not isinstance(image.get("component"), str)
            or not objects.DNS_LABEL.fullmatch(image["component"])
            or image["component"] in components
            or not isinstance(image.get("reference"), str)
            or not re.fullmatch(r"[^\s@]+@sha256:[0-9a-f]{64}", image["reference"])
        ):
            raise BundleError("LW_BUNDLE_VERSIONS_INVALID")
        components.add(image["component"])
    secret_payloads = {
        payload
        for name, payload in files.items()
        if name.startswith("secrets/") and len(payload) >= 16
    }
    for name, payload in files.items():
        if b"AGE-SECRET-KEY-" in payload or payload.startswith(
            b"age-encryption.org/v1\n"
        ):
            raise BundleError("LW_BUNDLE_IDENTITY_MUST_BE_SEPARATE")
        if not name.startswith("secrets/") and (
            any(marker in payload for marker in PRIVATE_MARKERS)
            or any(secret in payload for secret in secret_payloads)
        ):
            raise BundleError("LW_BUNDLE_SECRET_LOCATION_INVALID")
    return model, files


def _write(path: Path, payload: bytes) -> None:
    _private_directory(path.parent, create=True)
    objects._write_exclusive(path, payload)


def _resolve_values(value: Any, root: Path, files: dict[str, bytes]) -> Any:
    if isinstance(value, dict):
        if set(value) == {"file"}:
            return str(root / value["file"])
        if set(value) == {"secret"}:
            try:
                return SecretText(files[value["secret"]].decode("utf-8").rstrip("\n"))
            except UnicodeError as error:
                raise BundleError("LW_BUNDLE_SECRET_TEXT_INVALID") from error
        return {key: _resolve_values(item, root, files) for key, item in value.items()}
    if isinstance(value, list):
        return [_resolve_values(item, root, files) for item in value]
    return value


def derive_bundle(
    root: Path, output: Path, manifest_path: Path = DEFAULT_MANIFEST
) -> dict[str, Any]:
    """Derive application objects, Helm values and Ansible inputs from the same source."""
    model, files = load_bundle(root, manifest_path)
    application = model["application"]
    _private_directory(output.parent)
    _no_symlink_ancestors(output.absolute())
    output.mkdir(mode=0o700)
    manifest = objects._load_manifest(manifest_path)
    manifest["namespace"] = model["target"]["namespace"]
    manifest_output = output / "object-manifest.json"
    _write(manifest_output, json.dumps(manifest, sort_keys=True).encode())
    inputs = output / "object-inputs"
    for group, directory in (("configMaps", "configmaps"), ("secrets", "secrets")):
        for name, bindings in application[group].items():
            for key, ref in bindings.items():
                _write(inputs / directory / name / key, files[ref])
    payload = objects.render_bundle(manifest_output, inputs)
    object_output = output / "application.yaml"
    _write(object_output, payload)
    # The object inputs are derived scratch, never a second editable configuration.
    shutil.rmtree(inputs)
    digest = "sha256:" + hashlib.sha256(payload).hexdigest()
    values = dict(application["helmValues"])
    package = _document(files[model["versions"]["packageFile"]])
    values["images"] = {
        image["component"].replace("-", "_"): image["reference"]
        for image in package["images"]
    }
    values["deploymentIdentity"] = {"configurationBundleSha256": digest}
    values_output = output / "helm-values.yaml"
    _write(values_output, yaml.safe_dump(values, sort_keys=True).encode())
    variables = _resolve_values(application["ansibleVariables"], root, files)
    variables.update(
        {
            "platform_application_namespace": model["target"]["namespace"],
            "platform_application_release_name": model["target"]["releaseName"],
            "platform_application_versions_lock_file": str(
                root / model["versions"]["lockFile"]
            ),
            "platform_application_package_manifest": str(
                root / model["versions"]["packageFile"]
            ),
            "platform_application_configuration_manifest": str(manifest_output),
            "platform_application_configuration_bundle": str(object_output),
            "platform_application_values_files": [str(values_output)],
        }
    )
    _write(
        output / "ansible-vars.yaml",
        yaml.dump(variables, Dumper=AnsibleDumper, sort_keys=True).encode(),
    )
    return {
        "revision": model["revision"],
        "configurationSha256": digest,
        "files": len(files),
    }


def _age(argv: list[str], **kwargs: Any) -> None:
    try:
        subprocess.run(
            ["age", *argv], check=True, timeout=120, stderr=subprocess.DEVNULL, **kwargs
        )
    except FileNotFoundError as error:
        raise BundleError("LW_BUNDLE_AGE_REQUIRED") from error
    except (subprocess.SubprocessError, OSError) as error:
        raise BundleError("LW_BUNDLE_AGE_FAILED") from error


def seal_bundle(
    root: Path,
    output: Path,
    recipients: list[str],
    runtime: Path,
    manifest_path: Path = DEFAULT_MANIFEST,
) -> dict[str, Any]:
    """Encrypt the entire validated portable directory. Never replace an existing archive."""
    model, files = load_bundle(root, manifest_path)
    if not recipients or any(
        not re.fullmatch(r"age1[0-9a-z]+", recipient) for recipient in recipients
    ):
        raise BundleError("LW_BUNDLE_RECIPIENT_REQUIRED")
    runtime = _private_directory(runtime, create=True)
    _no_symlink_ancestors(output.absolute())
    if output.exists():
        raise BundleError("LW_BUNDLE_OUTPUT_EXISTS")
    # Validate the actual deployment object path before exporting, including NATS permissions.
    with tempfile.TemporaryDirectory(prefix="bundle-", dir=runtime) as temporary:
        temporary_root = Path(temporary)
        source_root = temporary_root / "bundle"
        for name, payload in files.items():
            _write(source_root / name, payload)
        derive_bundle(source_root, temporary_root / "rendered", manifest_path)
        archive_path = temporary_root / "source.tar"
        _write(archive_path, b"")
        with tarfile.open(
            archive_path, mode="w", format=tarfile.USTAR_FORMAT
        ) as archive:
            for name in sorted(files):
                with io.BytesIO(files[name]) as source:
                    info = tarfile.TarInfo(name)
                    info.size = len(files[name])
                    info.mode = 0o600
                    archive.addfile(info, source)
        # Ciphertext is staged on the destination filesystem for an atomic no-replace publish.
        output.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        descriptor, pending_name = tempfile.mkstemp(
            prefix=".bundle-", dir=output.parent
        )
        pending = Path(pending_name)
        try:
            with os.fdopen(descriptor, "wb") as encrypted, archive_path.open(
                "rb"
            ) as source:
                _age(
                    [
                        "--encrypt",
                        *[arg for recipient in recipients for arg in ("-r", recipient)],
                    ],
                    stdin=source,
                    stdout=encrypted,
                )
                encrypted.flush()
                os.fsync(encrypted.fileno())
            os.link(pending, output)
        except FileExistsError as error:
            raise BundleError("LW_BUNDLE_OUTPUT_EXISTS") from error
        finally:
            pending.unlink(missing_ok=True)
    return {"revision": model["revision"], "files": len(files)}


def _extract(archive_path: Path, destination: Path) -> None:
    destination.mkdir(mode=0o700)
    names: set[str] = set()
    total = 0
    try:
        with tarfile.open(archive_path, mode="r:") as archive:
            for member in archive:
                name = _relative(member.name)
                if (
                    not member.isfile()
                    or name in names
                    or not 0 < member.size <= objects.MAX_FILE_BYTES
                ):
                    raise BundleError("LW_BUNDLE_ARCHIVE_INVALID")
                names.add(name)
                total += member.size
                if len(names) > MAX_FILES or total > MAX_TOTAL_BYTES:
                    raise BundleError("LW_BUNDLE_LIMIT_EXCEEDED")
                source = archive.extractfile(member)
                if source is None:
                    raise BundleError("LW_BUNDLE_ARCHIVE_INVALID")
                with source:
                    payload = source.read(member.size + 1)
                if len(payload) != member.size:
                    raise BundleError("LW_BUNDLE_ARCHIVE_INVALID")
                _write(destination / name, payload)
    except (tarfile.TarError, OSError) as error:
        raise BundleError("LW_BUNDLE_ARCHIVE_INVALID") from error


@contextmanager
def opened_bundle(
    archive: Path, identity: Path, runtime: Path, manifest_path: Path = DEFAULT_MANIFEST
) -> Iterator[tuple[Path, Path, dict[str, Any]]]:
    """Authenticate, validate and derive in a fresh runtime. Cleanup covers all exceptions."""
    _no_symlink_ancestors(archive.absolute())
    _no_symlink_ancestors(identity.absolute())
    archive = archive.resolve()
    identity = identity.resolve()
    if not identity.is_file() or identity.stat().st_mode & 0o077:
        raise BundleError("LW_BUNDLE_IDENTITY_REQUIRED")
    if not archive.is_file() or not 0 < archive.stat().st_size <= MAX_ARCHIVE_BYTES:
        raise BundleError("LW_BUNDLE_ARCHIVE_INVALID")
    runtime = _private_directory(runtime, create=True)
    with tempfile.TemporaryDirectory(prefix="bundle-", dir=runtime) as temporary:
        temporary_root = Path(temporary)
        if identity.absolute().is_relative_to(
            temporary_root
        ) or identity.absolute().is_relative_to(runtime):
            raise BundleError("LW_BUNDLE_IDENTITY_MUST_BE_SEPARATE")
        archive_path = temporary_root / "source.tar"
        _write(archive_path, b"")
        with archive_path.open("wb") as plaintext, archive.open("rb") as ciphertext:
            _age(
                ["--decrypt", "--identity", str(identity.absolute())],
                stdin=ciphertext,
                stdout=plaintext,
            )
        if archive_path.stat().st_size > MAX_ARCHIVE_BYTES:
            raise BundleError("LW_BUNDLE_LIMIT_EXCEEDED")
        root = temporary_root / "bundle"
        _extract(archive_path, root)
        archive_path.unlink()
        output = temporary_root / "rendered"
        metadata = derive_bundle(root, output, manifest_path)
        yield root, output, metadata


def _interrupt(signum: int, _frame: Any) -> None:
    raise BundleError("LW_BUNDLE_INTERRUPTED")


def _consume(command: list[str], environment: dict[str, str]) -> None:
    # Keep descendants in one process group. They must stop before plaintext disappears.
    with subprocess.Popen(
        command,
        env=environment,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    ) as process:
        try:
            if process.wait():
                raise BundleError("LW_BUNDLE_COMMAND_FAILED")
        finally:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
            # Also stop detached-in-group descendants after the main consumer exited.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime", type=Path, default=Path(".private/bundle-runtime"))
    subcommands = parser.add_subparsers(dest="operation", required=True)
    validate = subcommands.add_parser("validate")
    validate.add_argument("--bundle", type=Path, required=True)
    seal = subcommands.add_parser("seal")
    seal.add_argument("--bundle", type=Path, required=True)
    seal.add_argument("--recipient", action="append", required=True)
    seal.add_argument("--output", type=Path, required=True)
    use = subcommands.add_parser("exec")
    use.add_argument("--archive", type=Path, required=True)
    use.add_argument("--identity", type=Path, required=True)
    use.add_argument("command", nargs=argparse.REMAINDER)
    arguments = parser.parse_args()
    previous = {
        sig: signal.signal(sig, _interrupt) for sig in (signal.SIGINT, signal.SIGTERM)
    }
    try:
        if arguments.operation == "validate":
            # Validation includes the same derived object checks used during encrypted execution.
            runtime = _private_directory(arguments.runtime, create=True)
            with tempfile.TemporaryDirectory(
                prefix="bundle-", dir=runtime
            ) as temporary:
                metadata = derive_bundle(
                    arguments.bundle.absolute(), Path(temporary) / "rendered"
                )
        elif arguments.operation == "seal":
            metadata = seal_bundle(
                arguments.bundle.absolute(),
                arguments.output.absolute(),
                arguments.recipient,
                arguments.runtime,
            )
        else:
            with opened_bundle(
                arguments.archive, arguments.identity, arguments.runtime
            ) as (root, rendered, metadata):
                command = arguments.command
                if command[:1] == ["--"]:
                    command = command[1:]
                if command:
                    environment = dict(
                        os.environ,
                        LW_BUNDLE_ROOT=str(root),
                        LW_BUNDLE_RENDERED_ROOT=str(rendered),
                        LABWEAVER_APPLICATION_VARS_FILE=str(
                            rendered / "ansible-vars.yaml"
                        ),
                    )
                    # A consumer can read secrets. Do not forward its stdout/stderr to ordinary logs.
                    _consume(command, environment)
        print(json.dumps(metadata, sort_keys=True))
        return 0
    except (BundleError, objects.BundleError, OSError, KeyboardInterrupt) as error:
        diagnostic = (
            str(error)
            if isinstance(error, (BundleError, objects.BundleError))
            else "LW_BUNDLE_IO_FAILED"
        )
        print(diagnostic, file=sys.stderr)
        return 1
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)


if __name__ == "__main__":
    raise SystemExit(main())
