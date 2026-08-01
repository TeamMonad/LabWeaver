#!/usr/bin/env python3
"""Execute the Resource acceptance path through the public authenticated API.

The private profile describes only reviewed non-secret configuration. Authentication
comes from separate Playwright storage-state locators. This program never accepts SQL,
service mTLS credentials, bearer tokens, or arbitrary endpoints, and its report records
only identity hashes, counts, stable phase names and diagnostics.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path
from typing import Any

from validate_resource_replay_inputs import ReplayInputError, validate


class ReplayError(Exception):
    """Stable, secret-free blocker."""


def load_json(path: Path, code: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ReplayError(code) from error
    if not isinstance(value, dict):
        raise ReplayError(code)
    return value


def private_child(profile: Path, relative: str) -> Path:
    candidate = (profile.parent / relative).resolve()
    if profile.parent.resolve() not in candidate.parents or not candidate.is_file():
        raise ReplayError("LW_RESOURCE_REPLAY_MATERIAL_LOCATOR_INVALID")
    return candidate


class BffClient:
    def __init__(self, base_url: str, state: Path, run_id: str, role: str) -> None:
        document = load_json(state, "LW_RESOURCE_REPLAY_AUTHENTICATION_INVALID")
        cookies = document.get("cookies")
        if not isinstance(cookies, list):
            raise ReplayError("LW_RESOURCE_REPLAY_AUTHENTICATION_INVALID")
        pairs: list[str] = []
        for cookie in cookies:
            if isinstance(cookie, dict) and isinstance(cookie.get("name"), str) and isinstance(cookie.get("value"), str):
                pairs.append(f"{cookie['name']}={cookie['value']}")
        if not pairs:
            raise ReplayError("LW_RESOURCE_REPLAY_AUTHENTICATION_INVALID")
        self.base_url = base_url.rstrip("/")
        self.cookie = "; ".join(pairs)
        self.run_id = run_id
        self.role = role
        self.csrf = self._csrf()

    def _csrf(self) -> str:
        value, _ = self.request("GET", "/api/v1/auth/csrf", None, "csrf", csrf=False)
        token = value.get("token", value.get("csrfToken")) if isinstance(value, dict) else None
        if not isinstance(token, str) or not token:
            raise ReplayError("LW_RESOURCE_REPLAY_CSRF_INVALID")
        return token

    def request(
        self,
        method: str,
        path: str,
        body: Any,
        step: str,
        *,
        csrf: bool = True,
        if_match: str | None = None,
    ) -> tuple[Any, dict[str, str]]:
        if not path.startswith("/") or ".." in path.split("/"):
            raise ReplayError("LW_RESOURCE_REPLAY_PATH_INVALID")
        headers = {"Cookie": self.cookie, "Accept": "application/json"}
        data = None
        if body is not None:
            headers["Content-Type"] = "application/json"
            data = json.dumps(body, separators=(",", ":")).encode("utf-8")
        if csrf:
            headers["X-CSRF-Token"] = self.csrf
            headers["Idempotency-Key"] = f"resource-replay-{self.run_id}-{step}"[:128]
        if if_match is not None:
            if not if_match.startswith('"') or not if_match.endswith('"'):
                raise ReplayError("LW_RESOURCE_REPLAY_ETAG_INVALID")
            headers["If-Match"] = if_match
        request = urllib.request.Request(self.base_url + path, data=data, headers=headers, method=method)
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                raw = response.read()
                return (json.loads(raw) if raw else {}, dict(response.headers.items()))
        except urllib.error.HTTPError as error:
            raw = error.read(4096).decode("utf-8", errors="replace")
            diagnostic = raw.strip() if raw.startswith("LW_") else ""
            if not diagnostic:
                try:
                    payload = json.loads(raw)
                except json.JSONDecodeError:
                    payload = None
                if isinstance(payload, dict):
                    candidate = payload.get("diagnosticCode")
                    if isinstance(candidate, str) and candidate.startswith("LW_"):
                        diagnostic = candidate
            if not diagnostic:
                # The step labels are fixed in this program and therefore do
                # not expose a server response, request payload, or actor data.
                diagnostic = "LW_RESOURCE_REPLAY_PUBLIC_API_REJECTED_" + step.upper().replace("-", "_")
            raise ReplayError(diagnostic) from error
        except (OSError, json.JSONDecodeError) as error:
            raise ReplayError("LW_RESOURCE_REPLAY_PUBLIC_API_UNAVAILABLE") from error

    def poll(self, path: str, states: set[str], step: str) -> dict[str, Any]:
        deadline = time.monotonic() + 600
        while time.monotonic() < deadline:
            value, _ = self.request("GET", path, None, step, csrf=False)
            if not isinstance(value, dict):
                raise ReplayError("LW_RESOURCE_REPLAY_PUBLIC_DOCUMENT_INVALID")
            state = value.get("state")
            if state in states:
                return value
            if state in {"failed", "cancelled", "rejected", "expired", "revoked"}:
                raise ReplayError(str(value.get("diagnosticCode") or "LW_RESOURCE_REPLAY_TERMINAL_FAILURE"))
            time.sleep(1)
        raise ReplayError("LW_RESOURCE_REPLAY_TIMEOUT")

    def poll_deleted_environment(self, environment_id: str) -> dict[str, Any]:
        deadline = time.monotonic() + 600
        path = f"/api/v1/environments/{environment_id}"
        while time.monotonic() < deadline:
            value, _ = self.request("GET", path, None, "environment-tombstone", csrf=False)
            if not isinstance(value, dict):
                raise ReplayError("LW_RESOURCE_REPLAY_PUBLIC_DOCUMENT_INVALID")
            if value.get("desiredState") == "deleted" and value.get("observedState") == "deleted":
                if not isinstance(value.get("cleanupEvidence"), dict):
                    raise ReplayError("LW_RESOURCE_REPLAY_ENVIRONMENT_TOMBSTONE_INVALID")
                return value
            if value.get("observedState") == "failed":
                raise ReplayError(str(value.get("lastDiagnosticCode") or "LW_RESOURCE_REPLAY_ENVIRONMENT_CLEANUP_FAILED"))
            time.sleep(1)
        raise ReplayError("LW_RESOURCE_REPLAY_TIMEOUT")


def require(value: Any, key: str, code: str) -> Any:
    if not isinstance(value, dict) or key not in value:
        raise ReplayError(code)
    return value[key]


def etag(headers: dict[str, str]) -> str:
    value = headers.get("Etag", headers.get("ETag"))
    if not value:
        raise ReplayError("LW_RESOURCE_REPLAY_ETAG_MISSING")
    return value


def run(arguments: argparse.Namespace) -> dict[str, Any]:
    identity = validate(arguments)
    profile = load_json(arguments.profile, "LW_RESOURCE_REPLAY_PROFILE_INVALID")
    auth = load_json(arguments.authentication, "LW_RESOURCE_REPLAY_AUTHENTICATION_INVALID")
    replay = require(profile, "replay", "LW_RESOURCE_REPLAY_PROFILE_INVALID")
    material = require(replay, "materialFile", "LW_RESOURCE_REPLAY_PROFILE_INVALID")
    material_path = private_child(arguments.profile, str(require(material, "relativePath", "LW_RESOURCE_REPLAY_PROFILE_INVALID")))
    material_bytes = material_path.read_bytes()
    if hashlib.sha256(material_bytes).hexdigest() != require(material, "sha256", "LW_RESOURCE_REPLAY_PROFILE_INVALID"):
        raise ReplayError("LW_RESOURCE_REPLAY_MATERIAL_HASH_MISMATCH")
    teacher = BffClient(auth["baseUrl"], Path(auth["teacherStorageState"]), arguments.run_id, "teacher")
    admin = BffClient(auth["baseUrl"], Path(auth["platformAdminStorageState"]), arguments.run_id, "platform-admin")
    course_id = profile["courseId"]
    policy, _ = teacher.request("POST", f"/api/v1/courses/{course_id}/llm-egress-policies", replay["policy"], "policy")
    upload, _ = teacher.request("POST", f"/api/v1/courses/{course_id}/problem-package-uploads", {
        "files": [{"path": material["relativePath"], "sizeBytes": len(material_bytes), "sha256": material["sha256"], "mediaType": material["mediaType"]}],
        "retentionPolicyRevision": 1,
    }, "upload")
    target = require(upload, "uploadTargets", "LW_RESOURCE_REPLAY_UPLOAD_INVALID")
    if not isinstance(target, list) or len(target) != 1 or not isinstance(target[0], dict):
        raise ReplayError("LW_RESOURCE_REPLAY_UPLOAD_INVALID")
    upload_target = target[0]
    put = urllib.request.Request(upload_target["uploadUrl"], data=material_bytes, headers=upload_target.get("requiredHeaders", {}), method="PUT")
    try:
        with urllib.request.urlopen(put, timeout=30):
            pass
    except OSError as error:
        raise ReplayError("LW_RESOURCE_REPLAY_UPLOAD_FAILED") from error
    package, _ = teacher.request("POST", f"/api/v1/courses/{course_id}/problem-package-uploads/{upload['id']}/complete", {"manifestSha256": upload["manifestSha256"]}, "complete-upload")
    run_value, _ = teacher.request("POST", f"/api/v1/courses/{course_id}/work-agent-runs", {
        "packageId": package["id"], "packageRevision": package["revision"], "packageSha256": package["manifestSha256"],
        "policyId": policy["id"], "policyRevision": policy["revision"], "requestedRuntime": profile["runtimeKind"],
    }, "work-agent-run")
    agent = teacher.poll(run_value["statusUrl"], {"succeeded"}, "agent-run")
    tracks = require(agent, "tracks", "LW_RESOURCE_REPLAY_AGENT_INVALID")
    if not isinstance(tracks, list) or len(tracks) != 2:
        raise ReplayError("LW_RESOURCE_REPLAY_AGENT_INVALID")
    candidates = {item.get("kind"): item.get("candidateId") for item in tracks if isinstance(item, dict)}
    environment_id = candidates.get("environment")
    evaluation_id = candidates.get("evaluation")
    if not isinstance(environment_id, str) or not isinstance(evaluation_id, str):
        raise ReplayError("LW_RESOURCE_REPLAY_AGENT_INVALID")
    environment, environment_headers = teacher.request("GET", f"/api/v1/courses/{course_id}/environment-candidates/{environment_id}", None, "environment-candidate", csrf=False)
    evaluation, evaluation_headers = teacher.request("GET", f"/api/v1/courses/{course_id}/evaluation-candidates/{evaluation_id}", None, "evaluation-candidate", csrf=False)
    candidate = require(environment, "candidate", "LW_RESOURCE_REPLAY_AGENT_INVALID")
    if candidate.get("spec", {}).get("class") != "work":
        raise ReplayError("LW_RESOURCE_REPLAY_WORK_CLASS_REJECTED")
    for label, view, headers in (("environment", environment, environment_headers), ("evaluation", evaluation, evaluation_headers)):
        candidate_value = require(view, "candidate", "LW_RESOURCE_REPLAY_AGENT_INVALID")
        teacher.request("POST", f"/api/v1/courses/{course_id}/{label}-candidates/{candidate_value['id']}/decisions", {
            "candidateRevision": candidate_value["revision"], "candidateSha256": candidate_value.get("specSha256"),
            "policyRevision": candidate_value.get("policyRevision", policy["revision"]), "schemaSha256": candidate_value.get("schemaSha256"),
            "trustRevision": view["trustRevision"], "decision": "approved", "reason": "resource-acceptance",
        }, f"approve-{label}")
    environment, environment_headers = teacher.request("GET", f"/api/v1/courses/{course_id}/environment-candidates/{environment_id}", None, "environment-approved", csrf=False)
    candidate = environment["candidate"]
    approval = environment.get("approvals", [])[-1] if environment.get("approvals") else None
    if not isinstance(approval, dict) or approval.get("decision") != "approved":
        raise ReplayError("LW_RESOURCE_REPLAY_APPROVAL_INVALID")
    release_operation, _ = teacher.request("POST", f"/api/v1/courses/{course_id}/environment-template-releases", {
        "approvalId": approval["id"], "candidateId": candidate["id"], "candidateRevision": candidate["revision"],
        "environmentSpecSha256": candidate["specSha256"], "runtimeKind": profile["runtimeKind"],
    }, "publish-work-release")
    release_view, _ = teacher.request("GET", release_operation["statusUrl"], None, "work-release", csrf=False)
    release = require(release_view, "release", "LW_RESOURCE_REPLAY_RELEASE_INVALID")
    environment, _ = teacher.request("POST", "/api/v1/environments", {"courseId": course_id, "releaseId": release["id"], "releaseVersion": release["version"], "displayLabel": "resource-acceptance"}, "create-environment")
    observed = teacher.poll(environment["statusUrl"], {"ready"}, "environment-ready")
    request, _ = teacher.request("POST", "/api/v1/resource-requests", {
        "courseId": course_id, "projectId": replay["projectId"], "requestKey": f"resource-replay-{arguments.run_id}",
        "environmentId": observed["id"], "releaseId": release["id"], "releaseVersion": release["version"],
        "releaseSha256": release["releaseSha256"], "resources": profile["resources"], "durationSeconds": profile["durationSeconds"],
    }, "resource-request")
    resource, resource_headers = admin.request("GET", request["statusUrl"], None, "resource-request", csrf=False)
    approval, _ = admin.request("POST", f"/api/v1/resource-requests/{resource['id']}/approve", {
        "expectedRevision": resource["revision"], "providerBinding": replay["providerBinding"], "resources": profile["resources"],
        "durationSeconds": profile["durationSeconds"], "reason": "resource-acceptance",
    }, "approve-resource")
    lease = admin.poll(f"/api/v1/resource-leases/{approval['leaseId']}", {"active"}, "lease-active")
    renewed, renew_headers = admin.request("POST", f"/api/v1/resource-leases/{lease['id']}/renew", {"expectedRevision": lease["revision"], "durationSeconds": profile["durationSeconds"], "reason": "resource-acceptance-renew"}, "renew-lease")
    revoked, _ = admin.request("POST", f"/api/v1/resource-leases/{lease['id']}/revoke", {"expectedRevision": renewed["revision"], "reason": "resource-acceptance-revoke"}, "revoke-lease")
    admin.poll(f"/api/v1/resource-leases/{lease['id']}", {"revoked", "expired"}, "lease-terminal")
    _, environment_headers = teacher.request(
        "GET", f"/api/v1/environments/{observed['id']}", None, "environment-before-delete", csrf=False
    )
    delete, _ = teacher.request(
        "DELETE",
        f"/api/v1/environments/{observed['id']}",
        None,
        "delete-environment",
        if_match=etag(environment_headers),
    )
    if not isinstance(delete, dict) or not isinstance(delete.get("statusUrl"), str):
        raise ReplayError("LW_RESOURCE_REPLAY_ENVIRONMENT_DELETE_INVALID")
    tombstone = teacher.poll_deleted_environment(observed["id"])
    return {
        "schemaVersion": "resource-lease-replay-report.v1",
        "runId": arguments.run_id,
        "sourceCommit": arguments.source_commit,
        "checks": [
            "work-agent-run", "dual-approval", "work-release", "resource-request",
            "lease-renew", "lease-revoke", "environment-tombstone",
        ],
        "identity": identity,
        "counts": {"uploadedFiles": 1, "agentTracks": 2, "resourceRequests": 1, "leases": 1, "environmentTombstones": 1},
        "cleanup": {"observedState": tombstone["observedState"]},
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    for name in ("profile", "authentication", "deployment-manifest", "package-manifest"):
        parser.add_argument(f"--{name}", type=Path, required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--report", type=Path, required=True)
    arguments = parser.parse_args()
    try:
        report = run(arguments)
        arguments.report.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        arguments.report.write_text(json.dumps(report, sort_keys=True, separators=(",", ":")), encoding="utf-8")
        arguments.report.chmod(0o600)
    except (ReplayInputError, ReplayError) as error:
        raise SystemExit(str(error)) from error
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
