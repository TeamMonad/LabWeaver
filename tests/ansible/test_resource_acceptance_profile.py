from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
TOOLS = str(ROOT / "tools")
if TOOLS not in sys.path:
    sys.path.insert(0, TOOLS)
SCRIPT = ROOT / "tools/validate_resource_acceptance_profile.py"
SPEC = importlib.util.spec_from_file_location("resource_acceptance_profile", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("resource profile module could not be loaded")
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)

RENDER_SCRIPT = ROOT / "tools/render_resource_acceptance_profile.py"
RENDER_SPEC = importlib.util.spec_from_file_location("render_resource_acceptance_profile", RENDER_SCRIPT)
if RENDER_SPEC is None or RENDER_SPEC.loader is None:
    raise RuntimeError("resource profile renderer could not be loaded")
RENDER_MODULE = importlib.util.module_from_spec(RENDER_SPEC)
sys.modules[RENDER_SPEC.name] = RENDER_MODULE
RENDER_SPEC.loader.exec_module(RENDER_MODULE)

PREPARE_SCRIPT = ROOT / "tools/prepare_resource_acceptance_profile.py"
PREPARE_SPEC = importlib.util.spec_from_file_location("prepare_resource_acceptance_profile", PREPARE_SCRIPT)
if PREPARE_SPEC is None or PREPARE_SPEC.loader is None:
    raise RuntimeError("resource profile preparer could not be loaded")
PREPARE_MODULE = importlib.util.module_from_spec(PREPARE_SPEC)
sys.modules[PREPARE_SPEC.name] = PREPARE_MODULE
PREPARE_SPEC.loader.exec_module(PREPARE_MODULE)

REPORT_SCRIPT = ROOT / "tools/validate_resource_replay_report.py"
REPORT_SPEC = importlib.util.spec_from_file_location("resource_replay_report", REPORT_SCRIPT)
if REPORT_SPEC is None or REPORT_SPEC.loader is None:
    raise RuntimeError("resource replay report validator could not be loaded")
REPORT_MODULE = importlib.util.module_from_spec(REPORT_SPEC)
sys.modules[REPORT_SPEC.name] = REPORT_MODULE
REPORT_SPEC.loader.exec_module(REPORT_MODULE)


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

    def test_renderer_binds_seed_and_material_without_retaining_source_path(self) -> None:
        profile = self._profile()
        with tempfile.TemporaryDirectory() as temporary:
            private = Path(temporary) / ".private"
            private.mkdir()
            seed_path = private / "access-seed.json"
            configuration_path = private / "profile-configuration.json"
            material_path = private / "assignment.md"
            seed_path.write_text(json.dumps({"courseMemberships": profile["courseMemberships"]}), encoding="utf-8")
            configuration_path.write_text(
                json.dumps(
                    {
                        "runtimeKind": profile["runtimeKind"],
                        "resources": profile["resources"],
                        "durationSeconds": profile["durationSeconds"],
                        "configurationSha256": profile["configurationSha256"],
                        "projectId": profile["replay"]["projectId"],
                        "providerBinding": profile["replay"]["providerBinding"],
                        "policy": profile["replay"]["policy"],
                        "material": {"description": profile["material"]["description"], "mediaType": profile["replay"]["materialFile"]["mediaType"]},
                    }
                ),
                encoding="utf-8",
            )
            material_path.write_text("verified acceptance material\n", encoding="utf-8")
            rendered = RENDER_MODULE.render(
                RENDER_MODULE.document(seed_path, "invalid"),
                RENDER_MODULE.document(configuration_path, "invalid"),
                material_path,
            )
            self.assertEqual(rendered["courseId"], profile["courseId"])
            self.assertEqual(rendered["replay"]["materialFile"]["relativePath"], "assignment.md")
            self.assertNotIn(str(material_path), json.dumps(rendered))
            self.assertRegex(MODULE.validate(rendered, rendered), r"^[0-9a-f]{64}$")

    def test_renderer_rejects_non_private_inputs(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "seed.json"
            path.write_text("{}", encoding="utf-8")
            with self.assertRaisesRegex(RENDER_MODULE.RenderError, "ACCESS_SEED_INVALID"):
                RENDER_MODULE.private(path, "LW_RESOURCE_PROFILE_RENDER_ACCESS_SEED_INVALID")

    def test_preparer_generates_stable_private_source_without_secrets(self) -> None:
        profile = self._profile()
        with tempfile.TemporaryDirectory() as temporary:
            private = Path(temporary) / ".private"
            private.mkdir()
            seed_path = private / "access-seed.json"
            runtime_path = private / "agent-runtime.yaml"
            configuration_path = private / "configuration.json"
            material_path = private / "material.md"
            seed_path.write_text(json.dumps({"courseMemberships": profile["courseMemberships"]}), encoding="utf-8")
            runtime_path.write_text("runtime: reviewed\n", encoding="utf-8")
            configuration = PREPARE_MODULE.build_configuration(
                json.loads(seed_path.read_text(encoding="utf-8")), runtime_path,
                "e" * 64, "2.1.215", "ecnu-plus", "claude-code-production", "kubernetes-standard",
            )
            PREPARE_MODULE.atomic_json(configuration_path, configuration)
            PREPARE_MODULE.atomic_material(material_path)
            rendered = RENDER_MODULE.render(
                json.loads(seed_path.read_text(encoding="utf-8")), configuration, material_path
            )
            self.assertRegex(MODULE.validate(rendered, json.loads(seed_path.read_text(encoding="utf-8"))), r"^[0-9a-f]{64}$")
            if os.name != "nt":
                self.assertEqual(material_path.stat().st_mode & 0o777, 0o600)
            self.assertNotIn("token", material_path.read_text(encoding="utf-8").lower())

    def test_report_requires_tombstone_identity_and_counts(self) -> None:
        run_id = "019fbc00-0000-7000-8000-000000000501"
        commit = "a" * 40
        report = {
            "schemaVersion": "resource-lease-replay-report.v1",
            "runId": run_id,
            "sourceCommit": commit,
            "checks": ["work-agent-run", "dual-approval", "work-release", "resource-request", "lease-renew", "lease-revoke", "environment-tombstone"],
            "identity": {"runId": run_id, "sourceCommit": commit},
            "counts": {"uploadedFiles": 1, "agentTracks": 2, "resourceRequests": 1, "leases": 1, "environmentTombstones": 1},
            "cleanup": {"observedState": "deleted"},
        }
        REPORT_MODULE.validate(report, commit, run_id)
        report["cleanup"] = {"observedState": "ready"}
        with self.assertRaisesRegex(REPORT_MODULE.ReportError, "CLEANUP_INVALID"):
            REPORT_MODULE.validate(report, commit, run_id)

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
        self.assertIn("resource_application_profile_renderer", tasks)
        self.assertNotIn("become: false", tasks)

    def test_bootstrap_owns_a_bounded_postgres_tunnel(self) -> None:
        module = (
            ROOT / "deploy/ansible/library/labweaver_postgres_apply.py"
        ).read_text(encoding="utf-8")
        tasks = (
            ROOT / "deploy/ansible/roles/resource_application/tasks/main.yml"
        ).read_text(encoding="utf-8")
        defaults = (
            ROOT / "deploy/ansible/roles/resource_application/defaults/main.yml"
        ).read_text(encoding="utf-8")
        ansible_config = (ROOT / "deploy/ansible/ansible.cfg").read_text(encoding="utf-8")
        self.assertIn("port-forward", module)
        self.assertIn("service/postgres", module)
        self.assertIn("tunnel.terminate()", module)
        self.assertIn("RESOURCE_APPLICATION_POSTGRES_TUNNEL_CONFLICT", module)
        self.assertIn("labweaver_postgres_apply:", tasks)
        resource_bundle_module = (
            ROOT / "deploy/ansible/library/labweaver_resource_bundle_apply.py"
        ).read_text(encoding="utf-8")
        self.assertIn("RESOURCE_APPLICATION_SECRET_OWNERSHIP_CONFLICT", resource_bundle_module)
        self.assertIn("resource-service-secrets", resource_bundle_module)
        self.assertNotIn("force-conflicts", resource_bundle_module)
        self.assertIn("labweaver_resource_bundle_apply:", tasks)
        self.assertIn("/var/lib/labweaver/.private/resource-acceptance", defaults)
        self.assertIn("library = library", ansible_config)

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
        self.assertIn("diagnosticCode", replay)
        self.assertIn("LW_RESOURCE_REPLAY_PUBLIC_API_REJECTED_", replay)
        self.assertIn("LW_RESOURCE_REPLAY_AUTHENTICATION_INVALID", validator)
        self.assertIn(".private", validator)
        self.assertIn("def regular_file", validator)
        report_validator = (ROOT / "tools/validate_resource_replay_report.py").read_text(encoding="utf-8")
        self.assertIn("environment-tombstone", report_validator)
        self.assertIn("LW_RESOURCE_REPLAY_REPORT_CLEANUP_INVALID", report_validator)
        replay_tasks = (ROOT / "deploy/ansible/roles/resource_replay/tasks/main.yml").read_text(encoding="utf-8")
        self.assertNotIn("become: false", replay_tasks)
        self.assertIn("Surface a stable Resource replay diagnostic", replay_tasks)
        self.assertIn("LW_RESOURCE_REPLAY_FAILED", replay_tasks)

    def test_browser_authentication_is_a_private_ansible_boundary(self) -> None:
        defaults = (
            ROOT / "deploy/ansible/roles/resource_replay_auth/defaults/main.yml"
        ).read_text(encoding="utf-8")
        tasks = (
            ROOT / "deploy/ansible/roles/resource_replay_auth/tasks/main.yml"
        ).read_text(encoding="utf-8")
        self.assertIn(".private/resource-replay-auth", defaults)
        self.assertIn("resource_replay_auth_driver", tasks)
        self.assertIn("--trusted-ca", tasks)
        self.assertIn("stat.mode == '0600'", tasks)
        self.assertIn("no_log: true", tasks)
        driver = (ROOT / "tools/create_resource_replay_auth.py").read_text(encoding="utf-8")
        self.assertIn("resource-replay-auth/v1", driver)
        self.assertIn("create_resource_replay_auth.py", defaults)
        self.assertIn("os.chmod", driver)
        playbook = (ROOT / "deploy/ansible/playbooks/94-resource-replay-auth.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("connection: local", playbook)
        self.assertIn("resource-replay-auth", playbook)
        xtask = (ROOT / "xtask/src/main.rs").read_text(encoding="utf-8")
        self.assertIn("ResourceCommand::Auth", xtask)
        self.assertIn('"94-resource-replay-auth.yml"', xtask)


if __name__ == "__main__":
    unittest.main()
