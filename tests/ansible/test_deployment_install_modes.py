"""Exercise exact deployment boundaries with synthetic Kubernetes/Helm observations.

The optional LW_DEPLOY_COUNTERCHECK mutates only the in-memory source used by the
named test. It proves that removing each security check turns that test red,
without touching the production tree or connecting to a cluster.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

from jinja2 import Environment
import yaml

ROOT = Path(__file__).resolve().parents[2]
INSTALLATION = "019fa9d0-0000-7000-8000-000000000142"


def tasks(path: str) -> list[dict]:
    return yaml.safe_load((ROOT / path).read_text())


def named(entries: list[dict], name: str) -> dict:
    return next(task for task in entries if task.get("name") == name)


def run_assertion(task: dict, variables: dict) -> subprocess.CompletedProcess[str]:
    with tempfile.TemporaryDirectory() as directory:
        playbook = Path(directory) / "assert.yml"
        playbook.write_text(yaml.safe_dump([{
            "name": "Check external observation without mutating Kubernetes",
            "hosts": "localhost",
            "connection": "local",
            "gather_facts": False,
            "tasks": [task],
            "vars": variables,
        }]))
        return subprocess.run(
            ["ansible-playbook", "-i", "localhost,", str(playbook)],
            capture_output=True, text=True, check=False,
        )


def sample_object(*, release: str, namespace: str, installation: str = "", managed: str = "Helm") -> dict:
    return {"kind": "ConfigMap", "metadata": {
        "name": "labweaver-object-store-ca",
        "labels": {"app.kubernetes.io/part-of": "labweaver", "app.kubernetes.io/managed-by": managed},
        "annotations": {
            "meta.helm.sh/release-name": release,
            "meta.helm.sh/release-namespace": namespace,
            "labweaver.io/installation-id": installation,
        },
    }}


class DeploymentModeTests(unittest.TestCase):
    def test_bundle_variables_precede_target_probes_and_use_nondefault_target(self) -> None:
        for role, prefix in (("platform_application", "platform_application"), ("resource_application", "resource_application")):
            with self.subTest(role=role):
                source = tasks(f"deploy/ansible/roles/{role}/tasks/main.yml")
                if os.getenv("LW_DEPLOY_COUNTERCHECK") == "order":
                    source = source[1:] + source[:1]
                self.assertLess(next(i for i, t in enumerate(source) if "include_vars" in " ".join(t)),
                                next(i for i, t in enumerate(source) if "existing" in t.get("name", "").lower() and "namespace" in t.get("name", "").lower()))
                self.assertLess(next(i for i, t in enumerate(source) if t.get("name", "").startswith("Apply reviewed private") or t.get("name", "").startswith("Load the immutable application identities")),
                                next(i for i, t in enumerate(source) if "existing" in t.get("name", "").lower() and "namespace" in t.get("name", "").lower()))
                probe = next(t for t in source if "existing" in t.get("name", "").lower() and "namespace" in t.get("name", "").lower())
                self.assertEqual(Environment().from_string(probe["kubernetes.core.k8s_info"]["name"]).render({f"{prefix}_namespace": "science-sandbox"}), "science-sandbox")
                self.assertIn("{{ " + prefix + "_release_name }}", (ROOT / f"deploy/ansible/roles/{role}/tasks/main.yml").read_text())

    def test_object_probe_checks_both_helm_annotations_not_just_label(self) -> None:
        for role, prefix in (("platform_application", "platform_application"), ("resource_application", "resource_application")):
            with self.subTest(role=role):
                source = tasks(f"deploy/ansible/roles/{role}/tasks/main.yml")
                probe = next(t for t in source if t.get("name", "").startswith("Refuse to adopt unowned") and "objects" in t["name"].lower())
                command = Environment().from_string(probe["ansible.builtin.shell"]["cmd"]).render({
                    f"{prefix}_kubectl": "/usr/bin/kubectl",
                    f"{prefix}_kubeconfig": "/tmp/synthetic-kubeconfig",
                    f"{prefix}_namespace": "science-sandbox",
                    f"{prefix}_release_name": "science-labweaver",
                    f"{prefix}_installation_id": INSTALLATION,
                })
                program = command.split("jq -e ", 1)[1]
                program = program.strip().strip("'")
                if os.getenv("LW_DEPLOY_COUNTERCHECK") == "objects":
                    program = '[.items[] | select((.metadata.labels["app.kubernetes.io/part-of"] // "") != "labweaver")] | length == 0'
                for instance, accepted in (
                    (sample_object(release="foreign-release", namespace="science-sandbox"), False),
                    (sample_object(release="science-labweaver", namespace="foreign-namespace"), False),
                    (sample_object(release="science-labweaver", namespace="science-sandbox"), True),
                ):
                    result = subprocess.run(["jq", "-e", program], input=json.dumps({"items": [instance]}), text=True, capture_output=True, check=False)
                    self.assertEqual(result.returncode == 0, accepted, result.stderr)

    def test_xtask_refuses_mode_without_bundle_and_legacy_infra_entry(self) -> None:
        binary = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")) / "debug/xtask"
        environment = os.environ.copy()
        for key in ("LW_BUNDLE_ROOT", "LW_BUNDLE_RENDERED_ROOT", "LW_BUNDLE_INSTALLATION_ID"):
            environment.pop(key, None)
        for args, diagnostic in (
            (["--mode", "install"], "authenticated deployment Bundle source"),
            ([], "--mode install, adopt, upgrade, or wipe"),
        ):
            with self.subTest(args=args):
                result = subprocess.run(
                    [str(binary), "deploy", "--env", "demo", "--infra", "--yes", *args],
                    env=environment, capture_output=True, text=True, check=False,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(diagnostic, result.stderr)

    def test_release_probe_requires_immutable_installation_and_fails_closed(self) -> None:
        for role, prefix in (("platform_application", "platform_application"), ("resource_application", "resource_application")):
            with self.subTest(role=role):
                source = tasks(f"deploy/ansible/roles/{role}/tasks/main.yml")
                listing = next(t for t in source if t.get("name", "").startswith("List the exact"))
                metadata = next(t for t in source if t.get("name", "").startswith("Read existing") and "Helm release" in t["name"])
                self.assertNotIn("failed_when", listing)
                self.assertNotIn("failed_when", metadata)
                identity = next(t for t in source if t.get("name", "").startswith("Refuse a foreign"))
                if os.getenv("LW_DEPLOY_COUNTERCHECK") == "release":
                    identity["ansible.builtin.assert"]["that"][-1] = f"({prefix}_release_metadata.stdout | from_json).chart is search('^labweaver')"
                release_list = {"stdout": json.dumps([{"name": "science-labweaver"}])}
                foreign = {"stdout": json.dumps({"name": "science-labweaver", "namespace": "science-sandbox", "description": "labweaver-installation:foreign", "chart": "labweaver-1.0"})}
                variables = {f"{prefix}_release_list": release_list, f"{prefix}_release_metadata": foreign,
                             f"{prefix}_release_name": "science-labweaver", f"{prefix}_namespace": "science-sandbox", f"{prefix}_installation_id": INSTALLATION}
                rejected = run_assertion(identity, variables)
                self.assertNotEqual(rejected.returncode, 0, rejected.stdout + rejected.stderr)
                foreign["stdout"] = json.dumps({"name": "science-labweaver", "namespace": "science-sandbox", "description": "labweaver-installation:" + INSTALLATION})
                accepted = run_assertion(identity, variables)
                self.assertEqual(accepted.returncode, 0, accepted.stdout + accepted.stderr)
                variables[f"{prefix}_release_list"] = {"stdout": "[]"}
                variables.pop(f"{prefix}_release_metadata")
                absent = run_assertion(identity, variables)
                self.assertEqual(absent.returncode, 0, absent.stdout + absent.stderr)

    def test_foundation_and_buildkit_inspect_every_planned_object(self) -> None:
        for role, prefix in (("platform_foundation", "platform_foundation"), ("platform_buildkit", "platform_buildkit")):
            with self.subTest(role=role):
                source = tasks(f"deploy/ansible/roles/{role}/tasks/main.yml")
                inspect = next(t for t in source if t.get("name", "").startswith("Inspect every planned"))
                refuse = next(t for t in source if t.get("name", "").startswith("Refuse to overwrite any unowned"))
                self.assertLess(source.index(inspect), next(i for i, t in enumerate(source) if t.get("name", "").startswith("Reconcile the") and "namespace" in t["name"]))
                self.assertIn(prefix + "_objects", inspect["loop"])
                self.assertIn("workloads.yml.j2", inspect["loop"])
                if os.getenv("LW_DEPLOY_COUNTERCHECK") == "foundation":
                    refuse["ansible.builtin.assert"]["that"] = ["True"]
                foreign = {"item": {"kind": "Deployment", "metadata": {"name": "buildkit"}}, "resources": [{"metadata": {"annotations": {"labweaver.io/installation-id": "foreign"}}}]}
                variables = {"item": foreign, "labweaver_installation_id": INSTALLATION,
                             f"{prefix}_object_ownership": {"results": [foreign]}}
                self.assertNotEqual(run_assertion(refuse, variables).returncode, 0)
                foreign["resources"][0]["metadata"]["annotations"]["labweaver.io/installation-id"] = INSTALLATION
                self.assertEqual(run_assertion(refuse, variables).returncode, 0)

    def test_foundation_recovery_rejects_foreign_controllers_and_pods(self) -> None:
        source = tasks("deploy/ansible/roles/platform_foundation/tasks/main.yml")
        probe = named(source, "Read exact recovery Pods for installation-owned StatefulSets")
        conditions = probe["when"]
        if os.getenv("LW_DEPLOY_COUNTERCHECK") == "recovery":
            conditions = conditions[2:]
        for name, installation, accepted in (("nats", INSTALLATION, True), ("independent", INSTALLATION, False), ("nats", "foreign", False)):
            controller = {"metadata": {"name": name, "annotations": {"labweaver.io/installation-id": installation}},
                          "status": {"currentRevision": "old", "updateRevision": "new", "readyReplicas": 0}, "spec": {"replicas": 1}}
            selected = all(Environment().compile_expression(condition)(item=controller, labweaver_installation_id=INSTALLATION) for condition in conditions)
            self.assertEqual(selected, accepted)
        guard = named(source, "Refuse recovery of Pods outside their installation-owned controller")
        for owner_uid, accepted in (("owned-controller", True), ("foreign-controller", False)):
            observed = {"item": {"metadata": {"uid": "owned-controller"}},
                        "resources": [{"metadata": {"name": "nats-0", "uid": "observed-pod", "ownerReferences": [{"uid": owner_uid}]}}]}
            result = run_assertion(guard, {"platform_foundation_recovery_pods": {"results": [observed]}})
            self.assertEqual(result.returncode == 0, accepted, result.stdout + result.stderr)
        deletion = named(source, "Delete exact pod blocked on a superseded failed revision")["kubernetes.core.k8s"]
        self.assertEqual(deletion["delete_options"]["preconditions"]["uid"], "{{ item.resources[0].metadata.uid }}")
        self.assertEqual(deletion["name"], "{{ item.resources[0].metadata.name }}")

    def test_wipe_never_selects_foreign_environment_or_unowned_volume(self) -> None:
        source = tasks("deploy/ansible/playbooks/94-platform-clean-redeploy.yml")[-1]["tasks"]
        select = named(source, "Select only installation-owned environment namespaces")
        program = select["ansible.builtin.command"]["argv"][-1]
        if os.getenv("LW_DEPLOY_COUNTERCHECK") == "wipe":
            program = '[.[] | select(.metadata.name | startswith("lw-env-")) | .metadata.name]'
        inventory = [
            {"metadata": {"name": "lw-env-foreign", "annotations": {"labweaver.io/installation-id": "foreign"}}},
            {"metadata": {"name": "lw-env-owned", "annotations": {"labweaver.io/installation-id": INSTALLATION}}},
        ]
        result = subprocess.run(["jq", "-c", "--arg", "installation", INSTALLATION, program], input=json.dumps(inventory), capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), ["lw-env-owned"])
        check = named(source, "Reject unowned volumes in the destructive namespace scope")
        foreign_pv = {"metadata": {"name": "foreign-volume", "annotations": {}}, "spec": {"claimRef": {"namespace": "labweaver-system"}}}
        variables = {"item": foreign_pv, "labweaver_installation_id": INSTALLATION,
                     "platform_clean_redeploy_owned_namespaces": ["labweaver-system"],
                     "platform_clean_redeploy_persistent_volumes": {"resources": [foreign_pv]}}
        self.assertNotEqual(run_assertion(check, variables).returncode, 0)
        inventory_check = named(source, "Reject namespaces containing unowned resources")
        inventory_program = inventory_check["ansible.builtin.command"]["argv"][-1]
        for object_installation, accepted in (("foreign", False), (INSTALLATION, True)):
            candidate = sample_object(release="labweaver", namespace="labweaver-system",
                                      installation=object_installation, managed="outside")
            checked = subprocess.run(
                ["jq", "-e", "--arg", "installation", INSTALLATION,
                 "--arg", "scope", "labweaver-system", "--argjson", "releases", '["labweaver"]', inventory_program],
                input=json.dumps({"items": [candidate]}), capture_output=True, text=True, check=False,
            )
            self.assertEqual(checked.returncode == 0, accepted, checked.stderr)
        owner = sample_object(release="labweaver", namespace="labweaver-system",
                              installation=INSTALLATION, managed="ansible")
        owner["metadata"]["uid"] = "owned-uid"
        child = {"kind": "Pod", "metadata": {"name": "controller-child", "ownerReferences": [{"uid": "owned-uid"}]}}
        for parent_uid, accepted in (("owned-uid", True), ("foreign-uid", False)):
            child["metadata"]["ownerReferences"][0]["uid"] = parent_uid
            checked = subprocess.run(
                ["jq", "-e", "--arg", "installation", INSTALLATION,
                 "--arg", "scope", "labweaver-system", "--argjson", "releases", '["labweaver"]', inventory_program],
                input=json.dumps({"items": [owner, child]}), capture_output=True, text=True, check=False,
            )
            self.assertEqual(checked.returncode == 0, accepted, checked.stderr)


    def test_wipe_runs_full_preflight_before_destructive_playbook(self) -> None:
        source = (ROOT / "xtask/src/main.rs").read_text()
        plan = source.split("fn deployment_plan(", 1)[1].split("fn execute_deployment_plan(", 1)[0]
        wipe = source.split("if mode == DeploymentMode::Wipe {", 1)[1].split("let package_manifest =", 1)[0]
        if os.getenv("LW_DEPLOY_COUNTERCHECK") == "preflight":
            plan = plan.replace('let mut plan = vec![("00-preflight.yml", false)];', "let mut plan = Vec::new();")
        self.assertIn('let mut plan = vec![("00-preflight.yml", false)];', plan)
        self.assertLess(plan.index('"00-preflight.yml"'), plan.index('"94-platform-clean-redeploy.yml"'))
        self.assertIn("execute_deployment_plan(mode, args.stage", wipe)
        playbook = (ROOT / "deploy/ansible/playbooks/94-platform-clean-redeploy.yml").read_text()
        self.assertIn("- import_playbook: 00-preflight.yml", playbook)
        self.assertNotIn("labweaver_preflight_validate_remote_hosts: false", playbook)


if __name__ == "__main__":
    unittest.main()