#!/usr/bin/env python3
"""Prepare an isolated smoke manifest from a newly encrypted synthetic test fixture."""

import argparse
import json
import os
from pathlib import Path
from unittest.mock import patch

import test_bundle_infrastructure as fixtures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--namespace", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--create-namespace", action="store_true")
    arguments = parser.parse_args()
    fixture = fixtures.BundleInfrastructureTests(methodName="runTest")
    try:
        fixture.setUp()
        source = fixture.fixture
        source.model["configurationMode"] = "initial_settings"
        source.model["revision"] = 1
        configuration = source.root / source.model["configurationFile"]
        configuration.unlink()
        fixtures.bundle.initialize_settings(None, configuration)
        source.save_model()
        recipient = source.key()
        fixtures.bundle.seal_bundle(
            source.root, source.archive, [recipient], source.runtime
        )
        with fixtures.bundle.opened_bundle(
            source.archive, source.identity, source.runtime
        ) as (root, rendered, _), patch.dict(
            os.environ,
            {"LW_BUNDLE_ROOT": str(root), "LW_BUNDLE_RENDERED_ROOT": str(rendered)},
        ):
            metadata = fixtures.acceptance.prepare(
                arguments.namespace, arguments.output, arguments.create_namespace
            )
        source.clean_runtime()
        print(json.dumps(metadata, sort_keys=True))
        return 0
    finally:
        fixture.doCleanups()


if __name__ == "__main__":
    raise SystemExit(main())
