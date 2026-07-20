from __future__ import annotations

import importlib.util
import json
import uuid
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/prepare_sprint2_access_seed.py"
ADOPT_TEMPLATE = (
    ROOT
    / "deploy"
    / "ansible"
    / "roles"
    / "sprint2_application"
    / "templates"
    / "access-seed-adopt.sql.j2"
)
SPEC = importlib.util.spec_from_file_location("prepare_sprint2_access_seed", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def test_seed_binds_keycloak_subjects_without_persisting_raw_subjects() -> None:
    realm = {
        "users": [
            {"id": "teacher-subject", "username": "teacher", "realmRoles": ["teacher"]},
            {"id": "student-subject", "username": "student", "realmRoles": ["student"]},
        ]
    }
    seed = MODULE.build_seed(
        realm,
        "https://keycloak.example.test/realms/workloads",
        "00000000-0000-7000-8000-000000000301",
        "teacher",
        "student",
    )
    encoded = json.dumps(seed, sort_keys=True)
    assert "teacher-subject" not in encoded
    assert "student-subject" not in encoded
    memberships = seed["courseMemberships"]
    assert [item["role"] for item in memberships] == ["teacher", "student"]
    assert len({item["actorId"] for item in memberships}) == 2
    assert all(uuid.UUID(item["actorId"]).version == 7 for item in memberships)


def test_seed_rejects_missing_role_and_public_output(tmp_path: Path) -> None:
    realm = {"users": [{"id": "teacher-subject", "username": "teacher", "realmRoles": []}]}
    try:
        MODULE.find_user(realm, "teacher", "teacher")
    except MODULE.AccessSeedError as error:
        assert str(error) == "LW_SPRINT2_ACCESS_SEED_USER_INVALID"
    else:
        raise AssertionError("missing role must be rejected")
    try:
        MODULE.private_output(tmp_path / "seed.json")
    except MODULE.AccessSeedError as error:
        assert str(error) == "LW_SPRINT2_ACCESS_SEED_PRIVATE_PATH_REQUIRED"
    else:
        raise AssertionError("public output path must be rejected")


def test_seed_rejects_non_v7_course_identifier() -> None:
    realm = {
        "users": [
            {"id": "teacher-subject", "username": "teacher", "realmRoles": ["teacher"]},
            {"id": "student-subject", "username": "student", "realmRoles": ["student"]},
        ]
    }
    try:
        MODULE.build_seed(
            realm,
            "https://keycloak.example.test/realms/workloads",
            "00000000-0000-4000-8000-000000000301",
            "teacher",
            "student",
        )
    except MODULE.AccessSeedError as error:
        assert str(error) == "LW_SPRINT2_ACCESS_SEED_COURSE_INVALID"
    else:
        raise AssertionError("non-v7 course identity must be rejected")


def test_adoption_only_reconciles_unowned_legacy_v5_actor_ids() -> None:
    template = ADOPT_TEMPLATE.read_text(encoding="utf-8")
    assert "substring(current_actor_id::text from 15 for 1) <> '5'" in template
    assert "SPRINT2_LEGACY_ACTOR_HAS_OWNED_DATA" in template
    assert "UPDATE access.course_memberships" in template
    assert "UPDATE access.bff_sessions" in template
    assert "DELETE FROM access.actors" not in template
