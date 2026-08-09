"""Contract evidence for the router-owned Ansible controller and safety policies."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import re
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
    def test_platform_foundation_images_are_immutable(self) -> None:
        lock = yaml.safe_load((ROOT / "deploy/versions.lock.yml").read_text(encoding="utf-8"))
        foundation = lock["platform_foundation"]
        images = {key: foundation[key] for key in ("nats", "nats_box", "minio", "buildkit_rootless")}

        for reference in images.values():
            self.assertRegex(reference, r"^[^\s]+@sha256:[0-9a-f]{64}$")
        self.assertRegex(lock["postgresql"]["image"], r"^[^\s]+@sha256:[0-9a-f]{64}$")

    def test_platform_administration_tools_are_locked_and_installed_before_foundation(self) -> None:
        lock = yaml.safe_load((ROOT / "deploy/versions.lock.yml").read_text(encoding="utf-8"))
        tools = lock["platform_foundation"]["admin_tools"]
        foundation_playbook = (ROOT / "deploy/ansible/playbooks/92-platform-foundation.yml").read_text(
            encoding="utf-8"
        )
        tool_playbook = (ROOT / "deploy/ansible/playbooks/91-platform-admin-tools.yml").read_text(
            encoding="utf-8"
        )
        tasks = (ROOT / "deploy/ansible/roles/platform_admin_tools/tasks/main.yml").read_text(
            encoding="utf-8"
        )

        self.assertEqual(
            set(tools),
            {"nats_cli", "nsc", "minio_client", "buildctl", "keycloak_admin", "system_packages"},
        )
        for name in ("nats_cli", "nsc", "minio_client", "buildctl", "keycloak_admin"):
            self.assertRegex(tools[name]["linux_amd64_sha256"], r"^[0-9a-f]{64}$")
            self.assertTrue(tools[name]["linux_amd64_url"].startswith("https://github.com/"))
        self.assertIn("- import_playbook: 91-platform-admin-tools.yml", foundation_playbook)
        self.assertIn("platform_admin_tools_profile: authoring", tool_playbook)
        self.assertIn("platform_admin_tools_profile: execution", tool_playbook)
        self.assertIn("checksum: \"sha256:", tasks)
        self.assertNotIn("ansible.builtin.shell", tasks)

    def test_platform_foundation_is_persistent_and_reset_preserves_it(self) -> None:
        playbook = (ROOT / "deploy/ansible/playbooks/92-platform-foundation.yml").read_text(
            encoding="utf-8"
        )
        tasks = (ROOT / "deploy/ansible/roles/platform_foundation/tasks/main.yml").read_text(
            encoding="utf-8"
        )
        workloads = (
            ROOT / "deploy/ansible/roles/platform_foundation/templates/workloads.yml.j2"
        ).read_text(encoding="utf-8")
        reset = (ROOT / "deploy/ansible/roles/platform_reset/defaults/main.yml").read_text(
            encoding="utf-8"
        )

        self.assertIn("labweaver_preflight_scope: platform-foundation", playbook)
        self.assertIn("- import_playbook: 91-platform-admin-tools.yml", playbook)
        self.assertIn("platform-foundation --infra", (
            ROOT / "docs/deployment/ansible.md"
        ).read_text(encoding="utf-8"))
        self.assertIn("PLATFORM_FOUNDATION_BUNDLE_KEYS_INVALID", tasks)
        self.assertIn("platform_foundation_postgres_admin_role", tasks)
        self.assertIn("kubernetes.core.k8s_exec", tasks)
        self.assertIn("PLATFORM_FOUNDATION_POSTGRES_ADMIN_ROLE_INVALID", tasks)
        self.assertIn("platform_foundation_postgres_database", tasks)
        self.assertIn("Delete exact pod blocked on a superseded failed revision", tasks)
        self.assertIn("item.status.currentRevision != item.status.updateRevision", tasks)
        self.assertIn("kind: StatefulSet", workloads)
        self.assertEqual(workloads.count("kind: StatefulSet"), 3)
        replacements = {
            "{{ platform_foundation_namespace }}": "labweaver-data",
            "{{ platform_foundation_storage_class }}": "local-path",
            "{{ platform_foundation_minio_storage_class }}": "nfs-rwx",
            "{{ platform_foundation_postgres_storage }}": "20Gi",
            "{{ platform_foundation_nats_storage }}": "10Gi",
            "{{ platform_foundation_minio_storage }}": "100Gi",
            "{{ platform_foundation_lock.postgresql.image }}": "registry.invalid/postgres@sha256:" + "a" * 64,
            "{{ platform_foundation_lock.platform_foundation.nats }}": "registry.invalid/nats@sha256:" + "b" * 64,
            "{{ platform_foundation_lock.platform_foundation.minio }}": "registry.invalid/minio@sha256:" + "c" * 64,
            "{{ platform_foundation_workload_configuration_sha256.postgres }}": "d" * 64,
            "{{ platform_foundation_workload_configuration_sha256.nats }}": "e" * 64,
            "{{ platform_foundation_workload_configuration_sha256.minio }}": "f" * 64,
        }
        rendered = workloads
        for source, value in replacements.items():
            rendered = rendered.replace(source, value)
        documents = list(yaml.safe_load_all(rendered))
        self.assertEqual(len(documents), 9)
        self.assertEqual(sum(document["kind"] == "StatefulSet" for document in documents), 3)
        expected_hashes = {
            "postgres": "d" * 64,
            "nats": "e" * 64,
            "minio": "f" * 64,
        }
        for document in documents:
            if document["kind"] == "StatefulSet":
                self.assertEqual(
                    document["spec"]["template"]["metadata"]["annotations"]
                    ["labweaver.io/configuration-sha256"],
                    expected_hashes[document["metadata"]["name"]],
                )
        minio = next(
            document for document in documents
            if document["kind"] == "StatefulSet" and document["metadata"]["name"] == "minio"
        )
        self.assertEqual(minio["spec"]["template"]["spec"]["securityContext"]["runAsUser"], 65534)
        owner_policy = next(
            document for document in documents
            if document["kind"] == "NetworkPolicy" and document["metadata"]["name"] == "owner-services"
        )
        self.assertEqual(len(owner_policy["spec"]["ingress"]), 1)
        admin_policy = next(
            document for document in documents
            if document["kind"] == "CiliumNetworkPolicy"
            and document["metadata"]["name"] == "admin-probes"
        )
        controller_ingress = admin_policy["spec"]["ingress"][0]
        self.assertEqual(controller_ingress["fromEntities"], ["host", "remote-node"])
        self.assertEqual(
            {entry["port"] for entry in controller_ingress["toPorts"][0]["ports"]},
            {"4222", "5432", "9000"},
        )
        self.assertIn("pod-security.kubernetes.io/enforce: restricted", tasks)
        reset_namespaces = reset.split("platform_reset_domains:", maxsplit=1)[0]
        self.assertNotIn("labweaver-data", reset_namespaces)
        self.assertNotIn("labweaver-build", reset_namespaces)

    def test_nats_authority_rotation_is_bounded_and_recoverable(self) -> None:
        playbook = (
            ROOT / "deploy/ansible/playbooks/96-nats-authority-rotation.yml"
        ).read_text(encoding="utf-8")

        self.assertIn("--workloads-seed-file", playbook)
        self.assertIn("NATS_AUTHORITY_ROTATION_ROLLBACK_SURFACE_INCOMPLETE", playbook)
        self.assertIn("rollback/kubernetes-objects.yaml", playbook)
        self.assertIn("- import_playbook: 92-platform-foundation.yml", playbook)
        self.assertIn("Apply only reviewed NATS-bearing application objects", playbook)
        self.assertIn("--force-conflicts", playbook)
        self.assertIn("Roll every affected workload to the replacement authority", playbook)
        self.assertIn("resource-service", playbook)
        self.assertIn("nats_rotation_record.identities | length == 10", playbook)
        self.assertIn("Verify replacement administrator JWT and mutual TLS", playbook)
        self.assertIn("Wait for replacement NATS administrator transport", playbook)
        self.assertIn("LABWEAVER_RESOURCE", playbook)
        self.assertIn("NATS_AUTHORITY_ROTATION_RESOURCE_STREAM_INVALID", playbook)
        self.assertNotIn("ansible.builtin.shell", playbook)
        self.assertNotRegex(playbook, r"\bkubectl\s+delete\b")
        self.assertNotRegex(playbook, r"\bDROP\s+(?:DATABASE|SCHEMA)\b")

    def test_object_store_proxy_has_a_minio_only_transport_rule(self) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        marker = "Browser uploads terminate at the Web nginx object-store proxy."
        self.assertIn(marker, tasks)
        scoped_rule = tasks.split(marker, maxsplit=1)[1].split(
            "- name: Inspect the retained CDI clone source network policy",
            maxsplit=1,
        )[0]
        self.assertIn("app.kubernetes.io/name: web", scoped_rule)
        self.assertIn('port: "9000"', scoped_rule)
        self.assertNotIn('port: "5432"', scoped_rule)
        self.assertNotIn('port: "4222"', scoped_rule)

    def test_platform_buildkit_is_rootless_isolated_and_explicitly_exception_bound(self) -> None:
        playbook = (ROOT / "deploy/ansible/playbooks/92-platform-buildkit.yml").read_text(
            encoding="utf-8"
        )
        tasks = (ROOT / "deploy/ansible/roles/platform_buildkit/tasks/main.yml").read_text(
            encoding="utf-8"
        )
        workloads = (
            ROOT / "deploy/ansible/roles/platform_buildkit/templates/workloads.yml.j2"
        ).read_text(encoding="utf-8")
        self.assertIn("labweaver_preflight_scope: platform-buildkit", playbook)
        self.assertIn(
            "'platform-buildkit'",
            (ROOT / "deploy/ansible/roles/preflight/tasks/main.yml").read_text(
                encoding="utf-8"
            ),
        )
        self.assertIn("PLATFORM_BUILDKIT_BUNDLE_KEYS_INVALID", tasks)
        self.assertIn("PLATFORM_BUILDKIT_READBACK_INVALID", tasks)
        self.assertIn("pod-security.kubernetes.io/enforce: privileged", tasks)
        self.assertIn("runAsNonRoot: true", workloads)
        self.assertIn("privileged: false", workloads)
        self.assertIn("allowPrivilegeEscalation: true", workloads)
        self.assertIn("capabilities: {drop: [ALL], add: [SETUID, SETGID]}", workloads)
        self.assertIn("- --tlsservername\n                - buildkit", workloads)
        self.assertIn("seccompProfile: {type: Unconfined}", workloads)
        self.assertIn("appArmorProfile: {type: Unconfined}", workloads)
        self.assertIn("automountServiceAccountToken: false", workloads)
        self.assertIn("dnsPolicy: None", workloads)
        self.assertIn("nameservers: [{{ platform_buildkit_dns_nameserver }}]", workloads)
        self.assertNotIn("hostPath:", workloads)
        self.assertNotIn("hostNetwork: true", workloads)
        self.assertIn("--oci-worker-no-process-sandbox", workloads)
        self.assertIn("labweaver.io/configuration-sha256: {{ platform_buildkit_bundle_sha256 }}", workloads)
        self.assertIn("mountPath: /home/user/.local/tmp", workloads)
        self.assertIn("name: TMPDIR, value: /tmp", workloads)
        self.assertIn("default-deny", workloads)
        self.assertIn("buildctl", workloads)
        self.assertIn("platform_buildkit_registry_cidr", tasks)
        self.assertIn("/etc/buildkit/tls/registry-ca.crt", tasks)
        self.assertIn("PLATFORM_BUILDKIT_HARBOR_CA_MISMATCH", tasks)
        self.assertIn("harbor-nginx", (
            ROOT / "deploy/ansible/roles/platform_buildkit/defaults/main.yml"
        ).read_text(encoding="utf-8"))
        self.assertIn("harbor-public/registry-ca.crt", (
            ROOT / "docs/deployment/ansible.md"
        ).read_text(encoding="utf-8"))
        self.assertIn("platform-buildkit --infra", (
            ROOT / "docs/deployment/ansible.md"
        ).read_text(encoding="utf-8"))
        self.assertIn("PlatformBuildkit", (
            ROOT / "xtask/src/main.rs"
        ).read_text(encoding="utf-8"))

    def test_controller_execution_is_router_owned(self) -> None:
        docs = (ROOT / "docs/deployment/ansible.md").read_text(encoding="utf-8")
        controller_lock = (ROOT / "deploy/ansible/controller.lock.yml").read_text(encoding="utf-8")
        xtask = (ROOT / "xtask/src/main.rs").read_text(encoding="utf-8")
        self.assertIn("cargo xtask deploy --infra", docs)
        self.assertIn("ansible-rs", docs)
        self.assertNotIn("tools/ansible.py", docs)
        self.assertIn(
            "approved_controller_ids: edge-router,wsl-a-controller,docker-desktop-controller",
            controller_lock,
        )
        self.assertIn("python_kubernetes_version: 34.1.0", controller_lock)
        self.assertIn("require_python_module_version", xtask)
        self.assertIn("inventory_identity_hash(&inventory_root)", xtask)

    def test_harbor_gateway_uses_the_chart_nginx_contract(self) -> None:
        gateway = (
            ROOT / "deploy/ansible/roles/harbor/templates/gateway.yml.j2"
        ).read_text(encoding="utf-8")
        platform_route = (
            ROOT / "deploy/ansible/roles/platform_harbor_route/tasks/main.yml"
        ).read_text(encoding="utf-8")
        self.assertIn("name: {{ harbor_release_name }}\n          port: 80", gateway)
        self.assertNotIn("{{ harbor_release_name }}-core", gateway)
        self.assertNotIn("{{ harbor_release_name }}-portal", gateway)
        self.assertIn("LABWEAVER PLATFORM HARBOR", platform_route)
        self.assertIn("PLATFORM_HARBOR_ROUTER_RESOLUTION_INVALID", platform_route)
        self.assertIn("Refresh router system trust", platform_route)

    def test_platform_application_pins_and_adopts_the_vm_base(self) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        lock = (ROOT / "deploy/versions.lock.yml").read_text(encoding="utf-8")
        self.assertIn("PLATFORM_VM_BASE_DATAVOLUME_CONFLICT", tasks)
        self.assertIn("PLATFORM_VM_BASE_DATASOURCE_CONFLICT", tasks)
        self.assertIn("PLATFORM_VM_BASE_IDENTITY_INVALID", tasks)
        self.assertEqual(
            tasks.count('cdi.kubevirt.io/storage.bind.immediate.requested: "true"'),
            3,
        )
        self.assertIn(
            "Reconcile immediate binding for the immutable Sprint 2 VM base",
            tasks,
        )
        self.assertIn(
            "Reconcile immediate binding on the immutable Sprint 2 VM base claim",
            tasks,
        )
        self.assertIn("docker://quay.io/containerdisks/ubuntu@sha256:", lock)
        self.assertIn("data_source_name: ubuntu-lab-base-v1", lock)
        self.assertNotIn("state: absent", tasks)

    def test_platform_application_report_excludes_removed_product_dependencies(self) -> None:
        report = (
            ROOT
            / "deploy/ansible/roles/platform_application/templates/application-report.json.j2"
        ).read_text(encoding="utf-8")
        self.assertNotIn('"kyverno"', report)
        self.assertNotIn('"private-sigstore"', report)
        for retained in (
            "kubernetes",
            "kubevirt",
            "postgresql",
            "nats",
            "minio",
            "harbor",
            "keycloak",
        ):
            self.assertIn(f'"{retained}"', report)

    def test_platform_application_ssh_service_port_is_an_integer(self) -> None:
        values = (ROOT / "deploy/helm/labweaver/values.yaml").read_text(encoding="utf-8")
        self.assertIn("containerPort: 2222, servicePort: 2222", values)
        self.assertNotIn('servicePort: "2222"', values)

    def test_platform_application_builds_helm_value_arguments_without_regex(self) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        arguments = tasks.split(
            "- name: Build immutable Helm arguments", maxsplit=1
        )[1].split("- name:", maxsplit=1)[0]
        self.assertIn("for values_file in platform_application_values_files", arguments)
        self.assertIn("'--values=' + platform_application_report_root", arguments)
        self.assertIn(
            "'imagePullSecrets[0].name=' + platform_application_image_pull_secret_name",
            arguments,
        )
        self.assertIn(
            "'deploymentIdentity.configurationBundleSha256=' + platform_application_configuration_bundle_sha256",
            arguments,
        )
        self.assertIn("'portalRoute.enabled=true'", arguments)
        self.assertIn("'objectStoreRoute.enabled=true'", arguments)
        self.assertIn("'objectStoreRoute.pathPrefix=/' + platform_application_minio_bucket", arguments)
        self.assertIn("'objectStoreRoute.caCertificate=' + platform_application_minio_ca_file", arguments)
        self.assertIn("'workloads.control-service.externalEgress[0].cidr='", arguments)
        self.assertIn(
            "'portalRoute.namespace=' + platform_application_portal_route_namespace",
            arguments,
        )
        self.assertIn("'sshGatewayService.enabled=true'", arguments)
        self.assertIn(
            "'sshGatewayService.loadBalancerIP=' + platform_application_ssh_load_balancer_ip",
            arguments,
        )
        self.assertNotIn("regex_replace", arguments)

    def test_platform_application_binds_configuration_identity_to_every_workload(self) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        values = (ROOT / "deploy/helm/labweaver/values.yaml").read_text(encoding="utf-8")
        helpers = (
            ROOT / "deploy/helm/labweaver/templates/_helpers.tpl"
        ).read_text(encoding="utf-8")
        workloads = (
            ROOT / "deploy/helm/labweaver/templates/workloads.yaml"
        ).read_text(encoding="utf-8")

        self.assertIn("platform_application_configuration_bundle_sha256", tasks)
        self.assertIn("platform_application_controller_inputs.results[4].stat.checksum", tasks)
        self.assertIn("configurationBundleSha256: \"\"", values)
        self.assertIn(
            'required "deploymentIdentity.configurationBundleSha256 is required"',
            helpers,
        )
        self.assertIn('regexMatch "^sha256:[0-9a-f]{64}$"', helpers)
        self.assertIn(
            "labweaver.io/configuration-bundle-sha256:",
            workloads,
        )

    def test_platform_application_binds_freeze_worker_to_packaged_evaluation_image(
        self,
    ) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        self.assertIn(
            "Bind the evaluation freeze worker to the immutable package image",
            tasks,
        )
        self.assertIn(
            "PLATFORM_APPLICATION_EVALUATION_WORKER_IMAGE_INVALID",
            tasks,
        )
        self.assertIn(
            "'component', 'equalto', 'evaluation-service'",
            tasks,
        )

    def test_platform_application_requires_canonical_oidc_platform_admin_mapping(self) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        self.assertIn("platform_application_access_configuration", tasks)
        self.assertIn("| from_yaml_all", tasks)
        self.assertLess(
            tasks.index("Parse immutable package and private configuration"),
            tasks.index("Parse Access Service role configuration from the reviewed bundle"),
        )
        self.assertNotIn(
            "platform_application_configuration_objects\n         | selectattr('kind'",
            tasks.split("Parse Access Service role configuration from the reviewed bundle", 1)[0],
        )
        self.assertIn(
            "platform_application_access_configuration.oidc.role_mappings.platform_admin == 'platform_admin'",
            tasks,
        )
        self.assertIn(
            "platform_application_access_configuration.oidc.role_mappings['platform-admin'] == 'platform_admin'",
            tasks,
        )
        self.assertIn("PLATFORM_APPLICATION_OIDC_ROLE_MAPPING_INVALID", tasks)
        self.assertIn(
            "'workerImage':\n                              platform_application_evaluation_worker_image",
            tasks,
        )
        self.assertIn(
            "when: item.metadata.name != 'evaluation-service-config'",
            tasks,
        )

    def test_platform_application_rejects_stale_nats_ca_and_incomplete_evaluation_binding(self) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        self.assertIn("Select reviewed NATS and Evaluation configuration objects", tasks)
        self.assertIn("platform_application_bundle_nats_ca_sha256", tasks)
        self.assertIn("platform_application_remote_nats_ca_sha256", tasks)
        self.assertIn(
            "selectattr('item', 'equalto', platform_application_nats_ca_file)",
            tasks,
        )
        self.assertIn("map(attribute='stat.checksum')", tasks)
        self.assertIn(
            "platform_application_bundle_nats_ca_sha256 == platform_application_remote_nats_ca_sha256",
            tasks,
        )
        self.assertIn(
            "platform_application_evaluation_configuration.coordinator.workerRegistryPullConfigFile",
            tasks,
        )
        self.assertIn("PLATFORM_APPLICATION_CONFIGURATION_BINDING_INVALID", tasks)
        self.assertLess(
            tasks.index("Require reviewed configuration bindings before any cluster mutation"),
            tasks.index("Atomically deploy the immutable Sprint 2 profile"),
        )

    def test_platform_application_preinstalls_evaluation_runner_default_deny(
        self,
    ) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        policy = tasks.split(
            "- name: Reconcile the permanent evaluation runner namespace default deny",
            maxsplit=1,
        )[1].split(
            "- name: Require the evaluation worker to use the platform image pull identity",
            maxsplit=1,
        )[0]

        self.assertIn("name: oj-runner-default-deny", policy)
        self.assertIn(
            'namespace: "{{ platform_application_evaluation_namespace }}"',
            policy,
        )
        self.assertIn("podSelector: {}", policy)
        self.assertIn("policyTypes: [Ingress, Egress]", policy)
        self.assertIn("ingress: []", policy)
        self.assertIn("egress: []", policy)
        self.assertIn(
            "spec.ingress | default([]) == []",
            policy,
        )
        self.assertIn(
            "spec.egress | default([]) == []",
            policy,
        )
        self.assertIn(
            "PLATFORM_APPLICATION_EVALUATION_DEFAULT_DENY_INVALID",
            policy,
        )

    def test_environment_owner_rollout_does_not_require_surge_capacity(self) -> None:
        values = (ROOT / "deploy/helm/labweaver/values.yaml").read_text(encoding="utf-8")
        workloads = (
            ROOT / "deploy/helm/labweaver/templates/workloads.yaml"
        ).read_text(encoding="utf-8")

        self.assertIn("with $configuration.strategy", workloads)
        environment = values.split("  environment-service:", maxsplit=1)[1].split(
            "  container-executor:", maxsplit=1
        )[0]
        self.assertIn("maxSurge: 0", environment)
        self.assertIn("maxUnavailable: 1", environment)

        evaluation = values.split("  evaluation-service:", maxsplit=1)[1].split(
            "  resource-service:", maxsplit=1
        )[0]
        self.assertIn("maxSurge: 0", evaluation)
        self.assertIn("maxUnavailable: 1", evaluation)

    def test_object_store_route_uses_existing_web_workload_with_verified_tls(self) -> None:
        backend = (
            ROOT / "deploy/helm/labweaver/templates/object-store-backend.yaml"
        ).read_text(encoding="utf-8")
        route = (
            ROOT / "deploy/helm/labweaver/templates/portal-route.yaml"
        ).read_text(encoding="utf-8")
        nginx = (ROOT / "containers/nginx.conf").read_text(encoding="utf-8")
        self.assertIn("proxy_ssl_verify on;", backend)
        self.assertIn("proxy_request_buffering off;", backend)
        self.assertIn("proxy_set_header Host $http_host;", backend)
        self.assertNotIn("kind: BackendTLSPolicy", backend)
        self.assertNotIn("kind: CiliumNetworkPolicy", backend)
        self.assertIn("- name: web", route)
        self.assertEqual(route.count("kind: ReferenceGrant"), 1)
        object_store_rule = route.split("value: {{ required \"objectStoreRoute.pathPrefix is required\"", 1)[1]
        self.assertIn("- name: web", object_store_rule)
        self.assertIn("namespace: {{ .Release.Namespace }}", object_store_rule)
        self.assertIn("port: 8080", object_store_rule)
        self.assertNotIn("objectStoreRoute.serviceName", object_store_rule)
        self.assertIn("name: web", route)
        self.assertIn("object-store", route)
        self.assertNotIn("name: minio\n      namespace:", route)
        self.assertNotIn("-object-store\n", route)
        self.assertIn("include /etc/nginx/labweaver-conf.d/*.conf;", nginx)

    def test_kubernetes_api_egress_includes_submission_freeze_owner(self) -> None:
        policy = (
            ROOT / "deploy/helm/labweaver/templates/cilium-ingress-policy.yaml"
        ).read_text(encoding="utf-8")
        self.assertIn(
            "values: [container-executor, evaluation-service, kubevirt-executor, kubevirt-console-executor]",
            policy,
        )
        self.assertIn("toEntities: [kube-apiserver]", policy)

    def test_resource_release_owns_only_its_kubernetes_api_egress(self) -> None:
        policy = (
            ROOT
            / "deploy/helm/labweaver/templates/resource-kube-api-egress.yaml"
        ).read_text(encoding="utf-8")
        tasks = (
            ROOT / "deploy/ansible/roles/resource_application/tasks/main.yml"
        ).read_text(encoding="utf-8")

        self.assertIn("name: resource-kube-api-egress", policy)
        self.assertIn('(index .Values.workloads "resource-service").enabled', policy)
        self.assertIn("app.kubernetes.io/name: resource-service", policy)
        self.assertIn("toEntities: [kube-apiserver]", policy)
        self.assertIn(
            "network.resourceKubernetesApiCiliumPolicyEnabled=true", tasks
        )
        self.assertIn("resource_application_run_id is match('^[0-9a-f]{8}", tasks)
        self.assertIn("Apply or verify Resource Access bootstrap", tasks)
        self.assertIn("labweaver_postgres_apply:", tasks)
        postgres_apply = (
            ROOT / "deploy/ansible/library/labweaver_postgres_apply.py"
        ).read_text(encoding="utf-8")
        self.assertIn("PGSERVICEFILE", postgres_apply)
        self.assertIn("port-forward", postgres_apply)
        seed = (
            ROOT
            / "deploy/ansible/roles/resource_application/templates/access-seed-adopt.sql.j2"
        ).read_text(encoding="utf-8")
        self.assertIn("LW_RESOURCE_ACCEPTANCE_PROFILE_ACCESS_SEED_CONFLICT", seed)
        self.assertIn("LW_RESOURCE_ACCEPTANCE_PROFILE_ACCESS_ROLE_CONFLICT", seed)
        self.assertNotIn("resource_requests", seed)
        self.assertNotIn("environment_template_releases", seed)

    def test_resource_api_uses_mtls_and_signed_access_delegation(self) -> None:
        values = (ROOT / "deploy/helm/labweaver/values.yaml").read_text(encoding="utf-8")
        access_config = (
            ROOT / "deploy/config/access-auth.yaml.example"
        ).read_text(encoding="utf-8")
        resource_config = (
            ROOT / "deploy/config/resource-service.yaml.example"
        ).read_text(encoding="utf-8")
        network_policy = (
            ROOT / "deploy/helm/labweaver/templates/network-policy.yaml"
        ).read_text(encoding="utf-8")
        resource_values = values.split("  resource-service:", maxsplit=1)[1].split(
            "resources:", maxsplit=1
        )[0]
        self.assertIn("LABWEAVER_RESOURCE_MTLS_CONFIG_FILE", resource_values)
        self.assertIn("containerPort: 9448", resource_values)
        self.assertIn("delegation_key_locator", access_config)
        self.assertIn("delegation_key_file", resource_config)
        self.assertIn("port: 9448", network_policy)

    def test_platform_application_reads_kubernetes_items_as_a_mapping_key(self) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        readback = tasks.split(
            "- name: Require exact workload count and digest-only images", maxsplit=1
        )[1].split("- name:", maxsplit=1)[0]

        self.assertEqual(
            readback.count(
                "(platform_application_deployments.stdout | from_json)['items']"
            ),
            3,
        )
        self.assertNotIn("(platform_application_deployments.stdout | from_json).items", readback)

        helpers = (
            ROOT / "deploy/helm/labweaver/templates/_helpers.tpl"
        ).read_text(encoding="utf-8")
        self.assertIn("app.kubernetes.io/instance: {{ .Release.Name }}", helpers)

    def test_platform_application_binds_the_baseline_to_the_runtime_database(self) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        defaults = (
            ROOT / "deploy/ansible/roles/platform_application/defaults/main.yml"
        ).read_text(encoding="utf-8")
        adoption = (
            ROOT
            / "deploy/ansible/roles/platform_application/templates/baseline-adopt.sql.j2"
        ).read_text(encoding="utf-8")
        self.assertIn("platform_application_postgres_database: labweaver", defaults)
        self.assertIn("SELECT current_database()", tasks)
        self.assertIn(
            "PLATFORM_APPLICATION_POSTGRES_DATABASE_IDENTITY_MISMATCH", tasks
        )
        self.assertLess(
            tasks.index("Require the exact Sprint 2 PostgreSQL database"),
            tasks.index("Render non-destructive six-domain baseline adoption"),
        )
        self.assertIn("domain_catalog.migrations", adoption)
        self.assertIn("MIGRATION_PREFIX_INVALID", adoption)
        self.assertIn("MIGRATION_SET_INCOMPLETE", adoption)
        self.assertIn("migration.file | basename", adoption)
        self.assertIn("platform_application_retained_baseline_sha256", adoption)
        self.assertNotIn("count(*) FROM {{ domain }}.schema_migrations) <> 1", adoption)
        self.assertIn("PLATFORM_APPLICATION_RETAINED_BASELINE_IDENTITY_INVALID", tasks)
        self.assertIn("platform_application_retained_baseline_sha256", defaults)

    def test_platform_application_owns_a_reconnectable_postgres_port_forward(self) -> None:
        defaults = (
            ROOT / "deploy/ansible/roles/platform_application/defaults/main.yml"
        ).read_text(encoding="utf-8")
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        handlers = (
            ROOT / "deploy/ansible/roles/platform_application/handlers/main.yml"
        ).read_text(encoding="utf-8")
        service = (
            ROOT
            / "deploy/ansible/roles/platform_application/templates/postgres-port-forward.service.j2"
        ).read_text(encoding="utf-8")
        self.assertIn("platform_application_postgres_forward_enabled", defaults)
        self.assertIn("platform_application_postgres_forward_service_name is match", tasks)
        self.assertIn("Restart=on-failure", service)
        self.assertIn("service/{{ platform_application_postgres_forward_kubernetes_service }}", service)
        self.assertIn("Apply the PostgreSQL port-forward before database adoption", tasks)
        self.assertIn("Require the adopted PostgreSQL port-forward endpoint", tasks)
        self.assertIn("Restart the adopted PostgreSQL port-forward", handlers)
        self.assertIn("platform_application_postgres_forward_service_name", handlers)

    def test_platform_application_preflight_does_not_requalify_retained_hosts(self) -> None:
        application = (
            ROOT / "deploy/ansible/playbooks/93-platform-application.yml"
        ).read_text(encoding="utf-8")
        preflight = (
            ROOT / "deploy/ansible/playbooks/00-preflight.yml"
        ).read_text(encoding="utf-8")
        self.assertIn("labweaver_preflight_validate_remote_hosts: false", application)
        self.assertNotIn("91-platform-admin-tools.yml", application)
        self.assertIn("hosts: localhost", application)
        self.assertIn("connection: local", application)
        self.assertNotIn("hosts: control_plane", application)
        self.assertEqual(
            preflight.count(
                "labweaver_preflight_validate_remote_hosts | default(true) | bool"
            ),
            6,
        )

    def test_platform_application_reconciles_the_exact_durable_consumers(self) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        defaults = (
            ROOT / "deploy/ansible/roles/platform_application/defaults/main.yml"
        ).read_text(encoding="utf-8")
        for consumer in (
            "control-agent-run-projection-v1",
            "control-agent-build-projection-v1",
            "agent-build-command-v1",
            "environment-service-v1",
            "environment-release-v1",
        ):
            self.assertIn(consumer, defaults)
        self.assertIn("Create only missing Sprint 2 durable consumers", tasks)
        self.assertIn("PLATFORM_APPLICATION_CONSUMER_CONFLICT", tasks)
        self.assertIn("'--ack', 'explicit'", tasks)
        self.assertIn("'--pull'", tasks)
        self.assertIn("map('regex_replace', '^', '--filter=')", tasks)
        self.assertNotIn("consumer delete", tasks)

    def test_access_can_bind_the_adopted_keycloak_gateway_without_global_dns_changes(self) -> None:
        workloads = (
            ROOT / "deploy/helm/labweaver/templates/workloads.yaml"
        ).read_text(encoding="utf-8")
        policies = (
            ROOT / "deploy/helm/labweaver/templates/network-policy.yaml"
        ).read_text(encoding="utf-8")
        values = (ROOT / "deploy/helm/labweaver/values.yaml").read_text(encoding="utf-8")
        self.assertIn(
            "with (default $root.Values.hostAliases $configuration.hostAliases)",
            workloads,
        )
        self.assertNotIn("with $root.Values.hostAliases", workloads)
        self.assertIn("identityNamespaceSelector", policies)
        self.assertIn("port: 443", policies)
        self.assertIn("kubernetes.io/metadata.name: keycloak-system", values)

    def test_platform_application_distributes_harbor_ca_and_bounds_gateway_pod_security(self) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        handlers = (
            ROOT / "deploy/ansible/roles/platform_application/handlers/main.yml"
        ).read_text(encoding="utf-8")
        defaults = (
            ROOT / "deploy/ansible/roles/platform_application/defaults/main.yml"
        ).read_text(encoding="utf-8")
        self.assertIn("Load the adopted Harbor CA for Kubernetes nodes", tasks)
        self.assertIn("groups['k8s_cluster']", tasks)
        self.assertIn(
            'dest: "{{ platform_application_ca_trust_anchors_dir }}/labweaver-harbor.crt"',
            tasks,
        )
        self.assertIn(
            "platform_application_ca_trust_anchors_dir: /etc/pki/ca-trust/source/anchors",
            defaults,
        )
        self.assertIn("/etc/containers/certs.d/", tasks)
        self.assertIn("notify: Refresh Kubernetes node Harbor trust", tasks)
        self.assertIn("Apply changed Kubernetes node Harbor trust before workload rollout", tasks)
        self.assertIn("name: Refresh Kubernetes node Harbor trust", handlers)
        self.assertIn("{{ platform_application_ca_trust_refresh_command }}", handlers)
        self.assertIn("groups['k8s_cluster']", handlers)
        application_namespace = tasks.split(
            "- name: Reconcile application namespace without deleting retained state",
            maxsplit=1,
        )[1].split("- name:", maxsplit=1)[0]
        self.assertIn("pod-security.kubernetes.io/enforce: baseline", application_namespace)
        self.assertIn("pod-security.kubernetes.io/audit: restricted", application_namespace)
        self.assertIn("pod-security.kubernetes.io/warn: restricted", application_namespace)

    def test_platform_buildkit_prepares_the_router_owned_package_endpoint(self) -> None:
        tasks = (ROOT / "deploy/ansible/roles/platform_buildkit/tasks/main.yml").read_text(
            encoding="utf-8"
        )
        defaults = (
            ROOT / "deploy/ansible/roles/platform_buildkit/defaults/main.yml"
        ).read_text(encoding="utf-8")
        unit = (
            ROOT
            / "deploy/ansible/roles/platform_buildkit/templates/buildkit-port-forward.service.j2"
        ).read_text(encoding="utf-8")
        self.assertIn("PLATFORM_BUILDKIT_CONTROLLER_IDENTITY_INVALID", tasks)
        self.assertIn("tcp://127.0.0.1:1234", tasks)
        self.assertIn("groups['routers'] | first", defaults)
        self.assertNotIn("groups['edge_router']", defaults)
        self.assertIn("--server {{ platform_buildkit_controller_api_server }}", unit)
        self.assertIn("ProtectSystem=strict", unit)
        tunnel_tasks = tasks.split(
            "- name: Ensure the router-owned BuildKit tunnel is enabled and started",
            maxsplit=1,
        )[1].split("- name: Require the router-owned BuildKit endpoint", maxsplit=1)[0]
        self.assertIn("enabled: true", tunnel_tasks)
        self.assertIn("state: started", tunnel_tasks)
        self.assertIn("daemon_reload: true", tunnel_tasks)
        self.assertNotIn("state: absent", tasks)

    def test_cluster_addons_use_the_control_plane_kubeconfig(self) -> None:
        tasks = (ROOT / "deploy/ansible/roles/cluster_addons/tasks/main.yml").read_text(
            encoding="utf-8"
        )
        self.assertEqual(tasks.count("kubeconfig: /etc/kubernetes/admin.conf"), 3)
        self.assertEqual(tasks.count("KUBECONFIG: /etc/kubernetes/admin.conf"), 2)

    def test_platform_application_adopts_portal_route_and_shared_ssh_service(self) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        portal = (ROOT / "deploy/helm/labweaver/templates/portal-route.yaml").read_text(
            encoding="utf-8"
        )
        workloads = (
            ROOT / "deploy/helm/labweaver/templates/workloads.yaml"
        ).read_text(encoding="utf-8")
        network_policy = (
            ROOT / "deploy/helm/labweaver/templates/network-policy.yaml"
        ).read_text(encoding="utf-8")
        handlers = (
            ROOT / "deploy/ansible/roles/platform_application/handlers/main.yml"
        ).read_text(encoding="utf-8")
        self.assertIn("PLATFORM_APPLICATION_PORTAL_GATEWAY_CONFLICT", tasks)
        self.assertNotIn("PLATFORM_APPLICATION_SSH_ROUTE_INVALID", tasks)
        self.assertIn("Refresh retained router trust", tasks)
        self.assertIn("labweaver.io/gateway-routes: allowed", tasks)
        self.assertIn("kind: HTTPRoute", portal)
        self.assertIn("value: /connect", portal)
        self.assertIn("sshGatewayService.enabled", workloads)
        self.assertIn("metallb.io/allow-shared-ip", workloads)
        self.assertNotIn("metallb.io/loadBalancerIPs", workloads)
        self.assertIn("type: LoadBalancer", workloads)
        self.assertIn('eq $name "access-service"', network_policy)
        self.assertIn("port: 8080", network_policy)
        self.assertIn("groups['routers'] | first", tasks)
        self.assertIn("groups['routers'] | first", handlers)
        self.assertNotIn("groups['edge_router']", tasks + handlers)
        self.assertNotIn("state: absent", tasks)

    def test_platform_application_supports_ssh_on_the_existing_metallb_address(self) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        defaults = (
            ROOT / "deploy/ansible/roles/platform_application/defaults/main.yml"
        ).read_text(encoding="utf-8")

        self.assertIn("platform_application_ssh_load_balancer_ip", defaults)
        self.assertIn("Persist the MetalLB sharing key", tasks)
        self.assertIn("PLATFORM_APPLICATION_SSH_SHARED_IP_CONFLICT", tasks)
        self.assertIn("Wait for the OpenSSH Gateway shared load balancer address", tasks)
        self.assertIn("PLATFORM_APPLICATION_SSH_LOAD_BALANCER_INVALID", tasks)

    def test_cilium_ingress_identity_reaches_only_public_backends(self) -> None:
        policy = (
            ROOT / "deploy/helm/labweaver/templates/cilium-ingress-policy.yaml"
        ).read_text(encoding="utf-8")

        self.assertIn("name: gateway-backends", policy)
        self.assertIn("values: [web, access-service]", policy)
        self.assertIn("fromEntities: [ingress]", policy)
        self.assertIn('{port: "8080", protocol: TCP}', policy)
        self.assertIn("app.kubernetes.io/name: openssh-gateway", policy)
        self.assertIn("fromEntities: [world]", policy)
        self.assertIn('{port: "2222", protocol: TCP}', policy)

    def test_agent_runtime_egress_allows_all_destinations_and_ports(self) -> None:
        policy = (
            ROOT / "deploy/helm/labweaver/templates/network-policy.yaml"
        ).read_text(encoding="utf-8")

        self.assertIn('if eq $name "agent-service"', policy)
        self.assertIn("    - {}", policy)

    def test_platform_service_configs_use_declared_tls_secret_keys(self) -> None:
        manifest = json.loads(
            (ROOT / "deploy/config/platform-bundle-manifest.json").read_text(encoding="utf-8")
        )
        for service, config_map, example in (
            ("control-service-secrets", "control-service-config", "control-plane.yaml.example"),
            ("access-service-secrets", "access-service-config", "access-auth.yaml.example"),
            ("agent-service-secrets", "agent-service-config", "agent-control-plane.yaml.example"),
        ):
            configuration = (ROOT / "deploy/config" / example).read_text(encoding="utf-8")
            for locator in re.findall(r"/etc/labweaver/secrets/([a-z0-9.-]+)", configuration):
                self.assertIn(locator, manifest["secrets"][service])
            for locator in re.findall(r"/etc/labweaver/config/([a-z0-9.-]+)", configuration):
                self.assertIn(locator, manifest["configMaps"][config_map])

    def test_evaluation_freeze_worker_pull_secret_is_reconciled(self) -> None:
        manifest = json.loads(
            (ROOT / "deploy/config/platform-bundle-manifest.json").read_text(encoding="utf-8")
        )
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")

        self.assertIn(
            "registry-pull-config.json",
            manifest["secrets"]["evaluation-service-secrets"],
        )
        self.assertIn(
            "Reconcile the evaluation freeze worker registry pull secret",
            tasks,
        )
        self.assertIn(
            "PLATFORM_APPLICATION_EVALUATION_REGISTRY_PULL_CONFIG_INVALID",
            tasks,
        )
        self.assertIn("type: kubernetes.io/dockerconfigjson", tasks)

    def test_exact_retained_cdi_policy_is_adopted_before_helm(self) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")

        self.assertIn(
            "PLATFORM_APPLICATION_CDI_CLONE_SOURCE_POLICY_CONFLICT",
            tasks,
        )
        self.assertIn(
            "Adopt the exact retained CDI clone source network policy into the Helm release",
            tasks,
        )
        self.assertIn("meta.helm.sh/release-name", tasks)
        self.assertLess(
            tasks.index("Adopt the exact retained CDI clone source network policy"),
            tasks.index("Atomically deploy the immutable Sprint 2 profile"),
        )
        self.assertEqual(tasks.count("'--take-ownership'"), 2)

    def test_platform_application_uses_supported_nats_account_probe(self) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        probe = tasks.split("- name: Probe retained JetStream", maxsplit=1)[1].split(
            "- name: Inspect exact Sprint 2 streams", maxsplit=1
        )[0]

        self.assertIn("- account\n      - info", probe)
        self.assertNotIn("--json", probe)

    def test_platform_application_nats_administration_requires_mtls(self) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        nats_sections = [
            tasks.split(f"- name: {name}", maxsplit=1)[1].split("- name:", maxsplit=1)[0]
            for name in (
                "Probe retained JetStream",
                "Inspect exact Sprint 2 streams without mutation",
                "Create only missing Sprint 2 streams",
                "Read back exact Sprint 2 streams",
            )
        ]
        for section in nats_sections:
            self.assertIn("--creds", section)
            self.assertIn("--tlsca", section)
            self.assertIn("--tlscert", section)
            self.assertIn("--tlskey", section)

        defaults = (
            ROOT / "deploy/ansible/roles/platform_application/defaults/main.yml"
        ).read_text(encoding="utf-8")
        self.assertIn("platform_application_nats_client_certificate_file", defaults)
        self.assertIn("platform_application_nats_client_private_key_file", defaults)

    def test_platform_application_reads_locked_minio_versioning_shape(self) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        versioning = tasks.split(
            "- name: Verify immutable artifact bucket versioning", maxsplit=1
        )[1].split("- name:", maxsplit=1)[0]
        self.assertIn(".status != 'success'", versioning)
        self.assertIn(".versioning.status != 'Enabled'", versioning)
        self.assertNotIn("no_log: true", versioning)

    def test_platform_application_stages_keycloak_realm_on_execution_host(self) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        staging = tasks.split(
            "- name: Stage the reviewed Keycloak realm on the execution host", maxsplit=1
        )[1].split("- name:", maxsplit=1)[0]
        self.assertIn('src: "{{ platform_application_keycloak_realm_file }}"', staging)
        self.assertIn("keycloak-realm.json", staging)
        self.assertIn('mode: "0600"', staging)
        self.assertIn("no_log: true", staging)

        remote_path = (
            '"{{ platform_application_report_root }}/{{ platform_application_run_id }}/'
            'keycloak-realm.json"'
        )
        self.assertEqual(tasks.count(f"- {remote_path}"), 2)

    def test_platform_application_rejects_kcadm_http_errors_and_missing_authorization(self) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        authentication = tasks.split(
            "- name: Authenticate retained Keycloak administration", maxsplit=1
        )[1].split("- name:", maxsplit=1)[0]
        self.assertIn("platform_application_keycloak_authentication.stderr", authentication)
        self.assertIn("HTTP [45][0-9][0-9]", authentication)

        session_binding = tasks.split(
            "- name: Load the isolated Keycloak administration session", maxsplit=1
        )[1].split("- name: Require retained Keycloak realm-management authorization", maxsplit=1)[0]
        self.assertIn("platform_application_keycloak_session_file.content", session_binding)
        self.assertIn("platform_application_keycloak_admin_realm", session_binding)
        self.assertIn("PLATFORM_APPLICATION_KEYCLOAK_ADMIN_TOKEN_INVALID", session_binding)
        self.assertIn("no_log: true", session_binding)

        authorization = tasks.split(
            "- name: Require retained Keycloak realm-management authorization", maxsplit=1
        )[1].split("- name:", maxsplit=1)[0]
        self.assertIn("- get\n      - users", authorization)
        self.assertIn("platform_application_keycloak_admin_realm", authorization)
        self.assertIn("- --limit\n      - \"1\"", authorization)
        self.assertNotIn("--max-results", authorization)
        self.assertIn("HTTP [45][0-9][0-9]", authorization)
        self.assertIn("platform_application_keycloak_admin_token", authorization)
        self.assertIn("no_log: true", authorization)

        target_realm_commands = tasks.split(
            "- name: Inspect retained Sprint 2 Keycloak realm", maxsplit=1
        )[1].split("- name: Require the reviewed Sprint 2 identity surface", maxsplit=1)[0]
        self.assertGreaterEqual(
            target_realm_commands.count("platform_application_keycloak_admin_token"),
            7,
        )

    def test_platform_application_reconciles_only_reviewed_demo_identities(self) -> None:
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        identity_reconcile = tasks.split(
            "- name: Load the reviewed Sprint 2 identity seed", maxsplit=1
        )[1].split(
            "- name: Require the reviewed Sprint 2 identity surface", maxsplit=1
        )[0]

        self.assertIn("PLATFORM_APPLICATION_KEYCLOAK_SEED_INVALID", identity_reconcile)
        self.assertIn("platform_application_keycloak_required_users", identity_reconcile)
        self.assertIn("clients/{{", identity_reconcile)
        self.assertIn("reset-password", identity_reconcile)
        self.assertIn("firstName", identity_reconcile)
        self.assertIn("requiredActions", identity_reconcile)
        self.assertEqual(identity_reconcile.count("status_code: 204"), 3)
        self.assertEqual(identity_reconcile.count("ca_path:"), 3)
        self.assertNotIn("delete", identity_reconcile.lower())
        self.assertGreaterEqual(identity_reconcile.count("no_log: true"), 5)

    def test_platform_runtime_authorities_match_control_policy(self) -> None:
        control = (ROOT / "deploy/config/control-plane.yaml.example").read_text(encoding="utf-8")
        providers = json.loads(
            (ROOT / "deploy/config/environment-providers.json.example").read_text(
                encoding="utf-8"
            )
        )
        policy_id = re.search(r'imagePolicyId: "([0-9a-f-]+)"', control)
        self.assertIsNotNone(policy_id)
        container = next(provider for provider in providers if provider["providerKind"] == "container")
        virtual_machine = next(
            provider for provider in providers if provider["providerKind"] == "kubevirt"
        )
        self.assertEqual(container["activeImagePolicyId"], policy_id.group(1))
        self.assertNotIn("activeImagePolicyId", virtual_machine)
        self.assertNotIn("activeImagePolicyRevision", virtual_machine)

        control_config = yaml.safe_load(control)
        lock = yaml.safe_load((ROOT / "deploy/versions.lock.yml").read_text(encoding="utf-8"))
        vm_policy = control_config["control"]["virtualMachineBase"]
        vm_lock = lock["platform_vm_base"]
        self.assertEqual(vm_policy["providerBinding"], virtual_machine["binding"])
        self.assertEqual(vm_policy["storageClassBinding"], virtual_machine["storageClassBinding"])
        self.assertEqual(vm_policy["artifactId"], vm_lock["artifact_id"])
        self.assertEqual(vm_policy["baseDisk"]["binding"], vm_lock["binding"])
        self.assertEqual(vm_policy["baseDisk"]["sourceRegistryDigest"], vm_lock["registry_url"])
        self.assertEqual(vm_policy["baseDisk"]["diskSha256"], vm_lock["disk_sha256"])
        self.assertEqual(vm_policy["baseDisk"]["capacityBytes"], vm_lock["capacity_bytes"])

    def test_control_quarantine_subjects_belong_to_the_retained_agent_stream(self) -> None:
        control = yaml.safe_load(
            (ROOT / "deploy/config/control-plane.yaml.example").read_text(encoding="utf-8")
        )
        tasks = (
            ROOT / "deploy/ansible/roles/platform_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        defaults = (
            ROOT / "deploy/ansible/roles/platform_application/defaults/main.yml"
        ).read_text(encoding="utf-8")

        self.assertEqual(control["nats"]["stream_name"], "LABWEAVER_AGENT_EVENTS")
        for field in ("quarantine_subject", "build_quarantine_subject"):
            self.assertTrue(control["nats"][field].startswith("labweaver.agent."))
        self.assertNotEqual(
            control["nats"]["quarantine_subject"],
            control["nats"]["build_quarantine_subject"],
        )
        self.assertIn("PLATFORM_APPLICATION_CONTROL_QUARANTINE_STREAM_MISMATCH", tasks)
        self.assertIn("validate_nats_user_credentials.py", defaults)
        self.assertIn("platform_application_control_nats_credentials", tasks)

    def test_environment_quarantine_subjects_are_retained_by_application_streams(self) -> None:
        defaults = yaml.safe_load(
            (
                ROOT
                / "deploy/ansible/roles/platform_application/defaults/main.yml"
            ).read_text(encoding="utf-8")
        )
        streams = {
            stream["name"]: stream
            for stream in defaults["platform_application_nats_streams"]
        }
        self.assertIn(
            "labweaver.environment.command.quarantine.v1",
            streams["LABWEAVER_ENVIRONMENT_COMMANDS"]["subjects"],
        )
        self.assertIn(
            "labweaver.environment.release.quarantine.v1",
            streams["LABWEAVER_RELEASES"]["subjects"],
        )

    def test_kubevirt_executor_can_apply_its_planned_resource_quota(self) -> None:
        service_account = (
            ROOT / "deploy/helm/labweaver/templates/service-account.yaml"
        ).read_text(encoding="utf-8")
        kubevirt_profile = service_account.split(
            '{{- else if eq $configuration.rbacProfile "kubevirt" }}',
            maxsplit=1,
        )[1].split(
            '{{- else if eq $configuration.rbacProfile "evaluation" }}',
            maxsplit=1,
        )[0]
        self.assertIn('"resourcequotas"', kubevirt_profile)
        self.assertIn('resources: ["datavolumes/source"]', service_account)
        self.assertIn("name: {{ $name }}-datasource", service_account)
        self.assertIn("kind: RoleBinding", service_account)

    def test_kubevirt_console_executor_has_only_fixed_runtime_read_access(self) -> None:
        service_account = (
            ROOT / "deploy/helm/labweaver/templates/service-account.yaml"
        ).read_text(encoding="utf-8")
        profile = service_account.split(
            '{{- else if eq $configuration.rbacProfile "kubevirt-console" }}',
            maxsplit=1,
        )[1].split(
            '{{- else if eq $configuration.rbacProfile "evaluation" }}',
            maxsplit=1,
        )[0]
        self.assertIn('resources: ["virtualmachineinstances"]', profile)
        self.assertIn('resources: ["virtualmachineinstances/vnc"]', profile)
        self.assertEqual(profile.count('resourceNames: ["runtime"]'), 2)
        self.assertEqual(profile.count('verbs: ["get"]'), 2)
        self.assertNotIn("secrets", profile)
        self.assertNotIn("pods", profile)
        self.assertNotIn("virtualmachines/start", profile)

        network_policy = (
            ROOT / "deploy/helm/labweaver/templates/network-policy.yaml"
        ).read_text(encoding="utf-8")
        console_policy = network_policy.split(
            '{{- else if eq $name "kubevirt-console-executor" }}',
            maxsplit=1,
        )[1].split('{{- else }}', maxsplit=1)[0]
        self.assertIn("app.kubernetes.io/name: environment-service", console_policy)
        self.assertIn("port: 9451", console_policy)
        self.assertNotIn("port: 8089", console_policy)

    def test_cdi_clone_network_is_bounded_to_dns_and_upload_server(self) -> None:
        network_policy = (
            ROOT / "deploy/helm/labweaver/templates/network-policy.yaml"
        ).read_text(encoding="utf-8")
        self.assertIn("name: cdi-clone-source", network_policy)
        self.assertIn("cdi.kubevirt.io: cdi-clone-source", network_policy)
        self.assertIn("cdi.kubevirt.io: cdi-upload-server", network_policy)
        self.assertIn("{protocol: TCP, port: 8443}", network_policy)
        self.assertIn("{protocol: UDP, port: 53}", network_policy)

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
        self.assertIn("Wait for the Harbor Trivy StatefulSet object before applying the NFS policy", harbor)
        self.assertIn("fsGroupChangePolicy: OnRootMismatch", harbor)
        self.assertIn("Reconcile the Harbor Trivy NFS fsGroup policy before readiness", harbor)
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
        self.assertIn('get "realms/{{ identity_realm }}"', provision_job)
        self.assertIn('create realms', provision_job)
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
        self.assertNotIn("'labweaver-demo'", operator_rbac)
        self.assertIn("[identity_namespace, 'harbor']", operator_rbac)
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

    def test_platform_reset_is_identity_bound_and_fail_closed(self) -> None:
        playbook = (ROOT / "deploy/ansible/playbooks/93-platform-reset.yml").read_text(
            encoding="utf-8"
        )
        tasks = (ROOT / "deploy/ansible/roles/platform_reset/tasks/main.yml").read_text(
            encoding="utf-8"
        )
        baseline = (
            ROOT / "deploy/ansible/roles/platform_reset/templates/baseline.sql.j2"
        ).read_text(encoding="utf-8")
        xtask = (ROOT / "xtask/src/main.rs").read_text(encoding="utf-8")
        self.assertEqual(playbook.splitlines()[1], "- import_playbook: 00-preflight.yml")
        self.assertIn("labweaver_preflight_scope: platform-reset", playbook)
        self.assertIn("destroy-pre-release-data:", tasks)
        self.assertIn("platform_reset_cluster_uid", tasks)
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
        self.assertIn("PLATFORM_ATOMIC_ROLLBACK_FAILED", tasks)
        self.assertIn("Delete exact residual Kyverno CRDs", tasks)
        self.assertIn("Require the exact Sprint 2 deployment set", tasks)
        self.assertIn("platform_reset_migration_catalog_stat", tasks)
        self.assertIn("platform_reset_inventory_stat", tasks)
        self.assertIn("PLATFORM_CONFIGURATION_BUNDLE_INVALID", tasks)
        self.assertIn("Apply the reviewed workload configuration bundle", tasks)
        self.assertIn("kubernetes.core.k8s", tasks)
        self.assertNotIn("configuration_bundle", baseline)
        self.assertIn("DROP SCHEMA IF EXISTS", baseline)
        self.assertIn("0001_platform_baseline.sql", baseline)
        self.assertEqual(baseline.count("0001_roles_and_schemas.sql"), 2)
        bootstrap = (ROOT / "migrations/bootstrap/0001_roles_and_schemas.sql").read_text(
            encoding="utf-8"
        )
        self.assertIn("current_user = 'postgres-admin'", bootstrap)
        self.assertIn("GRANT %I TO %I", bootstrap)
        self.assertIn("SET ROLE lw_{{ domain }}_owner", baseline)
        self.assertIn("schema_migrations", baseline)
        self.assertIn("catalog_sha256", baseline)
        self.assertIn('run_infrastructure(&args.env, "93-platform-reset.yml"', xtask)
        self.assertNotIn("ansible.builtin.shell", tasks)


if __name__ == "__main__":
    unittest.main()
