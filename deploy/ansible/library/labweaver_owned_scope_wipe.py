#!/usr/bin/python3
"""Wipe a reviewed installation scope without name-addressed Helm uninstall.

All Kubernetes traffic goes through the configured kubectl transport, preserving
its kubeconfig authentication/TLS behavior. Discovery and every list are checked;
only an explicit API NotFound is absence. Before each DELETE the complete scope
and the reviewed Helm storage identities/revisions are checked again. DELETE
requests themselves carry both UID and resourceVersion preconditions.
"""
from __future__ import annotations

import base64
import gzip
import hashlib
import json
import re
import subprocess
import time
import zlib
from urllib.parse import quote, urlencode

import yaml
from ansible.module_utils.basic import AnsibleModule


class WipeError(RuntimeError):
    """A stable diagnostic, without command output or Helm credential values."""


class Kubernetes:
    def __init__(self, kubectl: str, kubeconfig: str):
        self.command = [kubectl, "--kubeconfig", kubeconfig]

    def request(self, path: str, delete: dict | None = None):
        args = self.command + (["get", "--raw", path] if delete is None else ["delete", "--raw", path, "-f", "-"])
        try:
            result = subprocess.run(args, input=None if delete is None else json.dumps(delete),
                                    capture_output=True, text=True, check=False, timeout=60)
        except (OSError, subprocess.TimeoutExpired) as error:
            raise WipeError("PLATFORM_WIPE_API_UNAVAILABLE") from error
        if result.returncode:
            if re.match(r"^Error from server \(NotFound\):", result.stderr.strip()):
                return None
            raise WipeError("PLATFORM_WIPE_DELETE_FAILED" if delete is not None else "PLATFORM_WIPE_READ_FAILED")
        if delete is not None:
            return True
        try:
            value = json.loads(result.stdout)
        except (ValueError, TypeError) as error:
            raise WipeError("PLATFORM_WIPE_API_RESPONSE_INVALID") from error
        if not isinstance(value, dict):
            raise WipeError("PLATFORM_WIPE_API_RESPONSE_INVALID")
        return value

    def listing(self, path: str) -> list[dict]:
        items, token, versions = [], "", set()
        seen = set()
        while True:
            page = self.request(path + "?" + urlencode({"limit": 500, "continue": token}))
            if page is None or not isinstance(page.get("items"), list):
                raise WipeError("PLATFORM_WIPE_LIST_FAILED")
            metadata = page.get("metadata", {})
            if not isinstance(metadata.get('resourceVersion'), str) or not metadata['resourceVersion']:
                raise WipeError('PLATFORM_WIPE_LIST_FAILED')
            versions.add(metadata.get("resourceVersion"))
            if len(versions) > 1:
                raise WipeError("PLATFORM_WIPE_LIST_CHANGED")
            items.extend(page["items"])
            token = metadata.get("continue", "")
            if not token:
                return items
            if not isinstance(token, str) or token in seen:
                raise WipeError("PLATFORM_WIPE_LIST_FAILED")
            seen.add(token)


def identity(obj: dict) -> tuple[str, str]:
    metadata = obj.get("metadata", {})
    uid, revision = metadata.get("uid"), metadata.get("resourceVersion")
    if not isinstance(uid, str) or not uid or not isinstance(revision, str) or not revision:
        raise WipeError("PLATFORM_WIPE_TARGET_UID_MISSING")
    return uid, revision


def object_path(collection: str, name: str) -> str:
    if not isinstance(name, str) or not name:
        raise WipeError("PLATFORM_WIPE_TARGET_INVALID")
    return collection + "/" + quote(name, safe="")


def discover(client: Kubernetes) -> tuple[dict, list[tuple[str, str]]]:
    core, grouped = client.request("/api"), client.request("/apis")
    if core is None or grouped is None or not isinstance(core.get("versions"), list) or not isinstance(grouped.get("groups"), list):
        raise WipeError("PLATFORM_WIPE_DISCOVERY_FAILED")
    versions = [(version, "/api/" + quote(version, safe="")) for version in core["versions"]]
    for group in grouped["groups"]:
        preferred = group.get("preferredVersion", {}).get("groupVersion")
        served = [version["groupVersion"] for version in group["versions"]]
        if preferred not in served:
            raise WipeError("PLATFORM_WIPE_DISCOVERY_FAILED")
        versions.extend((version, "/apis/" + quote(version, safe="/")) for version in [preferred] + [v for v in served if v != preferred])
    kinds, namespaced, seen = {}, [], set()
    for version, root in versions:
        response = client.request(root)
        if response is None or response.get("groupVersion") != version or not isinstance(response.get("resources"), list):
            raise WipeError("PLATFORM_WIPE_DISCOVERY_FAILED")
        for resource in response["resources"]:
            name, verbs = resource["name"], resource.get("verbs", [])
            if "/" in name:
                continue
            key = (version, resource["kind"])
            kinds[key] = (root, name, resource["namespaced"])
            group_resource = (version.rpartition("/")[0], name)
            if resource["namespaced"] and "delete" in verbs:
                if "list" not in verbs:
                    raise WipeError("PLATFORM_WIPE_RESOURCE_NOT_LISTABLE")
                if group_resource not in seen:
                    namespaced.append((root, name))
                    seen.add(group_resource)
    if ("v1", "Namespace") not in kinds or ("v1", "PersistentVolume") not in kinds:
        raise WipeError("PLATFORM_WIPE_DISCOVERY_FAILED")
    return kinds, namespaced


def release_record(obj: dict, installation: str) -> dict:
    try:
        metadata = obj["metadata"]
        labels = metadata["labels"]
        encoded = obj["data"]["release"]
        payload = base64.b64decode(encoded, validate=True)
        if obj["kind"] == "Secret":
            payload = base64.b64decode(payload, validate=True)
        if payload.startswith(b"\x1f\x8b"):
            payload = gzip.decompress(payload)
        release = json.loads(payload)
        name, namespace, version = release["name"], release["namespace"], release["version"]
        valid = (namespace == "labweaver-system" and name in ("labweaver", "labweaver-resource") and
                 type(version) is int and version > 0 and
                 metadata["name"] == f"sh.helm.release.v1.{name}.v{version}" and
                 metadata["namespace"] == namespace and labels["owner"] == "helm" and
                 labels["name"] == name and labels["version"] == str(version) and
                 release["info"]["description"] == "labweaver-installation:" + installation)
        if not valid:
            raise ValueError("unowned release storage")
        identity(obj)
        return release
    except (KeyError, TypeError, ValueError, OSError, EOFError, zlib.error) as error:
        raise WipeError("PLATFORM_WIPE_RELEASE_OWNERSHIP_CONFLICT") from error


def manifest_objects(release: dict) -> list[dict]:
    try:
        manifests = [release.get("manifest", "")] + [hook["manifest"] for hook in release.get("hooks", [])]
        result = []
        for manifest in manifests:
            for document in yaml.safe_load_all(manifest):
                if document is None:
                    continue
                if not isinstance(document, dict):
                    raise ValueError("invalid manifest")
                documents = document["items"] if document.get("kind") == "List" else [document]
                for obj in documents:
                    if not isinstance(obj, dict) or not isinstance(obj.get("apiVersion"), str) or not isinstance(obj.get("kind"), str):
                        raise ValueError("invalid object")
                    object_path("", obj["metadata"]["name"])
                    result.append(obj)
        return result
    except (KeyError, TypeError, ValueError, yaml.YAMLError) as error:
        raise WipeError("PLATFORM_WIPE_RELEASE_MANIFEST_INVALID") from error


def inspect(client: Kubernetes, installation: str, reviewed: dict) -> dict:
    kinds, collections = discover(client)
    namespaces, objects, locations = {}, {}, {}
    for name, expected in reviewed.items():
        path = object_path("/api/v1/namespaces", name)
        namespace = client.request(path)
        if namespace is None:
            continue
        if identity(namespace) != identity(expected):
            raise WipeError("PLATFORM_WIPE_NAMESPACE_CHANGED")
        if namespace.get("metadata", {}).get("annotations", {}).get("labweaver.io/installation-id") != installation:
            raise WipeError("PLATFORM_WIPE_NAMESPACE_OWNERSHIP_CONFLICT")
        namespaces[name] = namespace
        for root, resource in collections:
            collection = root + "/namespaces/" + quote(name, safe="") + "/" + resource
            for obj in client.listing(collection):
                if obj.get("metadata", {}).get("namespace") != name:
                    raise WipeError("PLATFORM_WIPE_LIST_FAILED")
                uid, _revision = identity(obj)
                path = object_path(collection, obj["metadata"]["name"])
                if uid in objects:
                    previous = objects[uid]
                    if (identity(previous) != identity(obj) or previous['kind'] != obj['kind'] or
                        previous['metadata']['name'] != obj['metadata']['name'] or
                        previous['metadata']['namespace'] != obj['metadata']['namespace'] or
                        (previous['apiVersion'] == obj['apiVersion'] and previous != obj)):
                        raise WipeError('PLATFORM_WIPE_LIST_CHANGED')
                    # Core Events and events.k8s.io expose the same immutable
                    # object through different wire representations.
                    continue
                objects[uid], locations[uid] = obj, path

    storage, releases, fingerprints = {}, [], []
    seen = set()
    for uid, obj in objects.items():
        metadata = obj["metadata"]
        if obj["kind"] in ("Secret", "ConfigMap") and (
            metadata.get("labels", {}).get("owner") == "helm" or obj.get("type") == "helm.sh/release.v1" or
            metadata["name"].startswith("sh.helm.release.v1.")
        ):
            release = release_record(obj, installation)
            key = (release["namespace"], release["name"], release["version"])
            if key in seen:
                raise WipeError("PLATFORM_WIPE_RELEASE_STORAGE_AMBIGUOUS")
            seen.add(key)
            storage[uid] = obj
            releases.append(release)
            fingerprints.append((metadata["namespace"], obj["kind"], metadata["name"], *identity(obj),
                                 hashlib.sha256(obj["data"]["release"].encode()).hexdigest()))

    targets, release_roots, preserved = {}, set(), set()
    for release in releases:
        for planned in manifest_objects(release):
            descriptor = kinds.get((planned["apiVersion"], planned["kind"]))
            if descriptor is None:
                raise WipeError("PLATFORM_WIPE_DISCOVERY_FAILED")
            root, resource, namespaced = descriptor
            namespace = planned["metadata"].get("namespace", release["namespace"]) if namespaced else ""
            collection = root + ("/namespaces/" + quote(namespace, safe="") if namespaced else "") + "/" + resource
            path = object_path(collection, planned["metadata"]["name"])
            release_roots.add((path, release["name"], release["namespace"]))
            if namespace in namespaces:
                continue
            obj = client.request(path)
            if obj is None:
                continue
            uid, _revision = identity(obj)
            if namespaced:
                boundary = client.request(object_path("/api/v1/namespaces", namespace))
                if boundary is None or boundary.get("metadata", {}).get("annotations", {}).get("labweaver.io/installation-id") != installation:
                    raise WipeError("PLATFORM_WIPE_NAMESPACE_OWNERSHIP_CONFLICT")
            objects[uid], locations[uid] = obj, path
            if planned["kind"] in ("Namespace", "CustomResourceDefinition") or planned["metadata"].get("annotations", {}).get("helm.sh/resource-policy") == "keep":
                preserved.add(path)
            else:
                targets[path] = obj

    # The namespace-local default Kubernetes identity is an explicitly bounded
    # platform target. A same-name account with foreign metadata or additional
    # credential/runtime settings is not covered by that authorization.
    default_accounts = {
        uid for uid, obj in objects.items()
        if obj['kind'] == 'ServiceAccount' and obj['metadata']['name'] == 'default'
        and obj['metadata'].get('namespace') in namespaces
        and not obj['metadata'].get('annotations') and not obj['metadata'].get('labels')
        and not obj.get('secrets') and not obj.get('imagePullSecrets')
        and obj.get('automountServiceAccountToken') is None
    }

    public_bundles, root_ca = set(), None
    for uid, obj in objects.items():
        metadata = obj['metadata']
        if (obj['kind'] == 'ConfigMap' and metadata['name'] == 'kube-root-ca.crt' and
            metadata.get('namespace') in namespaces and not metadata.get('labels') and
            set(metadata.get('annotations', {})) <= {'kubernetes.io/description'}):
            if root_ca is None:
                root_ca = client.request('/api/v1/namespaces/kube-system/configmaps/kube-root-ca.crt')
            if root_ca is not None and obj.get('data') == root_ca.get('data') and set(obj.get('data', {})) == {'ca.crt'} and not obj.get('binaryData'):
                public_bundles.add(uid)
    namespace_uids = {identity(namespace)[0] for namespace in namespaces.values()}

    def owned(uid: str, visiting: frozenset = frozenset()) -> bool:
        if uid in visiting or uid not in objects:
            return False
        obj = objects[uid]
        metadata = obj["metadata"]
        annotations = metadata.get("annotations", {})
        if uid in storage or uid in default_accounts or uid in public_bundles or annotations.get("labweaver.io/installation-id") == installation:
            return True
        if (locations[uid], annotations.get("meta.helm.sh/release-name"), annotations.get("meta.helm.sh/release-namespace")) in release_roots and metadata.get("labels", {}).get("app.kubernetes.io/managed-by") == "Helm":
            return True
        # Events refer to their subject by UID rather than ownerReferences.
        # Names and labels are not authority; the subject must resolve to the
        # exact reviewed namespace or an independently verified owned root.
        if obj['kind'] == 'Event':
            reference = (obj.get('involvedObject', {}) if obj['apiVersion'] == 'v1' else
                         obj.get('regarding', {}) if obj['apiVersion'].startswith('events.k8s.io/') else {})
            subject_uid = reference.get('uid')
            if subject_uid in namespace_uids or owned(subject_uid, visiting | {uid}):
                return True
        return any(owned(parent.get("uid"), visiting | {uid}) for parent in metadata.get("ownerReferences", []))

    for uid in objects:
        if not owned(uid):
            raise WipeError('PLATFORM_WIPE_OBJECT_OWNERSHIP_CONFLICT')

    volumes = client.listing("/api/v1/persistentvolumes")
    for volume in volumes:
        if volume.get("spec", {}).get("claimRef", {}).get("namespace") in reviewed:
            identity(volume)
            if volume.get("metadata", {}).get("annotations", {}).get("labweaver.io/installation-id") != installation:
                raise WipeError("PLATFORM_WIPE_VOLUME_OWNERSHIP_CONFLICT")
            targets[object_path("/api/v1/persistentvolumes", volume["metadata"]["name"])] = volume
    return {"namespaces": namespaces, "targets": targets, "storage": sorted(fingerprints), "preserved": sorted(preserved)}


def wipe(client: Kubernetes, installation: str, namespaces: list[dict], timeout: int, check: bool = False) -> dict:
    reviewed = {obj["metadata"]["name"]: obj for obj in namespaces}
    if len(reviewed) != len(namespaces):
        raise WipeError("PLATFORM_WIPE_TARGET_INVALID")
    initial = inspect(client, installation, reviewed)
    queue = dict(initial["targets"])
    queue.update((object_path("/api/v1/namespaces", name), obj) for name, obj in initial["namespaces"].items())
    preserved = [path for path in initial['preserved'] if path not in queue]
    if check:
        return {"changed": bool(queue), "deleted": [], "preserved": preserved}
    deleted = []
    pending = []
    for path, expected in queue.items():
        current = inspect(client, installation, reviewed)
        live_namespaces = current["namespaces"]
        expected_storage = [record for record in initial["storage"] if record[0] in live_namespaces]
        if current["storage"] != expected_storage:
            raise WipeError("PLATFORM_WIPE_RELEASE_REVISION_CHANGED")
        observed = client.request(path)
        if observed is None:
            reviewed = {name: obj for name, obj in reviewed.items() if object_path("/api/v1/namespaces", name) != path}
            continue
        uid, revision = identity(expected)
        if identity(observed) != (uid, revision):
            raise WipeError("PLATFORM_WIPE_TARGET_CHANGED")
        client.request(path, {"apiVersion": "v1", "kind": "DeleteOptions", "propagationPolicy": "Foreground",
                              "preconditions": {"uid": uid, "resourceVersion": revision}})
        # A PV's protection finalizer may require its PVC namespace to be
        # deleted first. Submit every conditional DELETE before waiting.
        pending.append((path, uid))
        reviewed = {name: obj for name, obj in reviewed.items() if object_path('/api/v1/namespaces', name) != path}
    for path, uid in pending:
        deadline = time.monotonic() + timeout
        while True:
            remaining = client.request(path)
            if remaining is None:
                break
            if identity(remaining)[0] != uid:
                raise WipeError("PLATFORM_WIPE_TARGET_CHANGED")
            if time.monotonic() >= deadline:
                raise WipeError("PLATFORM_WIPE_DELETE_TIMEOUT")
            time.sleep(1)
        deleted.append(path)
    return {"changed": bool(deleted), "deleted": deleted, "preserved": preserved}


def main() -> None:
    module = AnsibleModule(argument_spec={
        "kubectl": {"type": "path", "required": True}, "kubeconfig": {"type": "path", "required": True},
        "installation_id": {"type": "str", "required": True},
        "namespaces": {"type": "list", "elements": "dict", "required": True},
        "wait_timeout": {"type": "int", "default": 300},
    }, supports_check_mode=True)
    if not re.fullmatch(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", module.params["installation_id"]) or module.params["wait_timeout"] <= 0:
        module.fail_json(msg="PLATFORM_WIPE_INPUT_INVALID", diagnostic_code="PLATFORM_WIPE_INPUT_INVALID")
    try:
        result = wipe(Kubernetes(module.params["kubectl"], module.params["kubeconfig"]), module.params["installation_id"],
                      module.params["namespaces"], module.params["wait_timeout"], module.check_mode)
    except WipeError as error:
        module.fail_json(msg=str(error), diagnostic_code=str(error))
    except (KeyError, TypeError, ValueError, AttributeError) as error:
        module.fail_json(msg="PLATFORM_WIPE_API_RESPONSE_INVALID", diagnostic_code="PLATFORM_WIPE_API_RESPONSE_INVALID")
    module.exit_json(**result, diagnostic_code="PLATFORM_WIPE_SCOPE_DELETED")


if __name__ == "__main__":
    main()
