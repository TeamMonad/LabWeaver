#!/usr/bin/env python3
"""Test the published #216 consumer without changing its production implementation."""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--consumer-root", type=Path, required=True)
    parser.add_argument(
        "--candidate-root", type=Path, default=Path(__file__).resolve().parents[1]
    )
    args = parser.parse_args()
    root = args.consumer_root.resolve()
    candidate = args.candidate_root.resolve()
    source_sha = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=root, text=True
    ).strip()
    if source_sha != "50a812768a67f14e7107282c6ab8394d82998410":
        raise SystemExit("expected the immutable published #216/#207 consumer source")
    for name in (
        "tools/deployment_bundle.py",
        "deploy/helm/labweaver/values.yaml",
        "deploy/helm/labweaver/templates/_helpers.tpl",
        "deploy/helm/labweaver/templates/workloads.yaml",
        "deploy/ansible/roles/identity_foundation/defaults/main.yml",
        "tests/ansible/test_deployment_bundle.py",
        "tests/ansible/prepare_agent_control_fixture.py",
        "tests/fixtures/agent_control_bundle_consumer.rs",
    ):
        source, target = candidate / name, root / name
        if source.resolve() != target.resolve():
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
    consumer = root / "services/agent-service/src/model_input_authorization.rs"
    fixture = root / "tests/fixtures/agent_control_bundle_consumer.rs"
    original = consumer.read_bytes()
    if b"mod bundle_connection_tests" in original:
        raise SystemExit("consumer already has a test injection")
    print("Frozen consumer source:", source_sha, flush=True)
    print("Production module SHA256:", hashlib.sha256(original).hexdigest(), flush=True)
    try:
        consumer.write_bytes(
            original
            + (
                f"\n#[cfg(test)]\n#[path = {json.dumps(str(fixture))}]\nmod bundle_connection_tests;\n"
            ).encode()
        )
        result = subprocess.run(
            [
                "cargo",
                "test",
                "-p",
                "agent-service",
                "--lib",
                "model_input_authorization::bundle_connection_tests::authenticated_bundle_reaches_control_with_the_existing_role",
                "--locked",
                "--",
                "--ignored",
                "--exact",
                "--test-threads=1",
            ],
            cwd=root,
        )
        return result.returncode
    finally:
        consumer.write_bytes(original)
        assert consumer.read_bytes() == original
        print("Production consumer module restored exactly.", flush=True)


if __name__ == "__main__":
    raise SystemExit(main())
