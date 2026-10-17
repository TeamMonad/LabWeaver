#!/usr/bin/env python3
"""Authenticate a synthetic Bundle and materialize only its Agent client inputs."""

import argparse
import base64
import json
from pathlib import Path

import yaml
import test_deployment_bundle as fixtures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--settings", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    settings = json.loads(args.settings.read_bytes())
    f = fixtures.DeploymentBundleTests(methodName="runTest")
    try:
        f.setUp()
        app = f.model["application"]
        environment = app["workloadInputs"]["environment-service"]["env"]
        environment["LABWEAVER_CONTROL_SERVICE_BASE_URI"] = settings["origin"]
        environment["LABWEAVER_CONTROL_SERVICE_TIMEOUT_MILLISECONDS"] = "5000"
        for role in ("agent-service", "control-service"):
            app["workloadInputs"][role]["env"]["LABWEAVER_SERVICE_OIDC_ISSUER"] = (
                settings["issuer"]
            )
            app["workloadInputs"][role]["env"][
                "LABWEAVER_SERVICE_JWT_ALGORITHMS"
            ] = "ES256"
            app["secrets"][role + "-secrets"]["oidc-ca.pem"] = "secrets/test-ca"
        for role in ("agent-service", "environment-service"):
            app["secrets"][role + "-secrets"]["mtls-ca.pem"] = "secrets/test-ca"
        app["secrets"]["agent-service-secrets"][
            "service-client-secret"
        ] = "secrets/test-agent-secret"
        fixtures.write(f.root / "secrets/test-ca", settings["certificate"].encode())
        fixtures.write(
            f.root / "secrets/test-agent-secret", settings["clientSecret"].encode()
        )
        assignments = yaml.safe_load(
            (
                fixtures.REPOSITORY
                / "deploy/ansible/roles/identity_foundation/defaults/main.yml"
            ).read_bytes()
        )["identity_service_role_assignments"]
        f.model["infrastructure"] = {
            "inventoryFile": "config/identity-inventory.yaml",
            "vaultPasswordFile": None,
            "identitySecretFile": None,
        }
        fixtures.write(
            f.root / "config/identity-inventory.yaml",
            yaml.safe_dump(
                {"all": {"vars": {"identity_service_role_assignments": assignments}}}
            ).encode(),
        )
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
            agent = dict(values["workloadInputs"]["agent-service"]["env"])
            control = dict(values["workloadInputs"]["control-service"]["env"])
            for variable, key in (
                ("LABWEAVER_CONTROL_SERVICE_CA_PATH", "mtls-ca.pem"),
                ("LABWEAVER_SERVICE_OIDC_CA", "oidc-ca.pem"),
                ("LABWEAVER_SERVICE_CLIENT_SECRET_FILE", "service-client-secret"),
            ):
                path = args.output / key
                fixtures.write(
                    path,
                    base64.b64decode(objects["agent-service-secrets"]["data"][key]),
                )
                agent[variable] = str(path)
            control["LABWEAVER_SERVICE_OIDC_CA"] = agent["LABWEAVER_SERVICE_OIDC_CA"]
            assignments = yaml.safe_load(
                (rendered / "infrastructure/inventory.yaml").read_bytes()
            )["all"]["vars"]["identity_service_role_assignments"]
            fixtures.write(
                args.output / "consumer.json",
                json.dumps(
                    {
                        "agent": agent,
                        "control": control,
                        "assignments": assignments,
                        "source": metadata,
                    }
                ).encode(),
            )
        f.clean_runtime()
        return 0
    finally:
        f.tearDown()


if __name__ == "__main__":
    raise SystemExit(main())
