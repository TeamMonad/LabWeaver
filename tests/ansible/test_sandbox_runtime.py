"""Behavioural tests for the sandbox runtime wrapper and its annotation shim.

gVisor's OCI runtime reads the CRI-standard container annotations that containerd
writes, while CRI-O writes its own `io.kubernetes.cri-o.*` names. The wrapper is
the sandbox handler's entry point, so these tests render it exactly as Ansible
does and assert the bundle it hands to `runsc`.
"""

from __future__ import annotations

import json
import os
import shlex
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

from jinja2 import Environment, StrictUndefined


ROOT = Path(__file__).resolve().parents[2]
ROLE = ROOT / "deploy" / "ansible" / "roles" / "sandbox_runtime"
WRAPPER = ROLE / "templates" / "labweaver-sandbox-runtime.j2"
TRANSLATOR = ROLE / "files" / "translate-cri-annotations.py"

CRI_O_PAUSE_ANNOTATIONS = {
    "io.kubernetes.cri-o.ContainerType": "sandbox",
    "io.kubernetes.cri-o.SandboxID": "0123456789abcdef",
    "io.kubernetes.cri-o.SandboxName": "lab-job-pod",
    "io.kubernetes.cri-o.Namespace": "labweaver-evaluation",
    "io.kubernetes.cri-o.ContainerName": "k8s_POD_lab-job-pod",
    "io.kubernetes.cri-o.RuntimeHandler": "labweaver-sandbox",
    "io.kubernetes.pod.name": "lab-job-pod",
}


class SandboxRuntimeWrapperTest(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.install = self.root / "install"
        self.install.mkdir()
        shutil.copy2(TRANSLATOR, self.install / TRANSLATOR.name)
        self.runsc = self.install / "runsc"
        self.runsc.write_text(
            '#!/bin/sh\necho "runsc $*"\n', encoding="utf-8"
        )
        self.runsc.chmod(self.runsc.stat().st_mode | stat.S_IEXEC)
        environment = Environment(undefined=StrictUndefined, keep_trailing_newline=True)
        environment.filters["quote"] = lambda value: shlex.quote(str(value))
        rendered = environment.from_string(WRAPPER.read_text(encoding="utf-8")).render(
            sandbox_runtime_install_dir=str(self.install)
        )
        self.wrapper = self.root / "labweaver-sandbox-runtime"
        self.wrapper.write_text(rendered, encoding="utf-8")
        self.wrapper.chmod(0o755)

    def bundle(self, name: str, annotations: dict[str, str]) -> Path:
        directory = self.root / name
        directory.mkdir()
        (directory / "config.json").write_text(
            json.dumps({"annotations": annotations, "process": {"args": ["/pause"]}}),
            encoding="utf-8",
        )
        return directory

    def run_wrapper(self, bundle: Path) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                str(self.wrapper),
                "--systemd-cgroup",
                "--root",
                "/run/crun",
                "create",
                "--bundle",
                str(bundle),
                "--pid-file",
                str(self.root / "pid"),
                "0123456789abcdef",
            ],
            capture_output=True,
            text=True,
            check=False,
            env={"PATH": os.environ.get("PATH", "/usr/bin:/bin")},
        )

    def read_annotations(self, bundle: Path) -> dict[str, str]:
        return json.loads((bundle / "config.json").read_text(encoding="utf-8"))[
            "annotations"
        ]

    def test_cri_o_annotations_reach_gvisor_under_their_standard_names(self) -> None:
        bundle = self.bundle("sandbox", dict(CRI_O_PAUSE_ANNOTATIONS))
        result = self.run_wrapper(bundle)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(result.stdout.startswith("runsc "), result.stdout)
        annotations = self.read_annotations(bundle)
        self.assertEqual(annotations["io.kubernetes.cri.container-type"], "sandbox")
        self.assertEqual(
            annotations["io.kubernetes.cri.sandbox-id"], "0123456789abcdef"
        )
        self.assertEqual(
            annotations["io.kubernetes.cri.sandbox-namespace"], "labweaver-evaluation"
        )
        self.assertEqual(
            annotations["io.kubernetes.cri.sandbox-name"], "lab-job-pod"
        )
        self.assertEqual(
            annotations["io.kubernetes.cri.container-name"], "k8s_POD_lab-job-pod"
        )
        # CRI-O's own names stay in place: the runtime handler annotation and the
        # rest of the spec must survive untouched.
        self.assertEqual(
            annotations["io.kubernetes.cri-o.RuntimeHandler"], "labweaver-sandbox"
        )
        self.assertEqual(annotations["io.kubernetes.pod.name"], "lab-job-pod")

    def test_an_existing_standard_annotation_is_never_overwritten(self) -> None:
        annotations = dict(CRI_O_PAUSE_ANNOTATIONS)
        annotations["io.kubernetes.cri.sandbox-id"] = "from-the-cri"
        bundle = self.bundle("existing", annotations)
        result = self.run_wrapper(bundle)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            self.read_annotations(bundle)["io.kubernetes.cri.sandbox-id"], "from-the-cri"
        )

    def test_a_bundle_without_annotations_is_left_alone(self) -> None:
        bundle = self.bundle("plain", {})
        result = self.run_wrapper(bundle)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.read_annotations(bundle), {})

    def test_an_unreadable_bundle_fails_closed_without_running_the_runtime(self) -> None:
        bundle = self.root / "broken"
        bundle.mkdir()
        (bundle / "config.json").write_text("{not json", encoding="utf-8")
        result = self.run_wrapper(bundle)
        self.assertEqual(result.returncode, 127)
        self.assertNotIn("runsc ", result.stdout)
        self.assertIn("cannot translate CRI annotations", result.stderr)

    def test_a_missing_runtime_fails_closed(self) -> None:
        self.runsc.unlink()
        bundle = self.bundle("missing-runtime", dict(CRI_O_PAUSE_ANNOTATIONS))
        result = self.run_wrapper(bundle)
        self.assertEqual(result.returncode, 127)
        self.assertIn("is missing or not executable", result.stderr)


if __name__ == "__main__":
    unittest.main()
