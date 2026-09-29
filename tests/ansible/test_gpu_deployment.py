"""Rendered checks for the opt-in GPU deployment roles."""

from pathlib import Path
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


def render_ansible_value(value, **values):
    """Render a scalar through Ansible's own native templating path."""

    if Templar is None or DataLoader is None:
        raise unittest.SkipTest("ansible-core is required for definition rendering")
    return Templar(loader=DataLoader(), variables=values).template(
        value,
        fail_on_undefined=True,
    )


class GpuDeploymentTests(unittest.TestCase):
    def test_device_plugin_renders_writable_termination_and_read_only_devices(self) -> None:
        defaults = load_yaml(
            ROOT / "deploy/ansible/roles/gpu_device_plugin/defaults/main.yml"
        )
        tasks = load_yaml(ROOT / "deploy/ansible/roles/gpu_device_plugin/tasks/main.yml")
        template = render_file(
            ROOT
            / "deploy/ansible/roles/gpu_device_plugin/templates/device-plugin-daemonset.yml.j2",
            gpu_device_plugin_mode="exclusive",
            gpu_device_plugin_namespace="kube-system",
            gpu_device_plugin_node_label="labweaver.io/gpu-mode",
            gpu_device_plugin_config_name="nvidia-gpu-device-plugin-exclusive-config",
            gpu_device_plugin_image=defaults["gpu_device_plugin_image"],
            gpu_device_plugin_device_discovery_strategy="nvml",
            gpu_device_plugin_device_list_strategy="cdi-annotations",
            gpu_device_plugin_cdi_path="/var/run/cdi",
            gpu_device_plugin_driver_root="/run/nvidia/driver",
            gpu_device_plugin_termination_message_path="/var/lib/nvidia-device-plugin/termination-log",
        )
        daemonset = yaml.safe_load(template)
        pod = daemonset["spec"]["template"]["spec"]
        container = pod["containers"][0]
        mounts = {mount["name"]: mount for mount in container["volumeMounts"]}
        volumes = {volume["name"]: volume for volume in pod["volumes"]}
        environment = {item["name"]: item["value"] for item in container["env"]}

        self.assertEqual(defaults["gpu_device_plugin_exclusive_resource_name"], "nvidia.com/gpu")
        self.assertEqual(defaults["gpu_device_plugin_shared_resource_name"], "nvidia.com/gpu.shared")
        self.assertRegex(
            defaults["gpu_device_plugin_image"],
            r"^nvcr\.io/nvidia/k8s-device-plugin:v0\.17\.0@sha256:[0-9a-f]{64}$",
        )
        self.assertNotEqual(
            defaults["gpu_device_plugin_exclusive_resource_name"],
            defaults["gpu_device_plugin_shared_resource_name"],
        )
        self.assertEqual(
            container["terminationMessagePath"],
            defaults["gpu_device_plugin_termination_message_path"],
        )
        self.assertEqual(
            mounts["termination-log"]["mountPath"],
            "/var/lib/nvidia-device-plugin",
        )
        self.assertEqual(volumes["termination-log"]["emptyDir"], {})
        self.assertEqual(mounts["dev"]["mountPath"], "/dev")
        self.assertTrue(mounts["dev"]["readOnly"])
        self.assertEqual(pod["runtimeClassName"], "nvidia")
        self.assertEqual(mounts["sys-pci-devices"]["mountPath"], "/sys/bus/pci/devices")
        self.assertTrue(mounts["sys-pci-devices"]["readOnly"])
        self.assertEqual(environment["NVIDIA_DRIVER_ROOT"], "/run/nvidia/driver")
        self.assertEqual(mounts["driver-root"]["mountPath"], "/driver-root")
        self.assertTrue(mounts["driver-root"]["readOnly"])
        self.assertEqual(volumes["driver-root"]["hostPath"]["path"], "/run/nvidia/driver")
        self.assertEqual(volumes["cdi"]["hostPath"]["type"], "DirectoryOrCreate")
        self.assertNotIn("readOnly", mounts["cdi"])
        self.assertNotIn("privileged", container["securityContext"])

        cleanup = task_named(tasks, "Remove stale GPU mode labels owned by this role")
        cleanup_patch = cleanup["kubernetes.core.k8s_json_patch"]["patch"]
        read_managed = task_named(tasks, "Read nodes previously labelled by this GPU device-plugin role")
        self.assertEqual(
            read_managed["kubernetes.core.k8s_info"]["label_selectors"],
            ["labweaver.io/gpu-plugin-managed=true"],
        )
        self.assertEqual(
            cleanup_patch[1]["path"],
            "/metadata/labels/labweaver.io~1gpu-plugin-managed",
        )
        exclusive = task_named(tasks, "Label nodes selected for exclusive GPU allocation")
        shared = task_named(tasks, "Label nodes selected for shared GPU allocation")
        exclusive_labels = render_ansible_value(
            exclusive["kubernetes.core.k8s"]["definition"]["metadata"]["labels"],
            gpu_device_plugin_node_label="labweaver.io/gpu-mode",
        )
        shared_labels = render_ansible_value(
            shared["kubernetes.core.k8s"]["definition"]["metadata"]["labels"],
            gpu_device_plugin_node_label="labweaver.io/gpu-mode",
        )
        self.assertEqual(
            exclusive_labels,
            {
                "labweaver.io/gpu-mode": "exclusive",
                "labweaver.io/gpu-plugin-managed": "true",
            },
        )
        self.assertEqual(
            shared_labels,
            {
                "labweaver.io/gpu-mode": "shared",
                "labweaver.io/gpu-plugin-managed": "true",
            },
        )

    def test_device_plugin_shared_config_renders_time_slicing(self) -> None:
        tasks = load_yaml(ROOT / "deploy/ansible/roles/gpu_device_plugin/tasks/main.yml")
        shared_task = task_named(tasks, "Reconcile shared NVIDIA device-plugin configuration")
        config_text = shared_task["kubernetes.core.k8s"]["definition"]["data"]["config.yaml"]
        config = yaml.safe_load(
            Environment(undefined=StrictUndefined)
            .from_string(config_text)
            .render(
                gpu_device_plugin_exclusive_resource_name="nvidia.com/gpu",
                gpu_device_plugin_shared_replicas=10,
            )
        )

        self.assertEqual(config["version"], "v1")
        sharing = config["sharing"]["timeSlicing"]
        self.assertTrue(sharing["renameByDefault"])
        self.assertEqual(
            sharing["resources"],
            [{"name": "nvidia.com/gpu", "replicas": 10}],
        )

    def test_kubevirt_mdev_configuration_renders_node_scoped_api_shape(self) -> None:
        defaults = load_yaml(ROOT / "deploy/ansible/roles/kubevirt/defaults/main.yml")
        tasks = load_yaml(ROOT / "deploy/ansible/roles/kubevirt/tasks/main.yml")
        baseline = task_named(tasks, "Forbid software emulation for KubeVirt workloads")
        configure = task_named(tasks, "Configure explicitly selected KubeVirt mediated devices")
        self.assertNotIn("apply", baseline["kubernetes.core.k8s"])
        definition = configure["kubernetes.core.k8s"]["definition"]
        payload = render_definition(
            definition,
            kubevirt_permitted_mediated_devices=[
                {
                    "mdevNameSelector": "nvidia-195",
                    "resourceName": "nvidia.com/grid-v100dx-2q",
                    "externalResourceProvider": True,
                }
            ],
            kubevirt_mediated_device_types=[],
            kubevirt_mediated_devices_configuration=[
                {
                    "nodeSelector": {"labweaver.io/gpu-vgpu": "v100"},
                    "mediatedDeviceTypes": ["nvidia-195"],
                }
            ],
        )
        configuration = payload["spec"]["configuration"]

        self.assertEqual(defaults["kubevirt_permitted_mediated_devices"], [])
        self.assertEqual(defaults["kubevirt_mediated_device_types"], [])
        self.assertEqual(
            configuration["permittedHostDevices"]["mediatedDevices"][0]["resourceName"],
            "nvidia.com/grid-v100dx-2q",
        )
        self.assertEqual(
            configuration["mediatedDevicesConfiguration"],
            {
                "mediatedDeviceTypes": [],
                "nodeMediatedDeviceTypes": [
                    {
                        "nodeSelector": {"labweaver.io/gpu-vgpu": "v100"},
                        "mediatedDeviceTypes": ["nvidia-195"],
                    }
                ]
            },
        )
        self.assertNotIn("apply", configure["kubernetes.core.k8s"])
        self.assertIn(
            "kubevirt_permitted_mediated_devices | length > 0",
            configure["when"],
        )

    def test_resource_catalog_uses_the_device_plugin_extended_resource(self) -> None:
        lock = load_yaml(ROOT / "deploy/versions.lock.yml")
        capacity = load_yaml(ROOT / "deploy/config/resource-capacity.json.example")

        self.assertEqual(lock["platform_gpu_classes"][0]["allocation_binding"], "nvidia.com/gpu")
        self.assertEqual(capacity["gpuCatalogSeed"][0]["allocationBinding"], "nvidia.com/gpu")


if __name__ == "__main__":
    unittest.main()
