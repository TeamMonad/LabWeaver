"""Black-box tests for the shared Work configuration runner.

Run this file directly when Docker is available:

    python services/environment-service/tests/work_configuration_runner.py

The test driver passes scripts to a disposable Debian container as files. It
does not build shell command lines from user script content, which keeps shell
metacharacters and dollar signs inside the container boundary.
"""

from __future__ import annotations

import shutil
import subprocess
import tempfile
import time
import unittest
import uuid
from collections.abc import Callable
from pathlib import Path


IMAGE = "debian:bookworm-slim"
RUNNER_IN_CONTAINER = "/runner.sh"
CASES_IN_CONTAINER = "/cases"
WORK_IN_CONTAINER = "/work"
MAX_OUTPUT_BYTES = 65_536


class WorkConfigurationRunnerTests(unittest.TestCase):
    """Exercise the runner through the same process boundary as a runtime Pod."""

    @classmethod
    def setUpClass(cls) -> None:
        if shutil.which("docker") is None:
            raise unittest.SkipTest("docker is not installed")
        image = subprocess.run(
            ["docker", "image", "inspect", IMAGE],
            capture_output=True,
            check=False,
        )
        if image.returncode != 0:
            raise AssertionError(
                f"Docker image {IMAGE} is unavailable: "
                + image.stderr.decode("utf-8", "replace").strip()
            )

        cls.container = f"labweaver-runner-test-{uuid.uuid4().hex[:12]}"
        started = subprocess.run(
            [
                "docker",
                "run",
                "--detach",
                "--name",
                cls.container,
                IMAGE,
                "tail",
                "-f",
                "/dev/null",
            ],
            capture_output=True,
            check=False,
        )
        if started.returncode != 0:
            raise AssertionError(
                "Docker could not start the runner test container: "
                + started.stderr.decode("utf-8", "replace").strip()
            )

        cls.host_tmp = tempfile.TemporaryDirectory(prefix="labweaver-runner-")
        cls.host_root = Path(cls.host_tmp.name)
        try:
            cls._exec("mkdir", "-p", CASES_IN_CONTAINER, WORK_IN_CONTAINER)
            runner = Path(__file__).parents[1] / ".." / "work-configuration-runner.sh"
            cls._copy(runner, RUNNER_IN_CONTAINER)
        except Exception:
            subprocess.run(
                ["docker", "rm", "--force", cls.container],
                capture_output=True,
                check=False,
            )
            cls.host_tmp.cleanup()
            raise

    @classmethod
    def tearDownClass(cls) -> None:
        subprocess.run(
            ["docker", "rm", "--force", cls.container],
            capture_output=True,
            check=False,
        )
        cls.host_tmp.cleanup()

    @classmethod
    def _exec(cls, *args: str, check: bool = True) -> subprocess.CompletedProcess[bytes]:
        result = subprocess.run(
            ["docker", "exec", cls.container, *args],
            capture_output=True,
            check=False,
        )
        if check and result.returncode != 0:
            raise AssertionError(
                "docker exec failed: "
                + " ".join(args)
                + "\n"
                + result.stderr.decode("utf-8", "replace")
            )
        return result

    @classmethod
    def _exec_detached(cls, *args: str) -> subprocess.CompletedProcess[bytes]:
        return subprocess.run(
            ["docker", "exec", "--detach", cls.container, *args],
            capture_output=True,
            check=False,
        )

    @classmethod
    def _copy(cls, source: Path, destination: str) -> None:
        result = subprocess.run(
            ["docker", "cp", str(source), f"{cls.container}:{destination}"],
            capture_output=True,
            check=False,
        )
        if result.returncode != 0:
            raise AssertionError(
                "docker cp failed: " + result.stderr.decode("utf-8", "replace")
            )

    @classmethod
    def _new_case(
        cls,
        primary: str | Callable[[str], str],
        verification: str | Callable[[str], str] | None = None,
    ) -> str:
        name = f"case-{uuid.uuid4().hex[:12]}"
        if callable(primary):
            primary = primary(name)
        if callable(verification):
            verification = verification(name)
        host_case = cls.host_root / name
        host_case.mkdir()
        (host_case / "primary.sh").write_text(primary, encoding="utf-8", newline="\n")
        cls._exec("mkdir", "-p", f"{CASES_IN_CONTAINER}/{name}")
        cls._copy(host_case / "primary.sh", f"{CASES_IN_CONTAINER}/{name}/primary.sh")
        if verification is not None:
            (host_case / "verification.sh").write_text(
                verification,
                encoding="utf-8",
                newline="\n",
            )
            cls._copy(
                host_case / "verification.sh",
                f"{CASES_IN_CONTAINER}/{name}/verification.sh",
            )
        return name

    @classmethod
    def _case_path(cls, name: str) -> str:
        return f"{CASES_IN_CONTAINER}/{name}"

    @classmethod
    def _run(cls, name: str, verification_required: bool, deadline: int) -> subprocess.CompletedProcess[bytes]:
        return cls._exec(
            "/bin/sh",
            RUNNER_IN_CONTAINER,
            cls._case_path(name),
            WORK_IN_CONTAINER,
            "1" if verification_required else "0",
            str(deadline),
            check=False,
        )

    @classmethod
    def _observe(cls, name: str, verification_required: bool) -> tuple[int, str, bool, bytes]:
        result = cls._exec(
            "/bin/sh",
            RUNNER_IN_CONTAINER,
            "--observe",
            cls._case_path(name),
            "1" if verification_required else "0",
            check=False,
        )
        if result.returncode != 0:
            raise AssertionError(
                "observe failed with exit "
                + str(result.returncode)
                + ": "
                + result.stderr.decode("utf-8", "replace")
            )
        fields = result.stdout.split(b"\n", 3)
        if len(fields) != 4:
            raise AssertionError(f"malformed observation: {result.stdout!r}")
        primary = int(fields[0])
        verification = fields[1].decode("ascii")
        truncated = fields[2] == b"1"
        return primary, verification, truncated, fields[3]

    @classmethod
    def _cancel(cls, name: str) -> subprocess.CompletedProcess[bytes]:
        return cls._exec(
            "/bin/sh",
            RUNNER_IN_CONTAINER,
            "--cancel",
            cls._case_path(name),
            check=False,
        )

    @classmethod
    def _exists(cls, path: str) -> bool:
        return cls._exec("test", "-f", path, check=False).returncode == 0

    @classmethod
    def _wait_for_file(cls, path: str, timeout: float = 5.0) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if cls._exists(path):
                return
            time.sleep(0.05)
        raise AssertionError(f"timed out waiting for {path}")

    @classmethod
    def _wait_for_text(cls, path: str, expected: bytes, timeout: float = 5.0) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            result = cls._exec("cat", path, check=False)
            if result.returncode == 0 and result.stdout == expected:
                return
            time.sleep(0.05)
        raise AssertionError(f"timed out waiting for {path} to contain {expected!r}")

    def test_primary_and_verification_succeed(self) -> None:
        name = self._new_case(
            "#!/bin/sh\nprintf 'primary:'\nprintf '%s\\n' '$USER_INPUT'\n",
            "#!/bin/sh\nprintf '%s\\n' 'verified'\n",
        )
        run = self._run(name, True, int(time.time()) + 30)
        self.assertEqual(run.returncode, 0, run.stderr.decode("utf-8", "replace"))
        self.assertEqual(self._observe(name, True), (0, "0", False, b"primary:$USER_INPUT\nverified\n"))

    def test_nonzero_primary_skips_verification(self) -> None:
        name = self._new_case(
            "#!/bin/sh\nprintf '%s\\n' 'primary-failed'\nexit 7\n",
            lambda case: f"#!/bin/sh\nprintf ran > {WORK_IN_CONTAINER}/{case}-verification-ran\n",
        )
        run = self._run(name, True, int(time.time()) + 30)
        self.assertEqual(run.returncode, 0, run.stderr.decode("utf-8", "replace"))
        self.assertEqual(self._observe(name, True), (7, "-", False, b"primary-failed\n"))
        self.assertFalse(self._exists(f"{WORK_IN_CONTAINER}/{name}-verification-ran"))

    def test_verification_failure_is_reported(self) -> None:
        name = self._new_case(
            "#!/bin/sh\nprintf '%s\\n' 'primary-ok'\n",
            "#!/bin/sh\nprintf '%s\\n' 'verification-failed'\nexit 9\n",
        )
        run = self._run(name, True, int(time.time()) + 30)
        self.assertEqual(run.returncode, 0, run.stderr.decode("utf-8", "replace"))
        self.assertEqual(
            self._observe(name, True),
            (0, "9", False, b"primary-ok\nverification-failed\n"),
        )

    def test_utf8_and_binary_output_are_capped(self) -> None:
        name = self._new_case(
            "#!/bin/sh\nprintf '\\342\\230\\203'\n"
            "head -c 70000 /dev/zero | tr '\\000' '\\377'\n"
        )
        run = self._run(name, False, int(time.time()) + 30)
        self.assertEqual(run.returncode, 0, run.stderr.decode("utf-8", "replace"))
        primary, verification, truncated, output = self._observe(name, False)
        self.assertEqual(primary, 0)
        self.assertEqual(verification, "-")
        self.assertTrue(truncated)
        self.assertEqual(len(output), MAX_OUTPUT_BYTES)
        self.assertTrue(output.startswith("☃".encode("utf-8")))
        self.assertEqual(output[3:], b"\xff" * (MAX_OUTPUT_BYTES - 3))

    def test_past_deadline_does_not_start_script(self) -> None:
        name = self._new_case(
            lambda case: f"#!/bin/sh\nprintf ran > {WORK_IN_CONTAINER}/{case}-deadline-ran\n"
        )
        run = self._run(name, False, int(time.time()) - 1)
        self.assertEqual(run.returncode, 0, run.stderr.decode("utf-8", "replace"))
        self.assertEqual(self._observe(name, False), (124, "-", False, b""))
        self.assertFalse(self._exists(f"{WORK_IN_CONTAINER}/{name}-deadline-ran"))
        self.assertTrue(self._exists(f"{self._case_path(name)}/timeout"))

    def test_deadline_is_shared_by_primary_and_verification(self) -> None:
        name = self._new_case(
            "#!/bin/sh\nsleep 1\nprintf '%s\\n' 'primary-ok'\n",
            lambda case: f"#!/bin/sh\nsleep 30\nprintf ran > {WORK_IN_CONTAINER}/{case}-verification-after-deadline\n",
        )
        deadline = int(time.time()) + 3
        started_at = time.monotonic()
        run = self._run(name, True, deadline)
        elapsed = time.monotonic() - started_at

        self.assertEqual(run.returncode, 0, run.stderr.decode("utf-8", "replace"))
        primary, verification, truncated, output = self._observe(name, True)
        self.assertEqual((primary, verification, truncated), (0, "124", False))
        self.assertEqual(output, b"primary-ok\n")
        self.assertFalse(
            self._exists(f"{WORK_IN_CONTAINER}/{name}-verification-after-deadline")
        )
        self.assertLess(elapsed, 7.0)

    def test_cancel_before_start_prevents_launch(self) -> None:
        name = self._new_case(
            lambda case: f"#!/bin/sh\nprintf ran > {WORK_IN_CONTAINER}/{case}-prestart-ran\n",
            "#!/bin/sh\nprintf verification-ran\n",
        )
        self.assertEqual(self._cancel(name).returncode, 0)
        run = self._run(name, True, int(time.time()) + 30)
        self.assertEqual(run.returncode, 0, run.stderr.decode("utf-8", "replace"))
        self.assertEqual(self._observe(name, True), (143, "-", False, b""))
        self.assertFalse(self._exists(f"{WORK_IN_CONTAINER}/{name}-prestart-ran"))

    def test_running_cancel_stops_process_and_is_idempotent(self) -> None:
        name = self._new_case(
            lambda case: f"#!/bin/sh\nprintf started > {WORK_IN_CONTAINER}/{case}-started\n"
            "trap 'exit 143' TERM INT\n"
            "sleep 30\n"
            f"printf completed > {WORK_IN_CONTAINER}/{case}-completed\n"
        )
        deadline = int(time.time()) + 30
        started = self._exec_detached(
            "/bin/sh",
            RUNNER_IN_CONTAINER,
            self._case_path(name),
            WORK_IN_CONTAINER,
            "0",
            str(deadline),
        )
        self.assertEqual(started.returncode, 0)
        self._wait_for_file(f"{WORK_IN_CONTAINER}/{name}-started")
        self.assertEqual(self._cancel(name).returncode, 0)
        self._wait_for_file(f"{self._case_path(name)}/primary_exit")
        self._wait_for_text(f"{self._case_path(name)}/phase", b"done\n")
        primary, verification, truncated, output = self._observe(name, False)
        self.assertEqual(primary, 143)
        self.assertEqual(verification, "-")
        self.assertFalse(truncated)
        self.assertIn(output, (b"", b"Terminated\n"))
        self.assertFalse(self._exists(f"{WORK_IN_CONTAINER}/{name}-completed"))
        self.assertFalse(self._exists(f"{self._case_path(name)}/pid"))
        self.assertEqual(self._cancel(name).returncode, 0)
        self.assertEqual(self._cancel(name).returncode, 0)

    def test_repeated_runner_call_keeps_first_receipt(self) -> None:
        name = self._new_case("#!/bin/sh\nprintf once\n")
        deadline = int(time.time()) + 30
        first = self._run(name, False, deadline)
        self.assertEqual(first.returncode, 0, first.stderr.decode("utf-8", "replace"))
        before = self._observe(name, False)
        second = self._run(name, False, deadline)
        self.assertEqual(second.returncode, 125)
        self.assertEqual(self._observe(name, False), before)


if __name__ == "__main__":
    unittest.main()
