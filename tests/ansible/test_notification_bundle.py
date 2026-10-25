"""SMTP has one Bundle source; real Helm consumes only its derived inputs."""
import base64
import copy
import json
import shutil
import subprocess
import unittest

import yaml
import test_deployment_bundle as existing

bundle = existing.bundle


class NotificationBundleTests(unittest.TestCase):
    def setUp(self):
        self.fixture = existing.DeploymentBundleTests(methodName="runTest")
        self.fixture.setUp()
        self.addCleanup(self.fixture.tearDown)

    def configure(self):
        # Generate an isolated trust anchor instead of depending on host CA installation.
        ca_file = self.fixture.root / "secrets/smtp-ca.pem"
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=SMTP test root", "-keyout", str(self.fixture.private / "smtp-test.key"), "-out", str(ca_file)], check=True, capture_output=True)
        ca_file.chmod(0o600)
        existing.write(self.fixture.root / "secrets/smtp-password", b"smtp-private-fixture-password")
        self.fixture.model["notifications"] = {"smtp": {
            "host": "relay.example.invalid", "port": 465, "tls": "tls",
            "username": "notifier", "passwordFile": "secrets/smtp-password",
            "caFile": "secrets/smtp-ca.pem", "from": "notices@example.invalid",
            "portalBaseUrl": "https://portal.example.invalid", "egressCidrs": ["192.0.2.10/32"],
        }}
        self.fixture.save_model()

    def test_absent_smtp_renders_complete_in_app_defaults(self):
        out = self.fixture.private / "rendered"
        bundle.derive_bundle(self.fixture.root, out)
        objects = list(yaml.safe_load_all((out / "application.yaml").read_bytes()))
        config = next(obj for obj in objects if obj["metadata"]["name"] == "control-service-config")
        self.assertEqual(json.loads(config["data"]["notifications.json"]), {"smtp": None, "expiryReminderSeconds": self.fixture.configuration["lifecycle"]["expiryReminderSeconds"]})
        values = yaml.safe_load((out / "helm-values.yaml").read_bytes())
        self.assertIsNone(values["notificationSmtpEgress"])
        self.assertEqual(values["workloadInputs"]["control-service"]["env"]["LABWEAVER_NOTIFICATIONS_FILE"], "/etc/labweaver/config/notifications.json")

    def test_initial_settings_reminders_use_the_same_custom_lifecycle_source(self):
        self.fixture.model["configurationMode"] = "initial_settings"
        self.fixture.model["revision"] = 1
        self.fixture.configuration["lifecycle"]["expiryReminderSeconds"] = [7200, 600]
        existing.write(self.fixture.root / self.fixture.model["configurationFile"], json.dumps({"expectedRevision": 0, "configuration": self.fixture.configuration}).encode())
        self.fixture.save_model()
        out = self.fixture.private / "initial-rendered"
        bundle.derive_bundle(self.fixture.root, out)
        objects = list(yaml.safe_load_all((out / "application.yaml").read_bytes()))
        config = next(obj for obj in objects if obj["metadata"]["name"] == "control-service-config")
        self.assertEqual(json.loads(config["data"]["notifications.json"])["expiryReminderSeconds"], [7200, 600])

    def test_enabled_smtp_derives_files_and_scoped_real_helm_egress(self):
        self.configure()
        out = self.fixture.private / "smtp-rendered"
        bundle.derive_bundle(self.fixture.root, out)
        objects = list(yaml.safe_load_all((out / "application.yaml").read_bytes()))
        config = next(obj for obj in objects if obj["metadata"]["name"] == "control-service-config")
        smtp = json.loads(config["data"]["notifications.json"])["smtp"]
        self.assertEqual(smtp["passwordFile"], "/etc/labweaver/secrets/smtp-password")
        self.assertNotIn("smtp-private-fixture-password", json.dumps(config))
        secret = next(obj for obj in objects if obj["metadata"]["name"] == "control-service-secrets")
        self.assertEqual(base64.b64decode(secret["data"]["smtp-password"]), b"smtp-private-fixture-password")
        helm = shutil.which("helm")
        self.assertIsNotNone(helm, "real Helm is required")
        bundle.seal_bundle(self.fixture.root, self.fixture.archive, [self.fixture.key()], self.fixture.runtime)
        with bundle.opened_bundle(self.fixture.archive, self.fixture.identity, self.fixture.runtime) as (_, loaded, _):
            result = subprocess.run([helm, "template", self.fixture.model["target"]["releaseName"], str(existing.REPOSITORY / "deploy/helm/labweaver"), "--namespace", self.fixture.model["target"]["namespace"], "-f", str(loaded / "helm-values.yaml")], capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        rendered = list(yaml.safe_load_all(result.stdout))
        control = next(obj for obj in rendered if obj.get("kind") == "Deployment" and obj["metadata"]["name"] == "control-service")
        env = control["spec"]["template"]["spec"]["containers"][0]["env"]
        self.assertIn({"name": "LABWEAVER_NOTIFICATIONS_FILE", "value": "/etc/labweaver/config/notifications.json"}, env)
        policy = next(obj for obj in rendered if obj.get("kind") == "NetworkPolicy" and obj["metadata"]["name"] == "control-service")
        self.assertIn({"to": [{"ipBlock": {"cidr": "192.0.2.10/32"}}], "ports": [{"protocol": "TCP", "port": 465}]}, policy["spec"]["egress"])
        self.assertNotIn(b"smtp-private-fixture-password", result.stdout)

    def test_invalid_smtp_is_rejected_with_field_before_rendering(self):
        self.configure()
        good = copy.deepcopy(self.fixture.model)
        for field, value in [("port", 0), ("port", True), ("tls", "plaintext"), ("passwordFile", "secrets/missing"), ("caFile", "secrets/missing"), ("egressCidrs", ["not-a-cidr"]), ("portalBaseUrl", "http://portal.invalid")]:
            with self.subTest(field=field, value=value):
                self.fixture.model = copy.deepcopy(good)
                self.fixture.model["notifications"]["smtp"][field] = value
                self.fixture.save_model()
                with self.assertRaisesRegex(bundle.BundleError, f"notifications.smtp.{field}"):
                    bundle.load_bundle(self.fixture.root)
        self.fixture.model = copy.deepcopy(good)
        self.fixture.model["application"]["helmValues"]["notificationSmtpEgress"] = {"port": 25}
        self.fixture.save_model()
        with self.assertRaisesRegex(bundle.BundleError, "LW_BUNDLE_DERIVED_OVERRIDE_FORBIDDEN"):
            bundle.load_bundle(self.fixture.root)


if __name__ == "__main__":
    unittest.main()
