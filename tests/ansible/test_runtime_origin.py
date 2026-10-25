"""Render the real chart and reject unsafe or incomplete runtime publication."""
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[2]


class RuntimeOriginTests(unittest.TestCase):
    def render(self, base_hostname="runtime.example.test", tls="runtime-tls", tls_hostname=None):
        helm = shutil.which("helm")
        self.assertIsNotNone(helm, "The deployment gate requires the pinned Helm renderer")
        values = yaml.safe_load((ROOT / "deploy/helm/labweaver/values.yaml").read_text())
        values["portalRoute"].update(enabled=True, namespace="labweaver-system", hostname="portal.example.test", gatewayName="portal")
        values["runtimeRoute"].update(baseHostname=base_hostname, tlsSecretName=tls,
                                      tlsHostname=f"*.{base_hostname}" if tls_hostname is None else tls_hostname)
        # This test renders just ingress, without requiring unrelated workload images.
        values["workloads"] = {}
        with tempfile.TemporaryDirectory() as directory:
            chart = Path(directory) / "chart"
            (chart / "templates").mkdir(parents=True)
            shutil.copy(ROOT / "deploy/helm/labweaver/Chart.yaml", chart / "Chart.yaml")
            for name in ("portal-route.yaml", "runtime-route.yaml", "_helpers.tpl"):
                shutil.copy(ROOT / "deploy/helm/labweaver/templates" / name, chart / "templates" / name)
            config = Path(directory) / "values.yaml"
            config.write_text(yaml.safe_dump(values))
            return subprocess.run([helm, "template", "runtime-test", str(chart), "--values", str(config)], text=True, capture_output=True)

    def test_missing_runtime_base_hostname_fails_render(self):
        result = self.render(base_hostname="")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("runtimeRoute.baseHostname is required", result.stderr)

    def test_missing_runtime_tls_fails_render(self):
        result = self.render(tls="")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("runtimeRoute.tlsSecretName is required", result.stderr)

    def test_portal_hostname_reuse_fails_render(self):
        result = self.render(base_hostname="portal.example.test")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must differ", result.stderr)

    def test_runtime_route_only_publishes_runtime_paths_over_tls(self):
        result = self.render()
        self.assertEqual(result.returncode, 0, result.stderr)
        objects = list(yaml.safe_load_all(result.stdout))
        gateway = next(obj for obj in objects if obj["kind"] == "Gateway")
        listener, = gateway["spec"]["listeners"]
        self.assertEqual(listener["protocol"], "HTTPS")
        self.assertEqual(listener["hostname"], "*.runtime.example.test")
        self.assertEqual(listener["tls"]["certificateRefs"][0]["name"], "runtime-tls")
        route = next(obj for obj in objects if obj["kind"] == "HTTPRoute" and obj["metadata"]["name"].endswith("-runtime"))
        self.assertEqual(route["spec"]["hostnames"], ["*.runtime.example.test"])
        paths = {match["path"]["value"] for rule in route["spec"]["rules"] for match in rule["matches"]}
        self.assertEqual(paths, {"/connect", "/runtime-bootstrap", "/runtime-handoff"})
        self.assertNotIn("/api", paths)
        self.assertNotIn("/auth", paths)

    def test_missing_wildcard_tls_hostname_fails_render(self):
        result = self.render(tls_hostname="")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("runtimeRoute.tlsHostname wildcard is required", result.stderr)

    def test_single_host_certificate_is_not_a_wildcard(self):
        result = self.render(tls_hostname="runtime.example.test")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must cover exactly", result.stderr)

    def test_portal_cannot_live_beneath_the_runtime_wildcard(self):
        result = self.render(base_hostname="example.test")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("outside the runtime base zone", result.stderr)
