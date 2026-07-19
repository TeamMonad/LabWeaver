"""Contract evidence for the router-owned Ansible controller and safety policies."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import sys
import unittest
import yaml


ROOT = Path(__file__).resolve().parents[2]
SAFETY_PATH = ROOT / "deploy/ansible/roles/storage_nodes/files/storage_safety.py"
SPEC = importlib.util.spec_from_file_location("storage_safety", SAFETY_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("storage safety module could not be loaded")
SAFETY = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = SAFETY
SPEC.loader.exec_module(SAFETY)


class AnsibleFixtureTests(unittest.TestCase):
    def test_sprint2_foundation_images_are_immutable(self) -> None:
        lock = yaml.safe_load((ROOT / "deploy/versions.lock.yml").read_text(encoding="utf-8"))
        foundation = lock["sprint2_foundation"]
        images = {key: foundation[key] for key in ("nats", "nats_box", "minio", "buildkit_rootless")}

        for reference in images.values():
            self.assertRegex(reference, r"^[^\s]+@sha256:[0-9a-f]{64}$")
        self.assertRegex(lock["postgresql"]["image"], r"^[^\s]+@sha256:[0-9a-f]{64}$")

    def test_sprint2_administration_tools_are_locked_and_installed_before_foundation(self) -> None:
        lock = yaml.safe_load((ROOT / "deploy/versions.lock.yml").read_text(encoding="utf-8"))
        tools = lock["sprint2_foundation"]["admin_tools"]
        foundation_playbook = (ROOT / "deploy/ansible/playbooks/92-sprint2-foundation.yml").read_text(
            encoding="utf-8"
        )
        tool_playbook = (ROOT / "deploy/ansible/playbooks/91-sprint2-admin-tools.yml").read_text(
            encoding="utf-8"
        )
        tasks = (ROOT / "deploy/ansible/roles/sprint2_admin_tools/tasks/main.yml").read_text(
            encoding="utf-8"
        )

        self.assertEqual(
            set(tools),
            {"nats_cli", "nsc", "minio_client", "buildctl", "keycloak_admin", "system_packages"},
        )
        for name in ("nats_cli", "nsc", "minio_client", "buildctl", "keycloak_admin"):
            self.assertRegex(tools[name]["linux_amd64_sha256"], r"^[0-9a-f]{64}$")
            self.assertTrue(tools[name]["linux_amd64_url"].startswith("https://github.com/"))
        self.assertIn("- import_playbook: 91-sprint2-admin-tools.yml", foundation_playbook)
        self.assertIn("sprint2_admin_tools_profile: authoring", tool_playbook)
        self.assertIn("sprint2_admin_tools_profile: execution", tool_playbook)
        self.assertIn("checksum: \"sha256:", tasks)
        self.assertNotIn("ansible.builtin.shell", tasks)

    def test_sprint2_foundation_is_persistent_and_reset_preserves_it(self) -> None:
        playbook = (ROOT / "deploy/ansible/playbooks/92-sprint2-foundation.yml").read_text(
            encoding="utf-8"
        )
        tasks = (ROOT / "deploy/ansible/roles/sprint2_foundation/tasks/main.yml").read_text(
            encoding="utf-8"
        )
        workloads = (
            ROOT / "deploy/ansible/roles/sprint2_foundation/templates/workloads.yml.j2"
        ).read_text(encoding="utf-8")
        reset = (ROOT / "deploy/ansible/roles/sprint2_reset/defaults/main.yml").read_text(
            encoding="utf-8"
        )

        self.assertIn("labweaver_preflight_scope: sprint2-foundation", playbook)
        self.assertIn("- import_playbook: 91-sprint2-admin-tools.yml", playbook)
        self.assertIn("sprint2-foundation --infra", (
            ROOT / "docs/deployment/ansible.md"
        ).read_text(encoding="utf-8"))
        self.assertIn("SPRINT2_FOUNDATION_BUNDLE_KEYS_INVALID", tasks)
        self.assertIn("Delete exact pod blocked on a superseded failed revision", tasks)
        self.assertIn("item.status.currentRevision != item.status.updateRevision", tasks)
        self.assertIn("kind: StatefulSet", workloads)
        self.assertEqual(workloads.count("kind: StatefulSet"), 3)
        replacements = {
            "{{ sprint2_foundation_namespace }}": "labweaver-data",
            "{{ sprint2_foundation_storage_class }}": "local-path",
            "{{ sprint2_foundation_minio_storage_class }}": "nfs-rwx",
            "{{ sprint2_foundation_postgres_storage }}": "20Gi",
            "{{ sprint2_foundation_nats_storage }}": "10Gi",
            "{{ sprint2_foundation_minio_storage }}": "100Gi",
            "{{ sprint2_foundation_lock.postgresql.image }}": "registry.invalid/postgres@sha256:" + "a" * 64,
            "{{ sprint2_foundation_lock.sprint2_foundation.nats }}": "registry.invalid/nats@sha256:" + "b" * 64,
            "{{ sprint2_foundation_lock.sprint2_foundation.minio }}": "registry.invalid/minio@sha256:" + "c" * 64,
            "{{ sprint2_foundation_bundle_sha256 }}": "d" * 64,
        }
        rendered = workloads
        for source, value in replacements.items():
            rendered = rendered.replace(source, value)
        documents = list(yaml.safe_load_all(rendered))
        self.assertEqual(len(documents), 8)
        self.assertEqual(sum(document["kind"] == "StatefulSet" for document in documents), 3)
        for document in documents:
            if document["kind"] == "StatefulSet":
                self.assertEqual(
                    document["spec"]["template"]["metadata"]["annotations"]
                    ["labweaver.io/configuration-sha256"],
                    "d" * 64,
                )
        minio = next(
            document for document in documents
            if document["kind"] == "StatefulSet" and document["metadata"]["name"] == "minio"
        )
        self.assertEqual(minio["spec"]["template"]["spec"]["securityContext"]["runAsUser"], 65534)
        self.assertIn("pod-security.kubernetes.io/enforce: restricted", tasks)
        reset_namespaces = reset.split("sprint2_reset_domains:", maxsplit=1)[0]
        self.assertNotIn("labweaver-data", reset_namespaces)
        self.assertNotIn("labweaver-build", reset_namespaces)

    def test_sprint2_buildkit_is_rootless_isolated_and_explicitly_exception_bound(self) -> None:
        playbook = (ROOT / "deploy/ansible/playbooks/92-sprint2-buildkit.yml").read_text(
            encoding="utf-8"
        )
        tasks = (ROOT / "deploy/ansible/roles/sprint2_buildkit/tasks/main.yml").read_text(
            encoding="utf-8"
        )
        workloads = (
            ROOT / "deploy/ansible/roles/sprint2_buildkit/templates/workloads.yml.j2"
        ).read_text(encoding="utf-8")
        self.assertIn("labweaver_preflight_scope: sprint2-buildkit", playbook)
        self.assertIn(
            "'sprint2-buildkit'",
            (ROOT / "deploy/ansible/roles/preflight/tasks/main.yml").read_text(
                encoding="utf-8"
            ),
        )
        self.assertIn("SPRINT2_BUILDKIT_BUNDLE_KEYS_INVALID", tasks)
        self.assertIn("SPRINT2_BUILDKIT_READBACK_INVALID", tasks)
        self.assertIn("pod-security.kubernetes.io/enforce: privileged", tasks)
        self.assertIn("runAsNonRoot: true", workloads)
        self.assertIn("privileged: false", workloads)
        self.assertIn("allowPrivilegeEscalation: true", workloads)
        self.assertIn("capabilities: {drop: [ALL], add: [SETUID, SETGID]}", workloads)
        self.assertIn("- --tlsservername\n                - buildkit", workloads)
        self.assertIn("seccompProfile: {type: Unconfined}", workloads)
        self.assertIn("appArmorProfile: {type: Unconfined}", workloads)
        self.assertIn("automountServiceAccountToken: false", workloads)
        self.assertNotIn("hostPath:", workloads)
        self.assertNotIn("hostNetwork: true", workloads)
        self.assertIn("--oci-worker-no-process-sandbox", workloads)
        self.assertIn("labweaver.io/configuration-sha256: {{ sprint2_buildkit_bundle_sha256 }}", workloads)
        self.assertIn("mountPath: /home/user/.local/tmp", workloads)
        self.assertIn("name: TMPDIR, value: /tmp", workloads)
        self.assertIn("default-deny", workloads)
        self.assertIn("buildctl", workloads)
        self.assertIn("sprint2_buildkit_registry_cidr", tasks)
        self.assertIn("sprint2-buildkit --infra", (
            ROOT / "docs/deployment/ansible.md"
        ).read_text(encoding="utf-8"))
        self.assertIn("Sprint2Buildkit", (
            ROOT / "xtask/src/main.rs"
        ).read_text(encoding="utf-8"))

    def test_controller_execution_is_router_owned(self) -> None:
        docs = (ROOT / "docs/deployment/ansible.md").read_text(encoding="utf-8")
        controller_lock = (ROOT / "deploy/ansible/controller.lock.yml").read_text(encoding="utf-8")
        xtask = (ROOT / "xtask/src/main.rs").read_text(encoding="utf-8")
        self.assertIn("cargo xtask deploy --infra", docs)
        self.assertIn("ansible-rs", docs)
        self.assertNotIn("tools/ansible.py", docs)
        self.assertIn("approved_controller_ids: edge-router,wsl-a-controller", controller_lock)
        self.assertIn("python_kubernetes_version: 34.1.0", controller_lock)
        self.assertIn("require_python_module_version", xtask)
        self.assertIn("inventory_identity_hash(&inventory_root)", xtask)

    def test_deploy_starts_with_preflight(self) -> None:
        site = (ROOT / "deploy/ansible/playbooks/site.yml").read_text(encoding="utf-8")
        self.assertEqual(site.splitlines()[1], "- import_playbook: 00-preflight.yml")

    def test_verify_uses_manual_vm_lifecycle_and_fails_on_cleanup(self) -> None:
        verify = (ROOT / "deploy/ansible/roles/verify/tasks/main.yml").read_text(encoding="utf-8")
        self.assertIn("verify_runtime_namespace", verify)
        self.assertIn("virtctl -n {{ verify_runtime_namespace }} start kvm-probe", verify)
        self.assertIn("virtctl -n {{ verify_runtime_namespace }} stop kvm-probe", verify)
        self.assertNotIn("patch virtualmachine/kvm-probe", verify)
        self.assertIn("verify_cleanup_failed", verify)
        self.assertIn("CILIUM_CLEANUP_FAILED", verify)

    def test_gateway_testflight_resources_are_run_scoped(self) -> None:
        verify = (ROOT / "deploy/ansible/roles/verify/tasks/main.yml").read_text(encoding="utf-8")
        probes = (ROOT / "deploy/ansible/roles/verify/templates/runtime-probes.yml.j2").read_text(encoding="utf-8")
        self.assertIn("verify_gateway_backend_name", verify)
        self.assertIn("labweaver.io/testflight-run={{ verify_testflight_run_id }}", verify)
        self.assertIn("name: {{ verify_gateway_backend_name }}", probes)
        self.assertIn("labweaver.io/testflight-run", probes)

    def test_harbor_reconcile_requires_bound_backup_and_pinned_artifacts(self) -> None:
        playbook = (ROOT / "deploy/ansible/playbooks/95-harbor.yml").read_text(encoding="utf-8")
        harbor = (ROOT / "deploy/ansible/roles/harbor/tasks/main.yml").read_text(encoding="utf-8")
        lock = (ROOT / "deploy/versions.lock.yml").read_text(encoding="utf-8")
        self.assertIn("roles: [backup, harbor]", playbook)
        self.assertIn("HARBOR_BACKUP_EVIDENCE_INVALID", harbor)
        self.assertIn("HARBOR_CHART_ARCHIVE_IDENTITY_INVALID", harbor)
        self.assertIn("database_permissions", harbor)
        self.assertIn("harbor_component_resources", harbor)
        self.assertIn("chart_archive_sha256", lock)
        self.assertNotIn("busybox:1.36", lock)

    def test_identity_foundation_is_pinned_private_and_fail_closed(self) -> None:
        deploy = (ROOT / "deploy/ansible/playbooks/91-identity-foundation.yml").read_text(
            encoding="utf-8"
        )
        tasks = (ROOT / "deploy/ansible/roles/identity_foundation/tasks/main.yml").read_text(
            encoding="utf-8"
        )
        workloads = (
            ROOT / "deploy/ansible/roles/identity_foundation/templates/workloads.yml.j2"
        ).read_text(encoding="utf-8")
        provision_job = (
            ROOT / "deploy/ansible/roles/identity_foundation/templates/provision-job.yml.j2"
        ).read_text(encoding="utf-8")
        operator_rbac = (
            ROOT / "deploy/ansible/roles/identity_foundation/templates/operator-rbac.yml.j2"
        ).read_text(encoding="utf-8")
        pki = (ROOT / "deploy/ansible/roles/identity_foundation/templates/pki.yml.j2").read_text(
            encoding="utf-8"
        )
        lock = (ROOT / "deploy/versions.lock.yml").read_text(encoding="utf-8")
        self.assertEqual(deploy.splitlines()[1], "- import_playbook: 00-preflight.yml")
        self.assertIn("labweaver_preflight_scope: identity-foundation", deploy)
        self.assertIn("IDENTITY_SECRET_LOCATOR_INVALID", tasks)
        self.assertIn("IDENTITY_GATEWAY_VIP_CONFLICT", tasks)
        self.assertIn("IDENTITY_DNS_CONFLICT", tasks)
        self.assertIn("no_log: true", tasks)
        self.assertIn("kind: Issuer", pki)
        self.assertNotIn("kind: ClusterIssuer", pki)
        self.assertIn("rotationPolicy: Never", pki)
        self.assertIn("replicas: {{ identity_keycloak_replicas }}", workloads)
        self.assertIn("standardFlowEnabled=false", provision_job)
        self.assertIn("directAccessGrantsEnabled=false", provision_job)
        self.assertIn("keycloak: docker.io/keycloak/keycloak:26.7.0@sha256:", lock)
        self.assertIn("postgres: docker.io/library/postgres:17.6-alpine@sha256:", lock)
        self.assertIn("python_kubernetes_rpm: python3-kubernetes-34.1.0-2.el10_2", lock)
        self.assertIn("identity_lock.python_kubernetes_rpm", tasks)
        self.assertIn("runAsUser: 70, runAsGroup: 70", workloads)
        self.assertIn("runAsUser: 1000", workloads)
        self.assertIn("oidc-audience-mapper", provision_job)
        self.assertIn("metallb.io/loadBalancerIPs", (
            ROOT / "deploy/ansible/roles/identity_foundation/templates/gateway.yml.j2"
        ).read_text(encoding="utf-8"))
        self.assertIn("fromEntities: [ingress]", (
            ROOT / "deploy/ansible/roles/identity_foundation/templates/policy.yml.j2"
        ).read_text(encoding="utf-8"))
        self.assertIn("IDENTITY_TOKEN_CLAIMS_INVALID", tasks)
        self.assertIn("service-account-{{ identity_workload_client_id }}", tasks)
        self.assertIn("automountServiceAccountToken: false", operator_rbac)
        self.assertIn("name: labweaver-cluster-observer", operator_rbac)
        self.assertIn("resources: [jobs, cronjobs]", operator_rbac)
        self.assertNotIn("private-sigstore", operator_rbac)
        self.assertIn("name: labweaver-devops-admin", operator_rbac)
        self.assertNotIn("sigstore-system", operator_rbac)
        self.assertNotIn("resources: [secrets", operator_rbac)
        self.assertNotIn(":latest", workloads)

    def test_testflight_report_requires_deployment_identity_chain(self) -> None:
        schema = json.loads(
            (ROOT / "schemas/infrastructure/infrastructure-testflight-report.v1.schema.json").read_text(
                encoding="utf-8"
            )
        )
        for field in (
            "commit_sha", "inventory_hash", "component_lock_hash",
            "harbor_policy_manifest_hash", "deployment_manifest_hash",
            "deployment_manifest_locator",
        ):
            self.assertIn(field, schema["required"])

    def test_baseline_testflight_defers_identity_governance(self) -> None:
        verify = (ROOT / "deploy/ansible/roles/verify/tasks/main.yml").read_text(encoding="utf-8")
        schema = json.loads(
            (ROOT / "schemas/infrastructure/infrastructure-testflight-report.v1.schema.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertIn("adopted-cluster-baseline", verify)
        self.assertIn("deferred-to-issue-47", verify)
        self.assertIn("deferred-to-issue-2", verify)
        self.assertIn("deferred", schema["properties"]["checks"]["items"]["properties"]["status"]["enum"])

    def test_storage_safety_rejects_dangerous_devices(self) -> None:
        safe = {"path": "/dev/test", "type": "disk", "fstype": None, "wwn": "fixture-wwn", "size": 1073741824, "pkname": None, "mountpoints": [None]}
        result = SAFETY.validate([safe], "/dev/test", "fixture-wwn", 1073741824, "/dev/root", [])
        self.assertTrue(result["safe_to_format"])
        for changed in (
            {**safe, "type": "part"}, {**safe, "wwn": "wrong"}, {**safe, "size": 1},
            {**safe, "mountpoints": ["/data"]}, {**safe, "children": [{"path": "/dev/test1"}], "pkname": None},
        ):
            with self.assertRaises(SAFETY.UnsafeStorage):
                SAFETY.validate([changed], "/dev/test", "fixture-wwn", 1073741824, "/dev/root", [])
        with self.assertRaises(SAFETY.UnsafeStorage):
            SAFETY.validate([safe], "/dev/test", "fixture-wwn", 1073741824, "/dev/root", ["holder"])

    def test_sprint2_reset_is_identity_bound_and_fail_closed(self) -> None:
        playbook = (ROOT / "deploy/ansible/playbooks/93-sprint2-reset.yml").read_text(
            encoding="utf-8"
        )
        tasks = (ROOT / "deploy/ansible/roles/sprint2_reset/tasks/main.yml").read_text(
            encoding="utf-8"
        )
        baseline = (
            ROOT / "deploy/ansible/roles/sprint2_reset/templates/baseline.sql.j2"
        ).read_text(encoding="utf-8")
        xtask = (ROOT / "xtask/src/main.rs").read_text(encoding="utf-8")
        self.assertEqual(playbook.splitlines()[1], "- import_playbook: 00-preflight.yml")
        self.assertIn("labweaver_preflight_scope: sprint2-reset", playbook)
        self.assertIn("destroy-pre-release-data:", tasks)
        self.assertIn("sprint2_reset_cluster_uid", tasks)
        self.assertIn("KYVERNO_EXTERNAL_DEPENDENCY_DETECTED", tasks)
        destructive_action = tasks.index("Uninstall the historical Private Sigstore release")
        for dependency_probe in (
            "Probe PostgreSQL before any destructive action",
            "Probe JetStream before any destructive action",
            "Probe MinIO before any destructive action",
            "Probe BuildKit before any destructive action",
            "Probe Harbor before any destructive action",
            "Authenticate Keycloak administration before any destructive action",
        ):
            self.assertLess(tasks.index(dependency_probe), destructive_action)
        self.assertLess(
            tasks.index("KYVERNO_EXTERNAL_DEPENDENCY_DETECTED"),
            tasks.index("Uninstall the Kyverno release"),
        )
        self.assertIn("KYVERNO_ADMISSION_WEBHOOK_REMAINS", tasks)
        self.assertIn("Deploy the identical Sprint 2 profile a second time", tasks)
        self.assertIn("Exercise atomic rollback with reviewed invalid readiness values", tasks)
        self.assertIn("SPRINT2_ATOMIC_ROLLBACK_FAILED", tasks)
        self.assertIn("Delete exact residual Kyverno CRDs", tasks)
        self.assertIn("Require the exact Sprint 2 deployment set", tasks)
        self.assertIn("sprint2_reset_migration_catalog_stat", tasks)
        self.assertIn("sprint2_reset_inventory_stat", tasks)
        self.assertIn("SPRINT2_CONFIGURATION_BUNDLE_INVALID", tasks)
        self.assertIn("Apply the reviewed workload configuration bundle", tasks)
        self.assertIn("kubernetes.core.k8s", tasks)
        self.assertNotIn("configuration_bundle", baseline)
        self.assertIn("DROP SCHEMA IF EXISTS", baseline)
        self.assertIn("0001_sprint2_baseline.sql", baseline)
        self.assertEqual(baseline.count("0001_roles_and_schemas.sql"), 2)
        self.assertIn("SET ROLE lw_{{ domain }}_owner", baseline)
        self.assertIn("schema_migrations", baseline)
        self.assertIn("catalog_sha256", baseline)
        self.assertIn('run_infrastructure(&args.env, "93-sprint2-reset.yml"', xtask)
        self.assertNotIn("ansible.builtin.shell", tasks)


if __name__ == "__main__":
    unittest.main()
