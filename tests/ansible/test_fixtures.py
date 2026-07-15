"""Contract evidence for the router-owned Ansible controller and safety policies."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SAFETY_PATH = ROOT / "deploy/ansible/roles/storage_nodes/files/storage_safety.py"
SPEC = importlib.util.spec_from_file_location("storage_safety", SAFETY_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("storage safety module could not be loaded")
SAFETY = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = SAFETY
SPEC.loader.exec_module(SAFETY)


class AnsibleFixtureTests(unittest.TestCase):
    def test_controller_execution_is_router_owned(self) -> None:
        docs = (ROOT / "docs/deployment/ansible.md").read_text(encoding="utf-8")
        controller_lock = (ROOT / "deploy/ansible/controller.lock.yml").read_text(encoding="utf-8")
        xtask = (ROOT / "xtask/src/main.rs").read_text(encoding="utf-8")
        self.assertIn("cargo xtask deploy --infra", docs)
        self.assertIn("ansible-rs", docs)
        self.assertNotIn("tools/ansible.py", docs)
        self.assertIn("approved_controller_ids: edge-router,wsl-a-controller", controller_lock)
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

    def test_private_sigstore_is_private_pinned_and_backup_guarded(self) -> None:
        playbook = (ROOT / "deploy/ansible/playbooks/96-private-sigstore.yml").read_text(
            encoding="utf-8"
        )
        tasks = (ROOT / "deploy/ansible/roles/private_sigstore/tasks/main.yml").read_text(
            encoding="utf-8"
        )
        values = (ROOT / "deploy/ansible/roles/private_sigstore/templates/values.yml.j2").read_text(
            encoding="utf-8"
        )
        policy = (ROOT / "deploy/ansible/roles/private_sigstore/templates/policy.yml.j2").read_text(
            encoding="utf-8"
        )
        gateway = (ROOT / "deploy/ansible/roles/private_sigstore/templates/gateway.yml.j2").read_text(
            encoding="utf-8"
        )
        tuf_static = (
            ROOT / "deploy/ansible/roles/private_sigstore/templates/tuf-static.yml.j2"
        ).read_text(encoding="utf-8")
        lock = (ROOT / "deploy/versions.lock.yml").read_text(encoding="utf-8")

        self.assertEqual(playbook.splitlines()[1], "- import_playbook: 00-preflight.yml")
        self.assertIn("labweaver_preflight_scope: private-sigstore", playbook)
        self.assertIn("SIGSTORE_BACKUP_REQUIRED", tasks)
        self.assertIn("SIGSTORE_CHART_IDENTITY_MISMATCH", tasks)
        self.assertIn("SIGSTORE_PUBLIC_ENDPOINT_FORBIDDEN", tasks)
        self.assertIn("SIGSTORE_MUTABLE_IMAGE_FORBIDDEN", tasks)
        self.assertIn("no_log: true", tasks)
        self.assertIn("createcerts: {enabled: false}", values)
        self.assertIn("createtree: {enabled: false}", values)
        self.assertIn("force: false", values)
        self.assertIn("signer: file:///var/run/rekor-signer/private-key.pem", values)
        self.assertIn("existingSecret: {{ private_sigstore_trillian_mysql_secret_name }}", values)
        self.assertIn("username: {{ private_sigstore_trillian_mysql_username }}", values)
        self.assertIn("tuf:\n  enabled: false", values)
        self.assertIn("SIGSTORE_C0_AUTHORITY_MISMATCH", tasks)
        self.assertIn("private_sigstore_tuf_metadata_configmap_name", tuf_static)
        self.assertIn("sigstore_lock.images.tuf_static", tuf_static)
        self.assertIn("automountServiceAccountToken: false", tuf_static)
        self.assertNotIn("signer: memory", values)
        self.assertLess(
            tasks.index("Apply fail-closed isolation before workload creation"),
            tasks.index("Reconcile the pinned Private Sigstore chart"),
        )
        self.assertNotIn("type: NodePort", values)
        self.assertNotIn("type: LoadBalancer", values)
        self.assertIn("kind: NetworkPolicy", policy)
        self.assertIn("kind: CiliumNetworkPolicy", policy)
        self.assertIn("fromEntities: [ingress]", policy)
        self.assertIn("private_sigstore_kubernetes_api_cluster_ip", policy)
        self.assertIn("SIGSTORE_KUBERNETES_API_IDENTITY_INVALID", tasks)
        self.assertIn("Delete only replaceable chart bootstrap Jobs", tasks)
        self.assertIn("post_renderer: /var/tmp/labweaver-private-sigstore-post-renderer", tasks)
        self.assertIn("atomic: false", tasks)
        self.assertIn("createdb:\n    enabled: false", values)
        self.assertIn("fsGroupChangePolicy: OnRootMismatch", values)
        self.assertIn("toEntities: [kube-apiserver]", policy)
        self.assertIn("podSelector: {}", policy)
        self.assertNotIn("0.0.0.0/0", policy)
        self.assertIn("protocol: HTTPS", gateway)
        self.assertIn("certificateRefs", gateway)
        self.assertIn("scaffold_chart: 0.6.111", lock)
        self.assertIn("cosign: 3.0.6", lock)
        for line in lock.splitlines():
            if line.lstrip().startswith(("fulcio:", "rekor:", "ctlog:", "tuf:")) and "ghcr.io" in line:
                self.assertRegex(line, r"@sha256:[0-9a-f]{64}$")
        self.assertNotIn(":latest", lock)
        self.assertNotIn("oauth2.sigstore.dev", tasks + values + policy + gateway)

    def test_private_sigstore_lifecycle_is_allowlisted_and_backup_first(self) -> None:
        lifecycle = ROOT / "deploy/ansible/roles/private_sigstore_lifecycle/tasks"
        main = (lifecycle / "main.yml").read_text(encoding="utf-8")
        provider = (lifecycle / "provider.yml").read_text(encoding="utf-8")
        cleanup = (lifecycle / "cleanup.yml").read_text(encoding="utf-8")
        for number, action in (
            (97, "backup"), (98, "restore"), (99, "rotate"),
            (100, "verify"), (101, "cleanup"), (102, "disaster-recovery"),
        ):
            playbook = ROOT / f"deploy/ansible/playbooks/{number}-private-sigstore-{action}.yml"
            self.assertTrue(playbook.is_file())
            self.assertEqual(playbook.read_text(encoding="utf-8").splitlines()[1], "- import_playbook: 00-preflight.yml")
            self.assertIn(
                "labweaver_preflight_scope: private-sigstore",
                playbook.read_text(encoding="utf-8"),
            )
        self.assertLess(main.index("Create mandatory pre-change backup"), main.index("Execute restore provider"))
        self.assertLess(main.index("Create mandatory pre-change backup"), main.index("Execute rotation provider"))
        self.assertLess(main.index("Create mandatory pre-change backup"), main.index("Execute disaster-recovery provider"))
        self.assertIn("SIGSTORE_LIFECYCLE_REPORT_IDENTITY_INVALID", provider)
        self.assertIn("labweaver.io/testflight-run", provider)
        self.assertIn("jobs,pods,configmaps", cleanup)
        cleanup_report = (lifecycle.parent / "templates/lifecycle-report.json.j2").read_text(encoding="utf-8")
        self.assertIn('"action": "cleanup"', cleanup_report)
        self.assertIn('"deployment_manifest_sha256"', cleanup_report)
        for forbidden in ("namespace,", "persistentvolumeclaim", "secret,", "deployment,"):
            self.assertNotIn(forbidden, cleanup.lower())

        lifecycle_schema = json.loads(
            (ROOT / "schemas/contracts/v1/private-sigstore-lifecycle-report.schema.json").read_text(encoding="utf-8")
        )
        for field in (
            "run_id", "commit_sha", "controller_id", "cluster_uid", "inventory_sha256",
            "deployment_manifest_sha256", "component_lock_sha256", "chart_archive_sha256",
            "image_digests", "trust_bundle_sha256", "tuf_root_version", "tuf_root_sha256",
            "workload_identity_policy_sha256", "checks", "blocked_items", "generated_at",
        ):
            self.assertIn(field, lifecycle_schema["required"])

    def test_private_sigstore_preflight_rejects_worker_and_nfs_targets(self) -> None:
        preflight = (
            ROOT / "deploy/ansible/roles/preflight/tasks/main.yml"
        ).read_text(encoding="utf-8")
        docs = (ROOT / "docs/deployment/private-sigstore.md").read_text(encoding="utf-8")
        self.assertIn("SIGSTORE_INVENTORY_SCOPE_INVALID", preflight)
        self.assertIn("groups.get('workers', []) | length == 0", preflight)
        self.assertIn("groups.get('nfs_servers', []) | length == 0", preflight)
        self.assertIn("(groups.get('control_plane', []) | sort) == ['k8s-cp1']", preflight)
        self.assertIn("when: (labweaver_preflight_scope | default('cluster')) == 'cluster'", preflight)
        self.assertNotIn("- item is not match('^REPLACE_')", preflight)
        self.assertIn("sigstore_controller_inputs | select('match', '^REPLACE_')", preflight)
        self.assertIn("rejects Worker or NFS inventory", docs)

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
        sigstore_values = (
            ROOT / "deploy/ansible/roles/private_sigstore/templates/values.yml.j2"
        ).read_text(encoding="utf-8")

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
        self.assertIn("ChallengeClaim: preferred_username", sigstore_values)
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
        self.assertIn("name: labweaver-private-sigstore-observer", operator_rbac)
        self.assertIn("name: labweaver-devops-observer", operator_rbac)
        self.assertNotIn("'harbor', 'sigstore-system'", operator_rbac)
        self.assertNotIn("resources: [secrets", operator_rbac)
        self.assertNotIn(":latest", workloads)

    def test_render_validator_rejects_mutable_and_public_images(self) -> None:
        validator_path = ROOT / "tests/ansible/validate_sigstore_render.py"
        spec = importlib.util.spec_from_file_location("sigstore_render", validator_path)
        assert spec is not None and spec.loader is not None
        validator = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(validator)
        original = sys.argv
        try:
            with tempfile.TemporaryDirectory() as directory:
                fixture = Path(directory) / "render.yml"
                fixture.write_text("image: ghcr.io/example/app:latest\n", encoding="utf-8")
                sys.argv = [str(validator_path), str(fixture)]
                with self.assertRaisesRegex(SystemExit, "SIGSTORE_RENDER_MUTABLE_IMAGE_FORBIDDEN"):
                    validator.main()
                fixture.write_text(
                    "image: ghcr.io/example/app@sha256:" + "a" * 64 + "\nvalue: fulcio.sigstore.dev\n",
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(SystemExit, "SIGSTORE_RENDER_PUBLIC_FALLBACK_FORBIDDEN"):
                    validator.main()
        finally:
            sys.argv = original

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


if __name__ == "__main__":
    unittest.main()
