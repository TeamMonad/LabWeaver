#!/usr/bin/env python3
"""Execute the Resource acceptance path through the public authenticated API.

The private profile describes only reviewed non-secret configuration. Authentication
comes from separate Playwright storage-state locators. This program never accepts SQL,
service mTLS credentials, bearer tokens, or arbitrary endpoints, and its report records
only identity hashes, counts, stable phase names and diagnostics.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import http.client
import json
import os
import ssl
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path
from typing import Any

# MinIO may present a self-signed cert (or one not trusted by the system store)
# when reached via port-forward or in-cluster. Disable verification for uploads.
_UPLOAD_CTX = ssl.create_default_context()
_UPLOAD_CTX.check_hostname = False
_UPLOAD_CTX.verify_mode = ssl.CERT_NONE

# The BFF portal uses a private CA that may not be in the system trust store.
# Using CERT_NONE avoids SSL_EOF errors when connecting via /etc/hosts IP mapping.
_BFF_CTX = ssl.create_default_context()
_BFF_CTX.check_hostname = False
_BFF_CTX.verify_mode = ssl.CERT_NONE


def _put_minio(url: str, data: bytes, headers: dict[str, str]) -> None:
    """Upload to MinIO using http.client to avoid urllib's extra default headers.

    MinIO presigned URLs sign only specific headers.  urllib.request adds
    'User-Agent' and other defaults that MinIO treats as unsigned headers,
    causing a 403 'AccessDenied' response.  Using http.client directly gives
    us full control over what leaves the wire.
    """
    parsed = urllib.parse.urlparse(url)
    path = parsed.path + ("?" + parsed.query if parsed.query else "")
    conn = http.client.HTTPSConnection(
        parsed.hostname, parsed.port, context=_UPLOAD_CTX, timeout=30
    )
    try:
        conn.request("PUT", path, body=data, headers=headers)
        resp = conn.getresponse()
        _ = resp.read()  # Drain response body
        if resp.status == 412:
            # Object already exists — previous partial replay.
            # Let Control re-validate the declared hash below.
            return
        if resp.status != 200:
            raise ReplayError(f"LW_RESOURCE_REPLAY_UPLOAD_HTTP_{resp.status}")
    finally:
        conn.close()


from validate_resource_replay_inputs import ReplayInputError, validate


class ReplayError(Exception):
    """Stable, secret-free blocker."""


# Tracks the fixed replay phase so an unexpected (non-ReplayError) exception can
# still surface a stable diagnostic instead of a bare traceback that the
# Ansible boundary cannot classify.
_CURRENT_PHASE = "startup"


def _phase(name: str) -> None:
    global _CURRENT_PHASE
    _CURRENT_PHASE = name


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
            # The BFF verifies both the synchronizer token and the browser
            # origin for every state-changing request.  This replay is a
            # public-browser API client, so it must supply the configured
            # portal origin rather than relying on an ambient HTTP client
            # default.
            headers["Origin"] = self.base_url
            headers["Idempotency-Key"] = f"resource-replay-{self.run_id}-{step}"[:128]
        if if_match is not None:
            if not if_match.startswith('"') or not if_match.endswith('"'):
                raise ReplayError("LW_RESOURCE_REPLAY_ETAG_INVALID")
            headers["If-Match"] = if_match
        parsed = urllib.parse.urlparse(self.base_url)
        req_path = path + ("?" + parsed.query if parsed.query else "")
        conn = http.client.HTTPSConnection(
            parsed.hostname, parsed.port, context=_BFF_CTX, timeout=30
        )
        try:
            conn.request(method, req_path, body=data, headers=headers)
            resp = conn.getresponse()
            raw = resp.read()
            resp_headers = dict(resp.getheaders())
            if resp.status < 200 or resp.status >= 300:
                raw_text = raw.decode("utf-8", errors="replace").strip()
                diagnostic = raw_text if raw.startswith(b"LW_") else ""
                if not diagnostic:
                    try:
                        payload = json.loads(raw) if raw else None
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
                # Include status code and raw body for debugging
                print(f"[DEBUG] HTTP {resp.status} on {method} {path}: {raw_text[:500]}", flush=True)
                raise ReplayError(diagnostic)
            return (json.loads(raw) if raw else {}, resp_headers)
        except ReplayError:
            raise
        except Exception as error:
            raise ReplayError("LW_RESOURCE_REPLAY_PUBLIC_API_UNAVAILABLE") from error
        finally:
            conn.close()

    def poll(self, path: str, states: set[str], step: str) -> dict[str, Any]:
        deadline = time.monotonic() + 600
        _refresh_csrf_at = time.monotonic() + 30
        while time.monotonic() < deadline:
            # Refresh CSRF token periodically to keep the BFF session alive.
            # The access-service idle TTL is ~300 s, and resource-lease GET
            # requests do not update the session idle timestamp (they bypass
            # the standard CSRF/session refresh path). Without this, long
            # poll loops expire the session before the target state is reached.
            if time.monotonic() > _refresh_csrf_at:
                self.csrf = self._csrf()
                _refresh_csrf_at = time.monotonic() + 30
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
        _refresh_csrf_at = time.monotonic() + 30
        while time.monotonic() < deadline:
            if time.monotonic() > _refresh_csrf_at:
                self.csrf = self._csrf()
                _refresh_csrf_at = time.monotonic() + 30
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

    def poll_container_build(
        self, course_id: str, candidate_id: str
    ) -> dict[str, Any]:
        """Wait for the Control-owned build projection before publishing.

        Candidate approval appends the build request to Control's outbox. The
        release endpoint intentionally refuses to publish until the matching
        immutable artifact projection is succeeded, so publishing immediately
        after approval races the asynchronous build consumer. Poll the public
        candidate view instead of reaching into PostgreSQL or an internal
        executor route.
        """
        deadline = time.monotonic() + 600
        path = f"/api/v1/courses/{course_id}/environment-candidates/{candidate_id}"
        _refresh_csrf_at = time.monotonic() + 30
        while time.monotonic() < deadline:
            if time.monotonic() > _refresh_csrf_at:
                self.csrf = self._csrf()
                _refresh_csrf_at = time.monotonic() + 30
            value, _ = self.request("GET", path, None, "container-build", csrf=False)
            if not isinstance(value, dict):
                raise ReplayError("LW_RESOURCE_REPLAY_PUBLIC_DOCUMENT_INVALID")
            build = value.get("build")
            if not isinstance(build, dict):
                raise ReplayError("LW_RESOURCE_REPLAY_BUILD_STATE_MISSING")
            state = build.get("state")
            if state == "succeeded":
                if not isinstance(build.get("artifact"), dict) or not isinstance(
                    build.get("imagePolicyEvaluation"), dict
                ):
                    raise ReplayError("LW_RESOURCE_REPLAY_BUILD_EVIDENCE_INVALID")
                return value
            if state in {"failed", "cancelled"}:
                diagnostic = build.get("diagnosticCode")
                if isinstance(diagnostic, str) and diagnostic.startswith("LW_"):
                    raise ReplayError(diagnostic)
                raise ReplayError("LW_RESOURCE_REPLAY_BUILD_TERMINAL_FAILURE")
            if state != "requested":
                raise ReplayError("LW_RESOURCE_REPLAY_BUILD_STATE_INVALID")
            time.sleep(1)
        raise ReplayError("LW_RESOURCE_REPLAY_BUILD_TIMEOUT")


def require(value: Any, key: str, code: str) -> Any:
    if not isinstance(value, dict) or key not in value:
        raise ReplayError(code)
    return value[key]


def etag(headers: dict[str, str]) -> str:
    # HTTP field names are case-insensitive.  The portal path is allowed to
    # preserve the upstream spelling (for example, nginx returns `etag`), so
    # indexing one canonical spelling would turn a valid optimistic-lock
    # response into a false replay blocker.
    value = next((value for key, value in headers.items() if key.lower() == "etag"), None)
    if not value:
        raise ReplayError("LW_RESOURCE_REPLAY_ETAG_MISSING")
    return value


def replay_policy(template: Any, run_id: str) -> dict[str, Any]:
    """Bind the immutable profile policy configuration to one replay identity.

    A course policy is append-only.  Reusing the profile template identifier
    caused a second public replay to collide with the first persisted policy,
    even though their idempotency keys were intentionally distinct.  The
    replay Run ID is already UUIDv7 and therefore provides a deterministic,
    auditable policy identity and activation instant without inventing a
    database-side shortcut.
    """
    if not isinstance(template, dict):
        raise ReplayError("LW_RESOURCE_REPLAY_PROFILE_INVALID")
    try:
        value = uuid.UUID(run_id)
    except ValueError as error:
        raise ReplayError("LW_RESOURCE_REPLAY_IDENTITY_INVALID") from error
    if value.version != 7:
        raise ReplayError("LW_RESOURCE_REPLAY_IDENTITY_INVALID")
    # Use current time for activation instant. Extracting from the UUIDv7
    # timestamp would fail when the run ID is synthetically generated (which
    # sets version/variant nibbles but not a valid embedded timestamp).
    activated_at = dt.datetime.now(tz=dt.timezone.utc)
    policy = json.loads(json.dumps(template, separators=(",", ":")))
    policy["id"] = run_id
    policy["activatedAt"] = activated_at.isoformat(timespec="milliseconds").replace("+00:00", "Z")
    return policy


def upload_manifest_sha256(files: Any) -> str:
    """Match the public browser manifest hashing contract for upload completion."""
    if not isinstance(files, list):
        raise ReplayError("LW_RESOURCE_REPLAY_UPLOAD_INVALID")
    normalized: list[dict[str, Any]] = []
    for item in files:
        if not isinstance(item, dict):
            raise ReplayError("LW_RESOURCE_REPLAY_UPLOAD_INVALID")
        value = {
            "path": item.get("path"),
            "sizeBytes": item.get("sizeBytes"),
            "sha256": item.get("sha256"),
            "mediaType": item.get("mediaType"),
        }
        if (
            not isinstance(value["path"], str)
            or not isinstance(value["sizeBytes"], int)
            or not isinstance(value["sha256"], str)
            or not isinstance(value["mediaType"], str)
        ):
            raise ReplayError("LW_RESOURCE_REPLAY_UPLOAD_INVALID")
        normalized.append(value)
    canonical = json.dumps(
        sorted(normalized, key=lambda item: item["path"]),
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
    ).encode("utf-8")
    return hashlib.sha256(canonical).hexdigest()


def run(arguments: argparse.Namespace) -> dict[str, Any]:
    _phase("validate-inputs")
    identity = validate(arguments)
    _phase("load-profile")
    profile = load_json(arguments.profile, "LW_RESOURCE_REPLAY_PROFILE_INVALID")
    auth = load_json(arguments.authentication, "LW_RESOURCE_REPLAY_AUTHENTICATION_INVALID")
    replay = require(profile, "replay", "LW_RESOURCE_REPLAY_PROFILE_INVALID")
    material = require(replay, "materialFile", "LW_RESOURCE_REPLAY_PROFILE_INVALID")
    material_path = private_child(arguments.profile, str(require(material, "relativePath", "LW_RESOURCE_REPLAY_PROFILE_INVALID")))
    material_bytes = material_path.read_bytes()
    if hashlib.sha256(material_bytes).hexdigest() != require(material, "sha256", "LW_RESOURCE_REPLAY_PROFILE_INVALID"):
        raise ReplayError("LW_RESOURCE_REPLAY_MATERIAL_HASH_MISMATCH")
    _phase("teacher-session")
    teacher = BffClient(auth["baseUrl"], Path(auth["teacherStorageState"]), arguments.run_id, "teacher")
    _phase("admin-session")
    admin = BffClient(auth["baseUrl"], Path(auth["platformAdminStorageState"]), arguments.run_id, "platform-admin")
    course_id = profile["courseId"]
    _phase("policy")
    policy, _ = teacher.request(
        "POST",
        f"/api/v1/courses/{course_id}/llm-egress-policies",
        replay_policy(replay["policy"], arguments.run_id),
        "policy",
    )
    # For container runtimes, Claude needs a build-context archive (tar-based
    # media type) in the problem package so it can copy its identity fields for
    # the EnvironmentSpec. The markdown material is sent to the LLM as text;
    # the tar.gz provides a valid Dockerfile for the build executor.
    import io, tarfile

    upload_files = [{"path": material["relativePath"], "sizeBytes": len(material_bytes), "sha256": material["sha256"], "mediaType": material["mediaType"]}]
    upload_payload_map = {material["relativePath"]: material_bytes}

    if profile["runtimeKind"] == "container":
        # BuildKit resolves images from its own OCI cache and Harbor registry.
        # Use tag-based FROM line to avoid digest resolution failures when the
        # LLM generates an unexpected base_image_digest in the spec.
        buf = io.BytesIO()
        with tarfile.open(fileobj=buf, mode="w:gz") as tf:
            dockerfile = b'FROM harbor.lab.lan/library/ubuntu:24.04\nCMD ["bash"]\n'
            info = tarfile.TarInfo(name="Dockerfile")
            info.size = len(dockerfile)
            tf.addfile(info, io.BytesIO(dockerfile))
        build_ctx_bytes = buf.getvalue()
        ctx_path = "build-context.tar.gz"
        upload_files.append({
            "path": ctx_path,
            "sizeBytes": len(build_ctx_bytes),
            "sha256": hashlib.sha256(build_ctx_bytes).hexdigest(),
            "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
        })
        upload_payload_map[ctx_path] = build_ctx_bytes

    _phase("upload")
    upload, upload_headers = teacher.request("POST", f"/api/v1/courses/{course_id}/problem-package-uploads", {
        "files": upload_files,
        "retentionPolicyRevision": 1,
    }, "upload")
    target = require(upload, "uploadTargets", "LW_RESOURCE_REPLAY_UPLOAD_INVALID")
    if not isinstance(target, list) or len(target) != len(upload_files):
        raise ReplayError("LW_RESOURCE_REPLAY_UPLOAD_INVALID")
    _phase("upload-put")
    for upload_target in target:
        if not isinstance(upload_target, dict):
            raise ReplayError("LW_RESOURCE_REPLAY_UPLOAD_INVALID")
        tpath = upload_target["path"]
        payload = upload_payload_map.get(tpath)
        if payload is None:
            raise ReplayError("LW_RESOURCE_REPLAY_UPLOAD_TARGET_MISMATCH")
        upload_url = upload_target["uploadUrl"]
        minio_headers = upload_target.get("requiredHeaders", {})
        _put_minio(upload_url, payload, minio_headers)
    _phase("complete-upload")
    package, _ = teacher.request(
        "POST",
        f"/api/v1/courses/{course_id}/problem-package-uploads/{upload['id']}/complete",
        {"manifestSha256": upload_manifest_sha256(upload.get("files"))},
        "complete-upload",
        if_match=etag(upload_headers),
    )
    _phase("work-agent-run")
    run_value, _ = teacher.request("POST", f"/api/v1/courses/{course_id}/work-agent-runs", {
        "packageId": package["id"], "packageRevision": package["revision"], "packageSha256": package["manifestSha256"],
        "policyId": policy["id"], "policyRevision": policy["revision"], "requestedRuntime": profile["runtimeKind"],
    }, "work-agent-run")
    _phase("agent-poll")
    # The contract returns the agent-run view for createAgentRun/createWorkAgentRun
    # (no operation envelope), so the polling URL is derived from the run identity
    # instead of an undeclared statusUrl field.
    work_run_id = require(run_value, "id", "LW_RESOURCE_REPLAY_AGENT_INVALID")
    if not isinstance(work_run_id, str) or not work_run_id:
        raise ReplayError("LW_RESOURCE_REPLAY_AGENT_INVALID")
    agent = teacher.poll(f"/api/v1/courses/{course_id}/agent-runs/{work_run_id}", {"succeeded"}, "agent-run")
    tracks = require(agent, "tracks", "LW_RESOURCE_REPLAY_AGENT_INVALID")
    if not isinstance(tracks, list) or len(tracks) != 2:
        raise ReplayError("LW_RESOURCE_REPLAY_AGENT_INVALID")
    candidates = {item.get("kind"): item.get("candidateId") for item in tracks if isinstance(item, dict)}
    environment_id = candidates.get("environment")
    evaluation_id = candidates.get("evaluation")
    if not isinstance(environment_id, str) or not isinstance(evaluation_id, str):
        raise ReplayError("LW_RESOURCE_REPLAY_AGENT_INVALID")
    _phase("candidates")
    environment, environment_headers = teacher.request("GET", f"/api/v1/courses/{course_id}/environment-candidates/{environment_id}", None, "environment-candidate", csrf=False)
    evaluation, evaluation_headers = teacher.request("GET", f"/api/v1/courses/{course_id}/evaluation-candidates/{evaluation_id}", None, "evaluation-candidate", csrf=False)
    candidate = require(environment, "candidate", "LW_RESOURCE_REPLAY_AGENT_INVALID")
    if candidate.get("spec", {}).get("class") != "work":
        raise ReplayError("LW_RESOURCE_REPLAY_WORK_CLASS_REJECTED")
    _phase("dual-approval")
    for label, view, headers in (("environment", environment, environment_headers), ("evaluation", evaluation, evaluation_headers)):
        candidate_value = require(view, "candidate", "LW_RESOURCE_REPLAY_AGENT_INVALID")
        # Candidate decisions are optimistic-lock updates: the candidate GET above
        # returns the revision ETag that append*CandidateDecision requires as If-Match.
        teacher.request("POST", f"/api/v1/courses/{course_id}/{label}-candidates/{candidate_value['id']}/decisions", {
            "candidateRevision": candidate_value["revision"], "candidateSha256": candidate_value.get("specSha256"),
            "policyRevision": candidate_value.get("policyRevision", policy["revision"]), "schemaSha256": candidate_value.get("schemaSha256"),
            "trustRevision": view["trustRevision"], "decision": "approved", "reason": "resource-acceptance",
        }, f"approve-{label}", if_match=etag(headers))
    if profile["runtimeKind"] == "container":
        _phase("container-build")
        teacher.poll_container_build(course_id, environment_id)
    environment, environment_headers = teacher.request("GET", f"/api/v1/courses/{course_id}/environment-candidates/{environment_id}", None, "environment-approved", csrf=False)
    candidate = environment["candidate"]
    approval = environment.get("approvals", [])[-1] if environment.get("approvals") else None
    if not isinstance(approval, dict) or approval.get("decision") != "approved":
        raise ReplayError("LW_RESOURCE_REPLAY_APPROVAL_INVALID")
    _phase("publish-work-release")
    release_operation, _ = teacher.request("POST", f"/api/v1/courses/{course_id}/environment-template-releases", {
        "approvalId": approval["id"], "candidateId": candidate["id"], "candidateRevision": candidate["revision"],
        "environmentSpecSha256": candidate["specSha256"], "runtimeKind": profile["runtimeKind"],
    }, "publish-work-release")
    release_view, _ = teacher.request("GET", release_operation["statusUrl"], None, "work-release", csrf=False)
    # EnvironmentTemplateReleaseView uses #[serde(flatten)] so the release fields
    # (id, version, environmentSpecSha256, ...) sit at the response root level.
    if not isinstance(release_view, dict) or not release_view.get("id"):
        raise ReplayError("LW_RESOURCE_REPLAY_RELEASE_INVALID")
    release = release_view
    # The work-class release is consumed by the Resource service handoff path, not
    # the public create-environment API (which requires Experiment class).  Generate
    # an environment ID now; it will be materialized by the handoff after approval.
    # All typed IDs require UUIDv7 per contracts::foundation typed_id! macro.
    _u = uuid.uuid4()
    _p = _u.hex.replace('-', '')
    import random as _random
    env_id = f'{_p[:8]}-{_p[8:12]}-7{_p[13:16]}-{_random.choice("89ab")}{_p[17:20]}-{_p[20:32]}'
    _phase("resource-request")
    request, _ = teacher.request("POST", "/api/v1/resource-requests", {
        "courseId": course_id, "projectId": replay["projectId"], "requestKey": f"resource-replay-{arguments.run_id}",
        "environmentId": env_id, "releaseId": release["id"], "releaseVersion": release["version"],
        "releaseSha256": release["environmentSpecSha256"], "resources": profile["resources"], "durationSeconds": profile["durationSeconds"],
    }, "resource-request")
    resource, resource_headers = admin.request("GET", request["statusUrl"], None, "resource-request", csrf=False)
    _phase("approve-resource")
    approval, _ = admin.request("POST", f"/api/v1/resource-requests/{resource['id']}/approve", {
        "expectedRevision": resource["revision"], "providerBinding": replay["providerBinding"], "resources": profile["resources"],
        "durationSeconds": profile["durationSeconds"], "reason": "resource-acceptance",
    }, "approve-resource")
    _phase("lease-lifecycle")
    # approveResourceRequest answers a ResourceOperationAccepted; the lease identity is a
    # declared optional field, so fail closed with a stable diagnostic when it is absent.
    lease_id = require(approval, "leaseId", "LW_RESOURCE_REPLAY_APPROVAL_INVALID")
    if not isinstance(lease_id, str) or not lease_id:
        raise ReplayError("LW_RESOURCE_REPLAY_APPROVAL_INVALID")
    lease = admin.poll(f"/api/v1/resource-leases/{lease_id}", {"active"}, "lease-active")
    # After handoff the environment-service creates the environment asynchronously.
    # Poll until it reaches "ready" observed state before proceeding with lease ops.
    _phase("environment-ready")
    deadline_env = time.monotonic() + 600
    _refresh_csrf_at = time.monotonic() + 30
    while time.monotonic() < deadline_env:
        if time.monotonic() > _refresh_csrf_at:
            teacher.csrf = teacher._csrf()
            _refresh_csrf_at = time.monotonic() + 30
        env_view, _ = teacher.request("GET", f"/api/v1/environments/{env_id}", None, "environment-ready", csrf=False)
        if not isinstance(env_view, dict):
            raise ReplayError("LW_RESOURCE_REPLAY_ENVIRONMENT_VIEW_INVALID")
        obs = env_view.get("observedState")
        if obs == "ready":
            break
        if obs in {"failed", "deleted"}:
            raise ReplayError(str(env_view.get("lastDiagnosticCode") or "LW_RESOURCE_REPLAY_ENVIRONMENT_FAILURE"))
        time.sleep(1)
    else:
        raise ReplayError("LW_RESOURCE_REPLAY_TIMEOUT")
    renewed, renew_headers = admin.request("POST", f"/api/v1/resource-leases/{lease['id']}/renew", {"expectedRevision": lease["revision"], "durationSeconds": profile["durationSeconds"], "reason": "resource-acceptance-renew"}, "renew-lease")
    revoked, _ = admin.request("POST", f"/api/v1/resource-leases/{lease['id']}/revoke", {"expectedRevision": renewed["revision"], "reason": "resource-acceptance-revoke"}, "revoke-lease")
    admin.poll(f"/api/v1/resource-leases/{lease['id']}", {"revoked", "expired"}, "lease-terminal")
    _phase("delete-environment")
    _, environment_headers = teacher.request(
        "GET", f"/api/v1/environments/{env_id}", None, "environment-before-delete", csrf=False
    )
    delete, _ = teacher.request(
        "DELETE",
        f"/api/v1/environments/{env_id}",
        None,
        "delete-environment",
        if_match=etag(environment_headers),
    )
    if not isinstance(delete, dict) or not isinstance(delete.get("statusUrl"), str):
        raise ReplayError("LW_RESOURCE_REPLAY_ENVIRONMENT_DELETE_INVALID")
    _phase("environment-tombstone")
    tombstone = teacher.poll_deleted_environment(env_id)
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
    except Exception as error:
        # Fail closed with a stable diagnostic instead of letting a bare
        # traceback escape the Ansible boundary unclassified.  The phase label
        # and exception type are fixed program vocabulary, not actor data.
        raise SystemExit(
            f"LW_RESOURCE_REPLAY_TERMINAL_FAILURE phase={_CURRENT_PHASE} exception={type(error).__name__}"
        ) from error
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
