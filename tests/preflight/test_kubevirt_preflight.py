import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path

MODULE = Path(__file__).parents[2] / "scripts" / "preflight" / "kubevirt_preflight.py"
SPEC = importlib.util.spec_from_file_location("vm01a", MODULE)
vm01a = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
sys.modules[SPEC.name] = vm01a
SPEC.loader.exec_module(vm01a)


class KubevirtPreflightTests(unittest.TestCase):
    def test_sanitizes_sensitive_values(self):
        text = vm01a.sanitize("token=abc 10.2.3.4 /home/operator/file")
        self.assertNotIn("abc", text)
        self.assertNotIn("10.2.3.4", text)
        self.assertNotIn("/home/operator", text)

    def test_e3_rejects_wrong_issue_without_running_commands(self):
        with tempfile.TemporaryDirectory() as directory:
            result = vm01a.main(["--mode", "e3", "--run-id", "run1", "--issue", "99", "--workload-image", "x@sha256:" + "0" * 64, "--evidence", str(Path(directory) / "report.json")])
        self.assertEqual(result, 1)

    def test_e3_requires_digest_for_workload_image(self):
        with tempfile.TemporaryDirectory() as directory:
            result = vm01a.main(["--mode", "e3", "--run-id", "run1", "--workload-image", "busybox:latest", "--evidence", str(Path(directory) / "report.json")])
        self.assertEqual(result, 1)

    def test_e3_requires_gateway_request_inputs(self):
        with tempfile.TemporaryDirectory() as directory:
            result = vm01a.main(["--mode", "e3", "--run-id", "run1", "--workload-image", "x@sha256:" + "0" * 64, "--evidence", str(Path(directory) / "report.json")])
        self.assertEqual(result, 1)

    def test_rejects_namespace_outside_run_scope(self):
        with tempfile.TemporaryDirectory() as directory:
            result = vm01a.main(["--mode", "readonly", "--run-id", "run1", "--namespace", "default", "--evidence", str(Path(directory) / "report.json")])
        self.assertEqual(result, 1)


if __name__ == "__main__":
    unittest.main()
