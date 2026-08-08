"""Static safety contract for the #126 console control-channel fault."""

from __future__ import annotations

from pathlib import Path
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[2]
PLAYBOOK = ROOT / "deploy/ansible/playbooks/98-connected-console-control-loss.yml"
CASE_TASKS = ROOT / "deploy/ansible/tasks/connected-console-control-loss-case.yml"


class ConnectedConsoleControlLossTests(unittest.TestCase):
    def test_playbook_requires_isolated_exact_identity(self) -> None:
        plays = yaml.safe_load(PLAYBOOK.read_text(encoding="utf-8"))
        rendered = PLAYBOOK.read_text(encoding="utf-8")
        self.assertEqual(plays[0]["hosts"], "control_plane[0]")
        self.assertIn("console_acceptance_namespace != 'labweaver-system'", rendered)
        self.assertIn("app.kubernetes.io/name=access-service", rendered)
        self.assertIn("console_access_pods.resources | length == 1", rendered)
        self.assertIn("metadata.uid == console_access_pod_uid", rendered)
        self.assertIn("configuration-bundle-sha256", rendered)

    def test_fault_is_port_scoped_and_always_removed_with_readback(self) -> None:
        tasks = yaml.safe_load(CASE_TASKS.read_text(encoding="utf-8"))
        fault = next(task for task in tasks if "always" in task)
        policy = fault["block"][0]["kubernetes.core.k8s"]["definition"]
        self.assertEqual(policy["kind"], "CiliumNetworkPolicy")
        self.assertNotIn("ingress", policy["spec"])
        self.assertEqual(
            policy["spec"]["endpointSelector"]["matchLabels"],
            {"app.kubernetes.io/name": "access-service"},
        )
        self.assertEqual(
            policy["spec"]["egressDeny"][0]["toPorts"][0]["ports"],
            [{"port": "4222", "protocol": "TCP"}],
        )
        always_text = yaml.safe_dump(fault["always"])
        self.assertIn("state: absent", always_text)
        self.assertIn("kubernetes.core.k8s_info", always_text)
        self.assertIn("LW_CONSOLE_CONTROL_LOSS_CLEANUP_REMAINING", always_text)
        self.assertNotIn("ansible.builtin.shell", CASE_TASKS.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
