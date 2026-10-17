#!/usr/bin/env python3
"""Materialize synthetic authenticated Bundle inputs for the Rust consumers."""

import argparse
import base64
import json
from pathlib import Path

import yaml
import test_deployment_bundle as fixtures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    settings = json.loads(args.input.read_bytes())
    f = fixtures.DeploymentBundleTests(methodName="runTest")
    try:
        f.setUp()
        app = f.model["application"]
        owner = app["workloadInputs"]["environment-service"]["env"]
        owner["LABWEAVER_SERVICE_OIDC_ISSUER"] = settings["issuer"]
        owner["LABWEAVER_RESOURCE_USAGE_BASE_URI"] = settings["resourceUri"]
        owner["LABWEAVER_SERVICE_CLIENT_ID"] = "synthetic-environment"
        for role in ("container-executor", "kubevirt-executor"):
            app["workloadInputs"][role]["env"].pop(
                "LABWEAVER_SERVICE_OIDC_ISSUER", None
            )
        for key, value in (
            ("database-url", settings["databaseUrl"]),
            ("oidc-ca.pem", settings["certificate"]),
            ("mtls-ca.pem", settings["certificate"]),
            ("service-client-secret", settings["clientSecret"]),
        ):
            reference = "secrets/owner-" + key
            app["secrets"]["environment-service-secrets"][key] = reference
            fixtures.write(f.root / reference, value.encode())
            if key == "database-url":
                for role in ("container-executor", "kubevirt-executor"):
                    app["secrets"][role + "-secrets"][key] = reference
        f.save_model()
        fixtures.bundle.seal_bundle(f.root, f.archive, [f.key()], f.runtime)
        with fixtures.bundle.opened_bundle(f.archive, f.identity, f.runtime) as (
            _,
            rendered,
            metadata,
        ):
            values = yaml.safe_load((rendered / "helm-values.yaml").read_bytes())
            objects = {
                o["metadata"]["name"]: o
                for o in yaml.safe_load_all(
                    (rendered / "application.yaml").read_bytes()
                )
            }
            args.output.mkdir(mode=0o700)
            roles = {}
            for role in ("container-executor", "kubevirt-executor"):
                directory = args.output / role
                directory.mkdir(mode=0o700)
                authority = directory / "authority"
                for source in values["environmentExecutorAuthority"]["secretSources"]:
                    for item in source["items"]:
                        fixtures.write(
                            authority / item["path"],
                            base64.b64decode(
                                objects[source["name"]]["data"][item["key"]]
                            ),
                        )
                config = yaml.safe_load(
                    objects[role + "-config"]["data"]["config.yaml"]
                )
                config["databaseUrlFile"] = str(authority / "database-url")
                config["nats"]["serverFile"] = str(authority / "nats-server")
                fixtures.write(
                    directory / "config.yaml", yaml.safe_dump(config).encode()
                )
                env = dict(values["workloadInputs"][role]["env"])
                for key in (
                    "LABWEAVER_SERVICE_OIDC_CA",
                    "LABWEAVER_SERVICE_CLIENT_SECRET_FILE",
                    "LABWEAVER_RESOURCE_USAGE_CA_PATH",
                ):
                    env[key] = str(authority / Path(env[key]).name)
                env["LABWEAVER_DATABASE_URL"] = settings["databaseUrl"]
                env["LABWEAVER_RUNTIME_EXECUTOR_CONFIG_FILE"] = str(
                    directory / "config.yaml"
                )
                roles[role] = env
            fixtures.write(
                args.output / "consumer-inputs.json",
                json.dumps({"roles": roles, "source": metadata}).encode(),
            )
        f.clean_runtime()
        return 0
    finally:
        f.tearDown()


if __name__ == "__main__":
    raise SystemExit(main())
