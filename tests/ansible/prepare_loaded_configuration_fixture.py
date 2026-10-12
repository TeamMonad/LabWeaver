#!/usr/bin/env python3
"""Create and load one synthetic age Bundle for the real Control integration target."""

import argparse
import json
from pathlib import Path

import test_deployment_bundle as fixtures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    fixture = fixtures.DeploymentBundleTests(methodName="runTest")
    fixture.setUp()
    try:
        version = json.loads(arguments.version.read_bytes())
        fixture.model["revision"] = version["revision"]
        fixtures.write(
            fixture.root / fixture.model["configurationFile"],
            json.dumps(version).encode(),
        )
        fixture.save_model()
        recipient = fixture.key()
        fixtures.bundle.seal_bundle(
            fixture.root, fixture.archive, [recipient], fixture.runtime
        )
        output = fixtures.bundle._private_directory(arguments.output, create=True)
        with fixtures.bundle.opened_bundle(
            fixture.archive, fixture.identity, fixture.runtime
        ) as (_, rendered, _):
            for name in (
                "platform-configuration.json",
                "platform-configuration-source.json",
            ):
                fixtures.write(output / name, (rendered / name).read_bytes())
            fixtures.write(output / "synthetic.age", fixture.archive.read_bytes())
        fixture.clean_runtime()
        return 0
    finally:
        fixture.tearDown()


if __name__ == "__main__":
    raise SystemExit(main())
