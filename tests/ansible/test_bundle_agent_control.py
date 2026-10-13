"""Render only synthetic Agent→Control connection and existing identity inputs."""

import copy
import json
import subprocess
import unittest

from jinja2 import Environment, StrictUndefined
import yaml
import test_deployment_bundle as fixtures


class BundleAgentControlTests(unittest.TestCase):
    def setUp(self):
        self.f = fixtures.DeploymentBundleTests(methodName="runTest")
        self.f.setUp()
        self.addCleanup(self.f.tearDown)

    def test_actual_age_and_helm_use_one_control_connection_and_agent_identity(self):
        f = self.f
        app = f.model["application"]
        owner = app["workloadInputs"]["environment-service"]["env"]
        owner["LABWEAVER_CONTROL_SERVICE_BASE_URI"] = (
            "https://control.example.invalid:9444/"
        )
        owner["LABWEAVER_CONTROL_SERVICE_TIMEOUT_MILLISECONDS"] = "2500"
        f.save_model()
        fixtures.bundle.seal_bundle(f.root, f.archive, [f.key()], f.runtime)
        with fixtures.bundle.opened_bundle(f.archive, f.identity, f.runtime) as (
            _,
            root,
            _,
        ):
            values = yaml.safe_load((root / "helm-values.yaml").read_bytes())
            result = subprocess.run(
                [
                    "helm",
                    "template",
                    "labweaver",
                    str(fixtures.bundle.WORKLOADS_FILE.parent),
                    "--namespace",
                    "labweaver-system",
                    "--values",
                    str(root / "helm-values.yaml"),
                ],
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            objects = {
                o["metadata"]["name"]: o
                for o in yaml.safe_load_all(result.stdout)
                if o and o["kind"] == "Deployment"
            }
            for name in ("agent-service", "environment-service"):
                env = {
                    e["name"]: e["value"]
                    for e in objects[name]["spec"]["template"]["spec"]["containers"][0][
                        "env"
                    ]
                    if "value" in e
                }
                for key in (
                    "LABWEAVER_CONTROL_SERVICE_BASE_URI",
                    "LABWEAVER_CONTROL_SERVICE_CA_PATH",
                    "LABWEAVER_CONTROL_SERVICE_AUDIENCE",
                    "LABWEAVER_CONTROL_SERVICE_TIMEOUT_MILLISECONDS",
                ):
                    self.assertEqual(env[key], owner[key])
                self.assertEqual(
                    env["LABWEAVER_SERVICE_CLIENT_ID"],
                    (
                        "labweaver-agent"
                        if name == "agent-service"
                        else "labweaver-environment"
                    ),
                )
            agent = values["workloadInputs"]["agent-service"]["env"]
            self.assertIn(
                "control.agent_run.read", agent["LABWEAVER_SERVICE_SCOPES"].split(",")
            )
            self.assertNotIn(f.secret.decode(), result.stdout + result.stderr)
            bad = subprocess.run(
                [
                    "helm",
                    "template",
                    "labweaver",
                    str(fixtures.bundle.WORKLOADS_FILE.parent),
                    "--values",
                    str(root / "helm-values.yaml"),
                    "--set-string",
                    "workloadInputs.agent-service.env.LABWEAVER_CONTROL_SERVICE_BASE_URI=https://another.invalid/",
                ],
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(bad.returncode, 0)
            self.assertIn("Agent Control connection owns", bad.stderr)
        f.clean_runtime()

    def test_source_rejects_separate_agent_origin_audience_or_timeout(self):
        env = self.f.model["application"]["workloadInputs"]["agent-service"]["env"]
        original = copy.deepcopy(env)
        for key, value in (
            ("LABWEAVER_CONTROL_SERVICE_BASE_URI", "https://another.invalid/"),
            ("LABWEAVER_CONTROL_SERVICE_AUDIENCE", "another-audience"),
            ("LABWEAVER_CONTROL_SERVICE_TIMEOUT_MILLISECONDS", "6000"),
        ):
            with self.subTest(key=key):
                env.clear()
                env.update(original)
                env[key] = value
                self.f.save_model()
                with self.assertRaisesRegex(
                    fixtures.bundle.BundleError, "LW_BUNDLE_AGENT_CONTROL_CONFLICT"
                ):
                    fixtures.bundle.load_bundle(self.f.root)

    def test_control_origin_and_timeout_match_the_native_consumer_limits(self):
        env = self.f.model["application"]["workloadInputs"]["environment-service"][
            "env"
        ]
        original = copy.deepcopy(env)
        for key, value in (
            ("LABWEAVER_CONTROL_SERVICE_BASE_URI", "http://control.invalid/"),
            ("LABWEAVER_CONTROL_SERVICE_BASE_URI", "https://control.invalid/path"),
            (
                "LABWEAVER_CONTROL_SERVICE_BASE_URI",
                "https://user:password@control.invalid/",
            ),
            ("LABWEAVER_CONTROL_SERVICE_TIMEOUT_MILLISECONDS", "0"),
            ("LABWEAVER_CONTROL_SERVICE_TIMEOUT_MILLISECONDS", "5001"),
        ):
            with self.subTest(key=key, value=value):
                env.clear()
                env.update(original)
                env[key] = value
                self.f.save_model()
                with self.assertRaisesRegex(
                    fixtures.bundle.BundleError, "LW_BUNDLE_AGENT_CONTROL_INVALID"
                ):
                    fixtures.bundle.load_bundle(self.f.root)

    def test_required_read_role_caller_and_ca_source_cannot_be_omitted(self):
        app = self.f.model["application"]
        original = copy.deepcopy(app)
        for change in ("role", "caller", "ca"):
            with self.subTest(change=change):
                self.f.model["application"] = app = copy.deepcopy(original)
                if change == "role":
                    app["workloadInputs"]["agent-service"]["env"][
                        "LABWEAVER_SERVICE_SCOPES"
                    ] = "agent.control.invoke"
                elif change == "caller":
                    app["workloadInputs"]["control-service"]["env"][
                        "LABWEAVER_SERVICE_ALLOWED_CLIENT_IDS"
                    ] = "labweaver-access,labweaver-environment,labweaver-evaluation"
                else:
                    fixtures.write(
                        self.f.root / "secrets/another-ca", b"synthetic-separate-ca"
                    )
                    app["secrets"]["agent-service-secrets"][
                        "mtls-ca.pem"
                    ] = "secrets/another-ca"
                self.f.save_model()
                with self.assertRaisesRegex(
                    fixtures.bundle.BundleError,
                    "LW_BUNDLE_AGENT_CONTROL_IDENTITY_MISMATCH",
                ):
                    fixtures.bundle.load_bundle(self.f.root)

    def test_existing_identity_template_adds_only_agent_control_read_assignment(self):
        root = fixtures.REPOSITORY / "deploy/ansible/roles/identity_foundation"
        context = yaml.safe_load((root / "defaults/main.yml").read_bytes())
        assignments = context["identity_service_role_assignments"]
        agent_control = [
            a
            for a in assignments
            if a["caller"] == "labweaver-agent" and a["target"] == "labweaver-control"
        ]
        self.assertEqual(
            agent_control,
            [
                {
                    "caller": "labweaver-agent",
                    "target": "labweaver-control",
                    "roles": ["control.agent_run.read"],
                }
            ],
        )
        self.assertEqual(
            [
                a
                for a in assignments
                if a["caller"] == "labweaver-environment"
                and a["target"] == "labweaver-control"
            ][0]["roles"],
            ["control.agent_run.read"],
        )
        context.update(
            identity_run_suffix="synthetic",
            identity_run_id="synthetic",
            identity_lock={
                "images": {"keycloak": "synthetic.invalid/keycloak@sha256:" + "a" * 64}
            },
        )
        environment = Environment(undefined=StrictUndefined)
        environment.filters.update(bool=bool, to_json=json.dumps)
        rendered = environment.from_string(
            (root / "templates/provision-job.yml.j2").read_text()
        ).render(**context)
        job = yaml.safe_load(rendered)
        script = job["spec"]["template"]["spec"]["containers"][0]["args"][0]
        self.assertIn(
            '--uusername "service-account-labweaver-agent" --cclientid "labweaver-control" --rolename "control.agent_run.read"',
            script,
        )
        self.assertIn("fullScopeAllowed=false", script)
        self.assertNotIn("fullScopeAllowed=true", script)
        self.assertNotIn("composite=true", script)
        self.assertIn("automountServiceAccountToken: false", rendered)


if __name__ == "__main__":
    unittest.main()
