from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/validate_resource_acceptance_profile.py"
SPEC = importlib.util.spec_from_file_location("resource_acceptance_profile", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("resource profile module could not be loaded")
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class ResourceAcceptanceProfileTests(unittest.TestCase):
    def _profile(self) -> dict[str, object]:
        return json.loads(
            (ROOT / "deploy/config/resource-acceptance-profile.example.json").read_text(
                encoding="utf-8"
            )
        )

    def test_profile_requires_exact_access_seed_memberships(self) -> None:
        profile = self._profile()
        self.assertRegex(MODULE.validate(profile, profile), r"^[0-9a-f]{64}$")
        profile["courseMemberships"][0]["courseId"] = "019fbc00-0000-7000-8000-000000000302"
        with self.assertRaisesRegex(MODULE.ProfileError, "COURSE_MISMATCH"):
            MODULE.validate(profile, self._profile())

    def test_profile_private_path_is_required(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "profile.json"
            path.write_text("{}", encoding="utf-8")
            with self.assertRaisesRegex(MODULE.ProfileError, "PRIVATE_PATH_REQUIRED"):
                MODULE.load(path)

    def test_bootstrap_rejects_extra_course_memberships(self) -> None:
        template = (
            ROOT
            / "deploy/ansible/roles/resource_application/templates/access-seed-adopt.sql.j2"
        ).read_text(encoding="utf-8")
        tasks = (
            ROOT / "deploy/ansible/roles/resource_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        self.assertIn("LW_RESOURCE_ACCEPTANCE_PROFILE_ACCESS_MEMBERSHIP_CONFLICT", template)
        self.assertIn("resource_application_postgres_service_file", tasks)

    def test_public_replay_accepts_locators_not_sql_or_secret_values(self) -> None:
        replay = (ROOT / "tools/resource_replay.py").read_text(encoding="utf-8")
        validator = (ROOT / "tools/validate_resource_replay_inputs.py").read_text(
            encoding="utf-8"
        )
        self.assertIn("/work-agent-runs", replay)
        self.assertIn("/api/v1/resource-requests", replay)
        self.assertIn("/api/v1/resource-leases", replay)
        self.assertIn("resource-lease-replay-report.v1", replay)
        self.assertNotIn("psql", replay)
        self.assertNotIn("Authorization: Bearer", replay)
        self.assertIn("LW_RESOURCE_REPLAY_AUTHENTICATION_INVALID", validator)
        self.assertIn(".private", validator)
        self.assertIn("def regular_file", validator)


if __name__ == "__main__":
    unittest.main()
