"""Exercise discovered wipe ownership and race checks at the Kubernetes API boundary."""
from __future__ import annotations

import base64
import copy
import gzip
import importlib.util
import http.server
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
from urllib.parse import parse_qs, urlsplit

import yaml

from test_deployment_install_modes import INSTALLATION, ROOT, tasks

MODULE = ROOT / "deploy/ansible/library/labweaver_owned_scope_wipe.py"
SPEC = importlib.util.spec_from_file_location("owned_scope_wipe_test", MODULE)
assert SPEC is not None and SPEC.loader is not None
WIPE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(WIPE)

# This is Kubernetes discovery data, not a product-resource allowlist. The
# unknown API group verifies the production code never relies on known CR kinds.
RESOURCES = {
    "v1": [("Namespace", "namespaces", False), ("PersistentVolume", "persistentvolumes", False),
           ("Pod", "pods", True), ("ConfigMap", "configmaps", True), ("Secret", "secrets", True),
           ("ServiceAccount", "serviceaccounts", True)],
    "apps/v1": [("Deployment", "deployments", True), ("ReplicaSet", "replicasets", True), ("DaemonSet", "daemonsets", True)],
    "gateway.networking.k8s.io/v1": [("Gateway", "gateways", True), ("HTTPRoute", "httproutes", True), ("TLSRoute", "tlsroutes", True)],
    "cdi.kubevirt.io/v1beta1": [("DataVolume", "datavolumes", True), ("DataSource", "datasources", True)],
    "foreign.example/v1": [("PreviouslyUnknown", "previouslyunknowns", True)],
    "rbac.authorization.k8s.io/v1": [("ClusterRole", "clusterroles", False), ("ClusterRoleBinding", "clusterrolebindings", False)],
    "apiextensions.k8s.io/v1": [("CustomResourceDefinition", "customresourcedefinitions", False)],
}


def root(version):
    return ("/api/" if "/" not in version else "/apis/") + version


def obj(kind, name="owned", namespace="labweaver-system", installation=INSTALLATION):
    version = next(version for version, resources in RESOURCES.items() if any(entry[0] == kind for entry in resources))
    namespaced = next(entry[2] for entry in RESOURCES[version] if entry[0] == kind)
    metadata = {"name": name, "uid": f"uid-{kind}-{name}", "resourceVersion": "17", "annotations": {}}
    if namespaced:
        metadata["namespace"] = namespace
    if installation is not None:
        metadata["annotations"]["labweaver.io/installation-id"] = installation
    return {"apiVersion": version, "kind": kind, "metadata": metadata}


def path_for(value):
    version, kind, metadata = value["apiVersion"], value["kind"], value["metadata"]
    resource, namespaced = next((name, scope) for item_kind, name, scope in RESOURCES[version] if item_kind == kind)
    return root(version) + ("/namespaces/" + metadata["namespace"] if namespaced else "") + "/" + resource + "/" + metadata["name"]


def release_storage(manifest=(), *, revision=1, name="labweaver", installation=INSTALLATION, kind="Secret", hooks=()):
    value = {"name": name, "namespace": "labweaver-system", "version": revision,
             "info": {"description": "labweaver-installation:" + installation, "status": "deployed"},
             "manifest": yaml.safe_dump_all(manifest), "hooks": [{"manifest": yaml.safe_dump(item)} for item in hooks]}
    encoded = base64.b64encode(gzip.compress(json.dumps(value).encode()))
    if kind == "Secret":
        encoded = base64.b64encode(encoded)
    storage = obj(kind, f"sh.helm.release.v1.{name}.v{revision}", installation=None)
    storage["metadata"]["labels"] = {"owner": "helm", "name": name, "version": str(revision), "status": "deployed"}
    storage["data"] = {"release": encoded.decode()}
    if kind == "Secret":
        storage["type"] = "helm.sh/release.v1"
    return storage


def helm_object(kind, name="owned", namespace="labweaver-system", release="labweaver"):
    value = obj(kind, name, namespace, installation=None)
    value["metadata"]["annotations"] = {"meta.helm.sh/release-name": release, "meta.helm.sh/release-namespace": "labweaver-system"}
    value["metadata"]["labels"] = {"app.kubernetes.io/managed-by": "Helm"}
    return value


class KubernetesAPI(WIPE.Kubernetes):
    """Controlled Kubernetes API responses; production discovery/listing stay real."""
    def __init__(self, values):
        self.values = {path_for(value): copy.deepcopy(value) for value in values}
        self.attempts, self.deleted, self.reads = [], [], []
        self.discovery_count = 0
        self.discovery_hook = None
        self.delete_hook = None
        self.failure_path = None
        self.page_size = 500
        self.page_revision_changes = False
        self.extra_resource = None
        self.defer_volumes = False
        self.pending_volumes = []

    def request(self, path, delete=None):
        parsed = urlsplit(path)
        clean = parsed.path
        if delete is not None:
            self.attempts.append((clean, copy.deepcopy(delete)))
            if self.delete_hook:
                self.delete_hook(self, clean)
            current = self.values.get(clean)
            if current is None:
                return None
            preconditions = delete.get("preconditions", {})
            if any(current["metadata"][field] != expected for field, expected in preconditions.items()):
                raise WIPE.WipeError("PLATFORM_WIPE_DELETE_FAILED")
            self.deleted.append(clean)
            if self.defer_volumes and current['kind'] == 'PersistentVolume':
                self.pending_volumes.append(clean)
                current['metadata']['resourceVersion'] = 'delete-submitted'
                return True
            del self.values[clean]
            if current["kind"] == "Namespace":
                for item_path in list(self.values):
                    if "/namespaces/" + current["metadata"]["name"] + "/" in item_path:
                        del self.values[item_path]
                for volume_path in self.pending_volumes:
                    self.values.pop(volume_path, None)
            return True
        self.reads.append(path)
        if clean == "/api":
            self.discovery_count += 1
            if self.discovery_hook:
                self.discovery_hook(self)
        if clean == self.failure_path:
            raise WIPE.WipeError("PLATFORM_WIPE_READ_FAILED")
        if clean == "/api":
            return {"versions": ["v1"]}
        if clean == "/apis":
            return {"groups": [{"name": version.split("/")[0], "preferredVersion": {"groupVersion": version},
                                "versions": [{"groupVersion": version}]} for version in RESOURCES if "/" in version]}
        for version, entries in RESOURCES.items():
            if clean == root(version):
                resources = [{"name": name, "kind": kind, "namespaced": namespaced, "verbs": ["get", "list", "delete"]}
                             for kind, name, namespaced in entries]
                resources.append({"name": "deployments/scale", "kind": "Scale", "namespaced": True, "verbs": ["get"]})
                if self.extra_resource and version == "v1":
                    resources.append(self.extra_resource)
                return {"groupVersion": version, "resources": resources}
        if "limit" in parse_qs(parsed.query):
            offset = int(parse_qs(parsed.query).get("continue", ["0"])[0] or "0")
            values = [value for item_path, value in sorted(self.values.items()) if item_path.rpartition("/")[0] == clean]
            page = values[offset:offset + self.page_size]
            token = str(offset + self.page_size) if offset + self.page_size < len(values) else ""
            return {"items": copy.deepcopy(page), "metadata": {"continue": token,
                    "resourceVersion": "changed" if self.page_revision_changes and offset else "list-version"}}
        return copy.deepcopy(self.values.get(clean))


class DiscoveredWipeOwnershipTests(unittest.TestCase):
    def test_foreign_gateway_cdi_and_unknown_custom_resources_prevent_every_delete(self):
        for kind in ("Gateway", "HTTPRoute", "TLSRoute", "DataVolume", "DataSource", "PreviouslyUnknown"):
            with self.subTest(kind=kind):
                namespace = obj("Namespace", "labweaver-system")
                foreign = obj(kind, installation="another-installation")
                foreign["metadata"]["labels"] = {"app.kubernetes.io/part-of": "labweaver"}
                api = KubernetesAPI([namespace, foreign])
                with self.assertRaisesRegex(WIPE.WipeError, "PLATFORM_WIPE_OBJECT_OWNERSHIP_CONFLICT"):
                    WIPE.wipe(api, INSTALLATION, [namespace], 1)
                self.assertEqual(api.attempts, [])

    def test_discovery_and_each_list_error_fail_before_any_deletion(self):
        namespace = obj("Namespace", "labweaver-system")
        for failure in ("/api", "/apis", "/apis/foreign.example/v1",
                        "/apis/foreign.example/v1/namespaces/labweaver-system/previouslyunknowns"):
            with self.subTest(failure=failure):
                api = KubernetesAPI([namespace])
                api.failure_path = failure
                with self.assertRaises(WIPE.WipeError):
                    WIPE.wipe(api, INSTALLATION, [namespace], 1)
                self.assertEqual(api.attempts, [])
        api = KubernetesAPI([namespace])
        api.extra_resource = {"name": "opaque", "kind": "Opaque", "namespaced": True, "verbs": ["delete"]}
        with self.assertRaisesRegex(WIPE.WipeError, "PLATFORM_WIPE_RESOURCE_NOT_LISTABLE"):
            WIPE.wipe(api, INSTALLATION, [namespace], 1)
        self.assertEqual(api.attempts, [])

    def test_paginated_unknown_resources_are_all_checked_and_inconsistent_pages_rejected(self):
        namespace = obj("Namespace", "labweaver-system")
        for foreign, changing in ((True, False), (False, True), (False, False)):
            with self.subTest(foreign_last_page=foreign, changing_pages=changing):
                api = KubernetesAPI([namespace, obj("PreviouslyUnknown", "a"),
                                     obj("PreviouslyUnknown", "b", installation="foreign" if foreign else INSTALLATION)])
                api.page_size, api.page_revision_changes = 1, changing
                if foreign or changing:
                    with self.assertRaises(WIPE.WipeError):
                        WIPE.wipe(api, INSTALLATION, [namespace], 1)
                    self.assertEqual(api.attempts, [])
                else:
                    result = WIPE.wipe(api, INSTALLATION, [namespace], 1)
                    self.assertEqual(result["deleted"], [path_for(namespace)])
                self.assertTrue(any("continue=1" in read for read in api.reads))

    def test_verified_helm_storage_roots_and_controller_uid_chains_are_accepted(self):
        namespace = obj("Namespace", "labweaver-system")
        deployment = helm_object("Deployment")
        replica, pod = obj("ReplicaSet", installation=None), obj("Pod", installation=None)
        replica["metadata"]["ownerReferences"] = [{"uid": deployment["metadata"]["uid"]}]
        pod["metadata"]["ownerReferences"] = [{"uid": replica["metadata"]["uid"]}]
        for driver in ("Secret", "ConfigMap"):
            with self.subTest(driver=driver):
                api = KubernetesAPI([namespace, deployment, replica, pod, release_storage([deployment], kind=driver)])
                result = WIPE.wipe(api, INSTALLATION, [namespace], 1)
                self.assertEqual(result["deleted"], [path_for(namespace)])
                self.assertEqual(api.attempts[0][1]["preconditions"], {"uid": namespace["metadata"]["uid"], "resourceVersion": "17"})
        for parent in ("missing", pod["metadata"]["uid"]):
            pod["metadata"]["ownerReferences"] = [{"uid": parent}]
            api = KubernetesAPI([namespace, deployment, replica, pod, release_storage([deployment])])
            with self.assertRaisesRegex(WIPE.WipeError, "PLATFORM_WIPE_OBJECT_OWNERSHIP_CONFLICT"):
                WIPE.wipe(api, INSTALLATION, [namespace], 1)
            self.assertEqual(api.attempts, [])

    def test_release_labels_without_verified_storage_or_manifest_identity_are_not_authority(self):
        namespace = obj("Namespace", "labweaver-system")
        for storage in ([], [release_storage(installation="foreign")], [release_storage(name="foreign")], [release_storage()]):
            with self.subTest(storage=bool(storage)):
                api = KubernetesAPI([namespace, helm_object("Deployment")] + storage)
                with self.assertRaises(WIPE.WipeError):
                    WIPE.wipe(api, INSTALLATION, [namespace], 1)
                self.assertEqual(api.attempts, [])

    def test_all_release_history_cluster_objects_hooks_and_external_namespaced_targets_use_uid_deletes(self):
        namespace, portal = obj("Namespace", "labweaver-system"), obj("Namespace", "portal")
        cluster = helm_object("ClusterRole", "old-chart-root")
        hook = helm_object("ClusterRoleBinding", "release-hook")
        route = helm_object("HTTPRoute", "portal", "portal")
        volume = obj("PersistentVolume", "owned-volume")
        volume["spec"] = {"claimRef": {"namespace": "labweaver-system"}}
        api = KubernetesAPI([namespace, portal, cluster, hook, route, volume,
                             release_storage([cluster, route], revision=1, hooks=[hook]), release_storage(revision=2)])
        result = WIPE.wipe(api, INSTALLATION, [namespace], 1)
        self.assertEqual(set(result["deleted"]), {path_for(value) for value in (cluster, hook, route, volume, namespace)})
        self.assertIn(path_for(portal), api.values)
        self.assertTrue(all(set(options["preconditions"]) == {"uid", "resourceVersion"} for _path, options in api.attempts))
        self.assertEqual(len(api.deleted), 5)

    def test_foreign_cluster_root_or_external_namespace_blocks_all_destructive_steps(self):
        namespace = obj("Namespace", "labweaver-system")
        for kind in ("ClusterRole", "HTTPRoute"):
            planned = helm_object(kind, namespace="portal")
            current = copy.deepcopy(planned)
            current["metadata"]["annotations"]["meta.helm.sh/release-name"] = "foreign"
            api = KubernetesAPI([namespace, obj("Namespace", "portal", installation="foreign"), current, release_storage([planned])])
            with self.assertRaises(WIPE.WipeError):
                WIPE.wipe(api, INSTALLATION, [namespace], 1)
            self.assertEqual(api.attempts, [])

    def test_target_replacement_at_delete_is_rejected_atomically(self):
        namespace = obj("Namespace", "labweaver-system")
        for kind in ("Namespace", "ClusterRole", "PersistentVolume"):
            for field in ("uid", "resourceVersion"):
                with self.subTest(kind=kind, field=field):
                    target = namespace if kind == "Namespace" else helm_object(kind)
                    values = [namespace]
                    if kind == "PersistentVolume":
                        target["metadata"]["annotations"]["labweaver.io/installation-id"] = INSTALLATION
                        target["spec"] = {"claimRef": {"namespace": "labweaver-system"}}
                        values.append(target)
                    elif kind == "ClusterRole":
                        values.extend([target, release_storage([target])])
                    api = KubernetesAPI(values)
                    def replace(boundary, path):
                        boundary.values[path]["metadata"][field] = "replacement"
                    api.delete_hook = replace
                    with self.assertRaisesRegex(WIPE.WipeError, "PLATFORM_WIPE_DELETE_FAILED"):
                        WIPE.wipe(api, INSTALLATION, [namespace], 1)
                    self.assertEqual(api.deleted, [])
                    self.assertEqual(api.attempts[0][1]["preconditions"][field], target["metadata"][field])

    def test_protected_pv_waits_only_after_namespace_delete_is_submitted(self):
        namespace = obj('Namespace', 'labweaver-system')
        volume = obj('PersistentVolume', 'protected')
        volume['spec'] = {'claimRef': {'namespace': 'labweaver-system'}}
        api = KubernetesAPI([namespace, volume])
        api.defer_volumes = True
        result = WIPE.wipe(api, INSTALLATION, [namespace], 1)
        self.assertEqual([path for path, _options in api.attempts], [path_for(volume), path_for(namespace)])
        self.assertEqual(set(result['deleted']), {path_for(volume), path_for(namespace)})

    def test_namespace_replacement_during_revalidation_prevents_all_deletes(self):
        namespace = obj("Namespace", "labweaver-system")
        api = KubernetesAPI([namespace])
        def replace(boundary):
            if boundary.discovery_count == 2:
                boundary.values[path_for(namespace)]["metadata"]["uid"] = "replacement"
        api.discovery_hook = replace
        with self.assertRaisesRegex(WIPE.WipeError, "PLATFORM_WIPE_NAMESPACE_CHANGED"):
            WIPE.wipe(api, INSTALLATION, [namespace], 1)
        self.assertEqual(api.attempts, [])

    def test_helm_revision_or_storage_replacement_aborts_before_next_destructive_step(self):
        namespace, cluster = obj("Namespace", "labweaver-system"), helm_object("ClusterRole")
        storage = release_storage([cluster])
        for change in ("revision", "uid", "resourceVersion", "data"):
            with self.subTest(change=change):
                api = KubernetesAPI([namespace, cluster, storage])
                def mutate(boundary):
                    if boundary.discovery_count != 2:
                        return
                    if change == "revision":
                        new = release_storage([cluster], revision=2)
                        boundary.values[path_for(new)] = new
                    elif change == "data":
                        changed = release_storage([cluster], hooks=[cluster])
                        boundary.values[path_for(storage)]["data"] = changed["data"]
                    else:
                        boundary.values[path_for(storage)]["metadata"][change] = "replacement"
                api.discovery_hook = mutate
                with self.assertRaisesRegex(WIPE.WipeError, "PLATFORM_WIPE_RELEASE_REVISION_CHANGED"):
                    WIPE.wipe(api, INSTALLATION, [namespace], 1)
                self.assertEqual(api.attempts, [])

    def test_full_inventory_is_rechecked_before_every_destructive_step(self):
        namespace, cluster = obj("Namespace", "labweaver-system"), helm_object("ClusterRole")
        api = KubernetesAPI([namespace, cluster, release_storage([cluster])])
        def insert_foreign(boundary):
            if boundary.discovery_count == 3:
                value = obj("PreviouslyUnknown", "concurrent", installation="foreign")
                boundary.values[path_for(value)] = value
        api.discovery_hook = insert_foreign
        with self.assertRaisesRegex(WIPE.WipeError, "PLATFORM_WIPE_OBJECT_OWNERSHIP_CONFLICT"):
            WIPE.wipe(api, INSTALLATION, [namespace], 1)
        self.assertEqual(api.deleted, [path_for(cluster)])
        self.assertIn(path_for(namespace), api.values)

    def test_explicit_not_found_and_own_previous_deletions_are_idempotent_but_errors_are_not_absence(self):
        namespace = obj("Namespace", "labweaver-system")
        missing = helm_object("ClusterRole", "already-gone")
        api = KubernetesAPI([namespace, release_storage([missing])])
        result = WIPE.wipe(api, INSTALLATION, [namespace], 1)
        self.assertEqual(result["deleted"], [path_for(namespace)])
        repeated = WIPE.wipe(api, INSTALLATION, [namespace], 1)
        self.assertFalse(repeated["changed"])
        api = KubernetesAPI([namespace, release_storage([missing])])
        api.failure_path = path_for(missing)
        with self.assertRaisesRegex(WIPE.WipeError, "PLATFORM_WIPE_READ_FAILED"):
            WIPE.wipe(api, INSTALLATION, [namespace], 1)
        self.assertEqual(api.attempts, [])

    def test_keep_policy_has_no_foreign_object_exemption_and_retention_matches_scope(self):
        namespace, outside = obj("Namespace", "labweaver-system"), obj("Namespace", "outside")
        crd = helm_object("CustomResourceDefinition", "types.example")
        keep = helm_object("ClusterRole", "retained")
        keep["metadata"]["annotations"]["helm.sh/resource-policy"] = "keep"
        api = KubernetesAPI([namespace, outside, crd, keep, release_storage([outside, crd, keep, namespace])])
        result = WIPE.wipe(api, INSTALLATION, [namespace], 1)
        self.assertEqual(set(result["preserved"]), {path_for(value) for value in (outside, crd, keep)})
        self.assertEqual(result["deleted"], [path_for(namespace)])
        foreign = obj("PreviouslyUnknown", installation="foreign")
        foreign["metadata"]["annotations"]["helm.sh/resource-policy"] = "keep"
        api = KubernetesAPI([namespace, foreign])
        with self.assertRaisesRegex(WIPE.WipeError, "PLATFORM_WIPE_OBJECT_OWNERSHIP_CONFLICT"):
            WIPE.wipe(api, INSTALLATION, [namespace], 1)
        self.assertEqual(api.attempts, [])

    def test_builtin_default_identity_and_ca_are_bounded_not_name_only_exemptions(self):
        namespace = obj('Namespace', 'labweaver-system')
        default = obj('ServiceAccount', 'default', installation=None)
        cluster_ca = obj('ConfigMap', 'kube-root-ca.crt', 'kube-system', installation=None)
        cluster_ca['data'] = {'ca.crt': 'reviewed-cluster-public-ca'}
        local_ca = obj('ConfigMap', 'kube-root-ca.crt', installation=None)
        local_ca['data'] = copy.deepcopy(cluster_ca['data'])
        api = KubernetesAPI([namespace, default, cluster_ca, local_ca])
        self.assertEqual(WIPE.wipe(api, INSTALLATION, [namespace], 1)['deleted'], [path_for(namespace)])
        for kind in ('default-foreign', 'default-credentials', 'ca-foreign', 'ca-content', 'ca-read-error'):
            with self.subTest(kind=kind):
                account, bundle = copy.deepcopy(default), copy.deepcopy(local_ca)
                if kind == 'default-foreign':
                    account['metadata']['annotations']['labweaver.io/installation-id'] = 'foreign'
                elif kind == 'default-credentials':
                    account['imagePullSecrets'] = [{'name': 'foreign-credential'}]
                elif kind == 'ca-foreign':
                    bundle['metadata']['annotations']['labweaver.io/installation-id'] = 'foreign'
                elif kind == 'ca-content':
                    bundle['data']['ca.crt'] = 'another-cluster'
                api = KubernetesAPI([namespace, account, cluster_ca, bundle])
                if kind == 'ca-read-error':
                    api.failure_path = path_for(cluster_ca)
                with self.assertRaises(WIPE.WipeError):
                    WIPE.wipe(api, INSTALLATION, [namespace], 1)
                self.assertEqual(api.attempts, [])

    def test_check_mode_inspects_all_targets_without_deleting(self):
        namespace, cluster = obj("Namespace", "labweaver-system"), helm_object("ClusterRole")
        api = KubernetesAPI([namespace, cluster, release_storage([cluster])])
        result = WIPE.wipe(api, INSTALLATION, [namespace], 1, check=True)
        self.assertTrue(result["changed"])
        self.assertEqual(result["deleted"], [])
        self.assertEqual(api.attempts, [])

    def test_transport_uses_official_kubectl_and_preserves_api_error_semantics(self):
        with tempfile.TemporaryDirectory() as directory:
            command, capture = Path(directory) / "kubectl", Path(directory) / "request.json"
            command.write_text(f'#!{sys.executable}\nimport json, os, pathlib, sys\npathlib.Path({str(capture)!r}).write_text(json.dumps({{"argv": sys.argv[1:], "stdin": sys.stdin.read() if "delete" in sys.argv else ""}}))\nprint(os.environ["API_STDOUT"])\nprint(os.environ["API_STDERR"], file=sys.stderr)\nsys.exit(int(os.environ["API_RC"]))\n')
            command.chmod(0o755)
            import os
            from unittest.mock import patch
            client = WIPE.Kubernetes(str(command), "/reviewed/exec-auth-kubeconfig")
            for rc, stderr, accepted in ((1, "Error from server (NotFound): missing", True),
                                         (1, "Error from server (Forbidden): denied", False),
                                         (1, "connection refused", False), (1, "exec auth plugin not found", False)):
                with self.subTest(stderr=stderr), patch.dict(os.environ, {"API_RC": str(rc), "API_STDOUT": "", "API_STDERR": stderr}):
                    if accepted:
                        self.assertIsNone(client.request("/api/v1/namespaces/gone"))
                    else:
                        with self.assertRaises(WIPE.WipeError):
                            client.request("/api/v1/namespaces/gone")
            options = {"apiVersion": "v1", "kind": "DeleteOptions", "preconditions": {"uid": "uid", "resourceVersion": "17"}}
            with patch.dict(os.environ, {"API_RC": "0", "API_STDOUT": "{}", "API_STDERR": ""}):
                client.request("/api/v1/namespaces/reviewed", options)
            recorded = json.loads(capture.read_text())
            self.assertEqual(recorded["argv"], ["--kubeconfig", "/reviewed/exec-auth-kubeconfig", "delete", "--raw", "/api/v1/namespaces/reviewed", "-f", "-"])
            self.assertEqual(json.loads(recorded["stdin"]), options)

    @unittest.skipUnless(shutil.which('kubectl'), 'official kubectl required for raw transport integration')
    def test_official_kubectl_sends_uid_and_resourceversion_deleteoptions(self):
        requests = []
        class Handler(http.server.BaseHTTPRequestHandler):
            def do_DELETE(self):
                if self.headers.get('Content-Length') is not None:
                    body = self.rfile.read(int(self.headers['Content-Length']))
                else:
                    if self.headers.get('Transfer-Encoding') != 'chunked':
                        raise ValueError('expected HTTP request body')
                    body = bytearray()
                    while True:
                        length = int(self.rfile.readline().split(b';', 1)[0], 16)
                        if length == 0:
                            while self.rfile.readline() != b'\r\n':
                                pass
                            break
                        body.extend(self.rfile.read(length))
                        if self.rfile.read(2) != b'\r\n':
                            raise ValueError('invalid HTTP chunk terminator')
                requests.append((self.path, json.loads(body)))
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.end_headers()
                self.wfile.write(b'{"apiVersion":"v1","kind":"Status","status":"Success"}')

            def log_message(self, *_args):
                pass

        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as directory:
                kubeconfig = Path(directory) / 'kubeconfig'
                kubeconfig.write_text(yaml.safe_dump({
                    'apiVersion': 'v1', 'kind': 'Config', 'current-context': 'fixture',
                    'clusters': [{'name': 'fixture', 'cluster': {'server': f'http://127.0.0.1:{server.server_port}'}}],
                    'contexts': [{'name': 'fixture', 'context': {'cluster': 'fixture', 'user': 'fixture'}}],
                    'users': [{'name': 'fixture', 'user': {}}],
                }))
                client = WIPE.Kubernetes(shutil.which('kubectl'), str(kubeconfig))
                options = {'apiVersion': 'v1', 'kind': 'DeleteOptions',
                           'preconditions': {'uid': 'reviewed-uid', 'resourceVersion': '17'}}
                client.request('/api/v1/namespaces/reviewed', options)
                self.assertEqual(requests, [('/api/v1/namespaces/reviewed', options)])
        finally:
            server.shutdown()
            server.server_close()
            thread.join()

    def test_playbook_routes_every_destructive_operation_through_discovered_module(self):
        play = tasks("deploy/ansible/playbooks/94-platform-clean-redeploy.yml")[-1]
        invocations = [task["labweaver_owned_scope_wipe"] for task in play["tasks"] if "labweaver_owned_scope_wipe" in task]
        self.assertEqual(len(invocations), 1)
        self.assertEqual(invocations[0]["namespaces"], "{{ platform_clean_redeploy_owned_namespace_objects }}")
        for task in play["tasks"]:
            self.assertNotEqual(task.get("kubernetes.core.k8s", {}).get("state"), "absent")
            self.assertNotIn("uninstall", task.get("ansible.builtin.command", {}).get("argv", []))


if __name__ == "__main__":
    unittest.main()
