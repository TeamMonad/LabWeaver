"""Rendered checks for the optional cluster-internal FastAPI-DLS addon."""

from pathlib import Path
import re
import unittest

import yaml
from jinja2 import Environment, FileSystemLoader, StrictUndefined

try:
    from ansible.parsing.dataloader import DataLoader
    from ansible.template import Templar
except (ImportError, ModuleNotFoundError):
    DataLoader = None
    Templar = None


ROOT = Path(__file__).resolve().parents[2]


def load_yaml(path: Path):
    return yaml.safe_load(path.read_text(encoding="utf-8"))


def task_named(tasks, name):
    return next(task for task in tasks if task.get("name") == name)


def render_file(path: Path, **values):
    environment = Environment(
        loader=FileSystemLoader(str(path.parent)),
        undefined=StrictUndefined,
    )
    return environment.get_template(path.name).render(**values)


def render_definition(definition, **values):
    if Templar is None or DataLoader is None:
        raise unittest.SkipTest("ansible-core is required for definition rendering")
    templar = Templar(loader=DataLoader(), variables=values)

    def render(value):
        if isinstance(value, dict):
            return {key: render(item) for key, item in value.items()}
        if isinstance(value, list):
            return [render(item) for item in value]
        if not isinstance(value, str):
            return value
        return templar.template(value, fail_on_undefined=True)

    return render(definition)


class FastapiDlsDeploymentTests(unittest.TestCase):
    def setUp(self) -> None:
        self.defaults = load_yaml(ROOT / "deploy/ansible/roles/fastapi_dls/defaults/main.yml")
        self.tasks = load_yaml(ROOT / "deploy/ansible/roles/fastapi_dls/tasks/main.yml")

    def test_dls_deployment_renders_pinned_stateful_runtime(self) -> None:
        deployment_task = task_named(self.tasks, "Reconcile the FastAPI-DLS Deployment")
        deployment = render_definition(
            deployment_task["kubernetes.core.k8s"]["definition"],
            fastapi_dls_service_name="fastapi-dls",
            fastapi_dls_namespace="labweaver-gpu-license",
            fastapi_dls_image=self.defaults["fastapi_dls_image"],
            fastapi_dls_proxy_image=self.defaults["fastapi_dls_proxy_image"],
            fastapi_dls_timezone="UTC",
            fastapi_dls_url="fastapi-dls.labweaver-gpu-license.svc.cluster.local",
            fastapi_dls_service_port=443,
            fastapi_dls_lease_expire_days=90,
            fastapi_dls_token_expire_days=1,
            fastapi_dls_instance_ref="00000000-0000-0000-0000-000000000001",
            fastapi_dls_database_url="sqlite:////app/database/db.sqlite",
            fastapi_dls_debug=False,
            fastapi_dls_data_pvc_name="fastapi-dls-data",
            fastapi_dls_tls_secret_name="fastapi-dls-tls",
        )
        containers = deployment["spec"]["template"]["spec"]["containers"]
        self.assertFalse(deployment["spec"]["template"]["spec"]["automountServiceAccountToken"])
        self.assertEqual(
            deployment["spec"]["strategy"],
            {"type": "Recreate", "rollingUpdate": None},
        )
        backend = next(container for container in containers if container["name"] == "fastapi-dls")
        proxy = next(container for container in containers if container["name"] == "tls-proxy")
        env = {item["name"]: item["value"] for item in backend["env"]}
        mounts = {mount["mountPath"]: mount for mount in backend["volumeMounts"]}

        self.assertRegex(backend["image"], r"@sha256:[0-9a-f]{64}$")
        self.assertRegex(proxy["image"], r"@sha256:[0-9a-f]{64}$")
        self.assertEqual(env["DLS_URL"], "fastapi-dls.labweaver-gpu-license.svc.cluster.local")
        self.assertEqual(env["DLS_PORT"], "443")
        self.assertEqual(env["DATABASE"], "sqlite:////app/database/db.sqlite")
        self.assertEqual(mounts["/app/database"]["name"], "data")
        self.assertEqual(mounts["/app/cert"]["name"], "data")
        self.assertEqual(
            backend["resources"],
            {
                "requests": {"cpu": "50m", "memory": "128Mi"},
                "limits": {"cpu": "500m", "memory": "512Mi"},
            },
        )
        self.assertEqual(
            proxy["ports"][0],
            {"name": "https-proxy", "containerPort": 8443, "protocol": "TCP"},
        )
        self.assertEqual(
            proxy["resources"],
            {
                "requests": {"cpu": "10m", "memory": "16Mi"},
                "limits": {"cpu": "100m", "memory": "64Mi"},
            },
        )
        self.assertEqual(backend["command"][2:4], ["--host", "127.0.0.1"])
        self.assertEqual(backend["command"][4:6], ["--port", "8080"])
        for probe_name in ("readinessProbe", "livenessProbe"):
            probe = backend[probe_name]
            self.assertIn("exec", probe)
            self.assertNotIn("tcpSocket", probe)
            command = probe["exec"]["command"]
            self.assertEqual(command[0], "python")
            self.assertIn("127.0.0.1:8080/-/health", command[2])

    def test_dls_service_proxy_and_network_policy_render_restricted_paths(self) -> None:
        service_task = task_named(self.tasks, "Reconcile the cluster-internal FastAPI-DLS Service")
        service = render_definition(
            service_task["kubernetes.core.k8s"]["definition"],
            fastapi_dls_service_name="fastapi-dls",
            fastapi_dls_namespace="labweaver-gpu-license",
            fastapi_dls_service_port=443,
        )
        proxy = render_file(ROOT / "deploy/ansible/roles/fastapi_dls/templates/nginx.conf.j2")
        policy = yaml.safe_load(
            render_file(
                ROOT / "deploy/ansible/roles/fastapi_dls/templates/network-policy.yml.j2",
                fastapi_dls_namespace="labweaver-gpu-license",
                fastapi_dls_allowed_client_namespace_selector={
                    "labweaver.io/managed": "true",
                    "labweaver.io/environment": "true",
                    "app.kubernetes.io/name": "labweaver-vm-runtime",
                },
                fastapi_dls_allowed_client_pod_selector={
                    "app": "runtime",
                    "labweaver.io/gpu-mode": "vm_vgpu",
                },
            )
        )

        self.assertEqual(service["spec"]["type"], "ClusterIP")
        self.assertEqual(service["spec"]["ports"][0]["port"], 443)
        self.assertEqual(service["spec"]["ports"][0]["targetPort"], "https-proxy")
        self.assertRegex(proxy, r"pid /tmp/nginx\.pid;")
        for temp_path in (
            "/tmp/client_temp",
            "/tmp/proxy_temp_path",
            "/tmp/fastcgi_temp",
            "/tmp/uwsgi_temp",
            "/tmp/scgi_temp",
        ):
            self.assertIn(temp_path, proxy)
        self.assertRegex(proxy, r"listen\s+8443\s+ssl;")
        self.assertRegex(
            proxy,
            r"location \^~ /auth/v1/ \{[\s\S]*?proxy_pass http://127\.0\.0\.1:8080;",
        )
        self.assertRegex(
            proxy,
            r"location \^~ /leasing/v1/ \{[\s\S]*?proxy_pass http://127\.0\.0\.1:8080;",
        )
        self.assertRegex(proxy, r"location / \{\s*return 404;\s*\}")
        self.assertNotRegex(proxy, r"location\s+\^~\s+/-/")

        ingress = policy["spec"]["ingress"]
        self.assertEqual(len(ingress), 1)
        source = ingress[0]["from"][0]
        self.assertEqual(
            source["namespaceSelector"]["matchLabels"],
            {
                "labweaver.io/managed": "true",
                "labweaver.io/environment": "true",
                "app.kubernetes.io/name": "labweaver-vm-runtime",
            },
        )
        self.assertEqual(
            source["podSelector"]["matchLabels"],
            {"app": "runtime", "labweaver.io/gpu-mode": "vm_vgpu"},
        )
        self.assertEqual(ingress[0]["ports"], [{"port": 8443, "protocol": "TCP"}])

    def test_guest_configuration_renders_references_without_token_material(self) -> None:
        config_task = task_named(
            self.tasks,
            "Reconcile the FastAPI-DLS guest client configuration references",
        )
        config = render_definition(
            config_task["kubernetes.core.k8s"]["definition"],
            fastapi_dls_namespace="labweaver-gpu-license",
            fastapi_dls_client_config_name="fastapi-dls-client",
            fastapi_dls_client_license_url="",
            fastapi_dls_url="fastapi-dls.labweaver-gpu-license.svc.cluster.local",
            fastapi_dls_service_port=443,
            fastapi_dls_client_token_secret_name="fastapi-dls-client-token",
            fastapi_dls_client_token_secret_namespace="labweaver-gpu-license",
            fastapi_dls_client_token_secret_key="client-token",
            fastapi_dls_tls_secret_name="fastapi-dls-tls",
            fastapi_dls_signing_root_secret_name="fastapi-dls-signing-root",
            fastapi_dls_signing_root_secret_namespace="labweaver-gpu-license",
            fastapi_dls_signing_root_secret_key="ca.crt",
        )
        data = config["data"]

        self.assertEqual(
            data["licenseUrl"],
            "https://fastapi-dls.labweaver-gpu-license.svc.cluster.local:443",
        )
        self.assertEqual(data["tokenSecretName"], "fastapi-dls-client-token")
        self.assertEqual(data["tokenSecretNamespace"], "labweaver-gpu-license")
        self.assertEqual(data["tokenSecretKey"], "client-token")
        self.assertEqual(data["caSecretName"], "fastapi-dls-tls")
        self.assertEqual(data["caSecretKey"], "ca.crt")
        self.assertEqual(data["signingRootSecretName"], "fastapi-dls-signing-root")
        self.assertEqual(data["signingRootSecretNamespace"], "labweaver-gpu-license")
        self.assertEqual(data["signingRootSecretKey"], "ca.crt")
        self.assertNotIn("tokenSecretValue", data)

    def test_dls_credentials_are_reconciled_after_backend_readiness(self) -> None:
        self.assertEqual(self.defaults["fastapi_dls_token_expire_days"], 1)
        self.assertEqual(
            self.defaults["fastapi_dls_client_token_secret_name"],
            "fastapi-dls-client-token",
        )
        self.assertEqual(
            self.defaults["fastapi_dls_signing_root_secret_name"],
            "fastapi-dls-signing-root",
        )
        names = [task["name"] for task in self.tasks]
        self.assertLess(
            names.index("Reconcile the FastAPI-DLS Deployment"),
            names.index("Wait for the FastAPI-DLS backend to become ready"),
        )
        for task_name in (
            "Read the retained FastAPI-DLS client token Secret",
            "Generate a candidate FastAPI-DLS client token",
            "Determine whether the retained FastAPI-DLS client token needs refresh",
            "Validate the retained FastAPI-DLS client token shape",
            "Determine whether the retained FastAPI-DLS client token identity is current",
            "Read the retained FastAPI-DLS signing root",
            "Reconcile the FastAPI-DLS client token Secret",
            "Reconcile the FastAPI-DLS signing root Secret",
        ):
            task = task_named(self.tasks, task_name)
            self.assertTrue(task["no_log"])
        token_task = task_named(self.tasks, "Generate a candidate FastAPI-DLS client token")
        self.assertIn(
            "127.0.0.1:8080/-/client-token",
            token_task["kubernetes.core.k8s_exec"]["command"],
        )
        self.assertFalse(token_task["changed_when"])
        self.assertIn("DLS client token URL mismatch", token_task["kubernetes.core.k8s_exec"]["command"])
        token_secret_task = task_named(
            self.tasks, "Reconcile the FastAPI-DLS client token Secret"
        )
        self.assertIn(
            "fastapi_dls_client_token_candidate_exec.stdout",
            token_secret_task["kubernetes.core.k8s"]["definition"]["data"],
        )
        root_task = task_named(self.tasks, "Read the retained FastAPI-DLS signing root")
        self.assertEqual(
            root_task["kubernetes.core.k8s_exec"]["command"],
            "cat /app/cert/root_certificate.pem",
        )

    def test_tls_bootstrap_always_removes_temporary_material(self) -> None:
        bootstrap = task_named(self.tasks, "Bootstrap FastAPI-DLS TLS material")
        cleanup = bootstrap["always"][0]

        self.assertEqual(cleanup["name"], "Remove the temporary FastAPI-DLS TLS material")
        self.assertEqual(cleanup["ansible.builtin.file"]["state"], "absent")
        self.assertEqual(cleanup["ansible.builtin.file"]["path"], "{{ fastapi_dls_tls_temp.path }}")


if __name__ == "__main__":
    unittest.main()
