"""Rendered model-input proxy ingress and cross-namespace trust configuration."""
from pathlib import Path
import subprocess
import unittest
from urllib.parse import urlsplit

import yaml

ROOT = Path(__file__).resolve().parents[2]


class ModelInputAuthorizationDeploymentTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        chart = ROOT / "deploy/helm/labweaver"
        values = yaml.safe_load((chart / "values.yaml").read_text())
        arguments = ["helm", "template", "model-input", str(chart), "-f", str(chart / "values.local-kind.yaml"), "--namespace", "labweaver-system",
                     "--show-only", "templates/network-policy.yaml", "--set-string",
                     "deploymentIdentity.configurationBundleSha256=sha256:" + "a" * 64]
        for name in values["images"]:
            arguments.extend(["--set-string", f"images.{name}=example.invalid/render-fixture@sha256:" + "b" * 64])
        rendered = subprocess.run(arguments, check=False, capture_output=True, text=True)
        if rendered.returncode:
            raise AssertionError(rendered.stderr)
        cls.policies = {
            item["metadata"]["name"]: item
            for item in yaml.safe_load_all(rendered.stdout)
            if item and item.get("kind") == "NetworkPolicy"
        }

    def test_only_owned_authoring_pods_reach_the_attempt_proxy_port(self) -> None:
        rules = self.policies["agent-service"]["spec"]["ingress"]
        cross_namespace = [rule for rule in rules if any("namespaceSelector" in peer for peer in rule["from"])]
        self.assertEqual(len(cross_namespace), 1)
        self.assertEqual(cross_namespace[0]["ports"], [{"protocol": "TCP", "port": 9445}])
        self.assertEqual(cross_namespace[0]["from"], [{
            "namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "labweaver-authoring"}},
            "podSelector": {"matchLabels": {"labweaver.io/managed-by": "agent-service"}},
        }])

    def test_agent_reaches_the_control_authorization_api(self) -> None:
        rules = self.policies["control-service"]["spec"]["ingress"]
        self.assertTrue(any(
            "agent-service" in peer.get("podSelector", {}).get("matchExpressions", [{}])[0].get("values", [])
            and rule["ports"] == [{"protocol": "TCP", "port": 9444}]
            for rule in rules for peer in rule["from"]
        ))

    def test_sandbox_uses_cross_namespace_agent_dns_not_a_provider_credential(self) -> None:
        configuration = yaml.safe_load((ROOT / "deploy/config/agent-control-plane.yaml.example").read_text())
        sandbox = configuration["sandbox"]
        proxy = urlsplit(sandbox["model_proxy_base_uri"])
        self.assertEqual(proxy.scheme, "https")
        self.assertEqual(proxy.hostname, "agent-service.labweaver-system.svc")
        self.assertEqual(proxy.port, 9445)
        self.assertEqual(sandbox["model_proxy_ca_file"], "/etc/labweaver/secrets/mtls-ca.pem")
        self.assertTrue(any(destination.endswith(":9445") for destination in sandbox["allowed_egress"]))
