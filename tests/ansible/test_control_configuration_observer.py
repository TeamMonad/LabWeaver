"""Reject partial, stale or replaced Kubernetes/HTTPS observations in isolated fixtures."""

import copy
import datetime as dt
from pathlib import Path
import sys
import http.server
import json
import ssl
import subprocess
import tempfile
import threading
import unittest
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "tools"))
import observe_control_configuration as observer


class ControlObserverTests(unittest.TestCase):
    def setUp(self):
        self.time = dt.datetime(2026, 10, 12, 18, 0, tzinfo=dt.timezone.utc)
        self.source = {
            "desiredRevision": 7,
            "configurationSha256": "a" * 64,
            "encryptedBundleSha256": "b" * 64,
            "expectedControlReplicas": 2,
            "installation": {
                "namespace": "synthetic-control",
                "releaseName": "synthetic",
            },
        }
        self.deployment_uid = str(uuid.uuid4())
        self.rs_uid = str(uuid.uuid4())
        self.deployment = {
            "metadata": {
                "name": "control-service",
                "namespace": "synthetic-control",
                "uid": self.deployment_uid,
                "generation": 2,
            },
            "spec": {
                "replicas": 2,
                "selector": {"matchLabels": {"app": "control-service"}},
            },
            "status": {"observedGeneration": 2},
        }
        self.replica_sets = {
            "items": [
                {
                    "metadata": {
                        "namespace": "synthetic-control",
                        "uid": self.rs_uid,
                        "ownerReferences": [
                            {
                                "controller": True,
                                "kind": "Deployment",
                                "uid": self.deployment_uid,
                            }
                        ],
                    }
                }
            ]
        }
        self.pods = {"items": []}
        self.observations = {}
        for index in range(2):
            uid = str(uuid.uuid4())
            name = f"control-{index}"
            self.pods["items"].append(
                {
                    "metadata": {
                        "name": name,
                        "namespace": "synthetic-control",
                        "uid": uid,
                        "labels": {"app": "control-service"},
                        "ownerReferences": [
                            {
                                "controller": True,
                                "kind": "ReplicaSet",
                                "uid": self.rs_uid,
                            }
                        ],
                    },
                    "status": {
                        "phase": "Running",
                        "podIP": f"192.0.2.{index+10}",
                        "conditions": [{"type": "Ready", "status": "True"}],
                        "containerStatuses": [
                            {"containerID": f"synthetic:{index}", "restartCount": 0}
                        ],
                    },
                }
            )
            self.observations[uid] = {
                "podName": name,
                "podUid": uid,
                "namespace": "synthetic-control",
                "instanceId": str(uuid.uuid4()),
                "desiredRevision": 7,
                "configurationSha256": "a" * 64,
                "encryptedBundleSha256": "b" * 64,
                "observedAt": self.time.isoformat(),
            }

    def kube(self):
        return copy.deepcopy((self.deployment, self.replica_sets, self.pods))

    def peer(self, pod):
        return copy.deepcopy(self.observations[pod["metadata"]["uid"]])

    def collect(self):
        return observer.collect(self.source, self.kube, self.peer, lambda: self.time)

    def test_complete_live_readbacks_are_bound_to_exact_roster_and_source(self):
        result = self.collect()
        self.assertEqual(result["deploymentUid"], self.deployment_uid)
        self.assertEqual(len(result["replicas"]), 2)
        self.assertEqual(
            {r["podUid"] for r in result["replicas"]}, set(self.observations)
        )
        self.assertNotIn("sourceJson", result)

    def test_ready_or_command_success_cannot_replace_loaded_peer_evidence(self):
        original = copy.deepcopy(self.observations)
        uid = next(iter(self.observations))
        for field, value in [
            ("desiredRevision", 6),
            ("configurationSha256", "c" * 64),
            ("encryptedBundleSha256", "d" * 64),
            ("namespace", "another"),
            ("instanceId", "not-a-uid"),
            ("observedAt", (self.time - dt.timedelta(seconds=31)).isoformat()),
            ("observedAt", (self.time + dt.timedelta(seconds=1)).isoformat()),
        ]:
            with self.subTest(field=field, value=value):
                self.observations = copy.deepcopy(original)
                self.observations[uid][field] = value
                with self.assertRaises(observer.ObservationError):
                    self.collect()

    def test_missing_extra_terminating_foreign_or_unready_pod_blocks_success(self):
        original = copy.deepcopy(self.pods)
        for change in (
            "missing",
            "surge",
            "terminating",
            "unready",
            "foreign",
            "duplicate",
        ):
            with self.subTest(change=change):
                self.pods = copy.deepcopy(original)
                if change == "missing":
                    self.pods["items"].pop()
                elif change == "surge":
                    extra = copy.deepcopy(self.pods["items"][0])
                    extra["metadata"]["uid"] = str(uuid.uuid4())
                    self.pods["items"].append(extra)
                elif change == "terminating":
                    self.pods["items"][0]["metadata"][
                        "deletionTimestamp"
                    ] = self.time.isoformat()
                elif change == "unready":
                    self.pods["items"][0]["status"]["conditions"][0]["status"] = "False"
                elif change == "foreign":
                    self.pods["items"][0]["metadata"]["ownerReferences"][0]["uid"] = (
                        str(uuid.uuid4())
                    )
                else:
                    self.pods["items"][1]["metadata"]["uid"] = self.pods["items"][0][
                        "metadata"
                    ]["uid"]
                with self.assertRaises(observer.ObservationError):
                    self.collect()

    def test_roster_generation_and_process_restart_during_read_are_not_ignored(self):
        for change in ("generation", "pod", "restart"):
            with self.subTest(change=change):
                first = self.kube()
                final = copy.deepcopy(first)
                if change == "generation":
                    final[0]["metadata"]["generation"] += 1
                    final[0]["status"]["observedGeneration"] += 1
                elif change == "pod":
                    final[2]["items"][0]["metadata"]["uid"] = str(uuid.uuid4())
                else:
                    final[2]["items"][0]["status"]["containerStatuses"][0][
                        "restartCount"
                    ] += 1
                data = iter((first, final))
                with self.assertRaises(observer.ObservationError):
                    observer.collect(
                        self.source, lambda: next(data), self.peer, lambda: self.time
                    )

    def test_https_peer_reader_preserves_service_identity_and_does_not_follow_redirects(
        self,
    ):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            certificate = directory / "synthetic.crt"
            private_key = directory / "synthetic.key"
            hostname = "control-service.synthetic-control.svc"
            generated = subprocess.run(
                [
                    "openssl",
                    "req",
                    "-x509",
                    "-newkey",
                    "rsa:2048",
                    "-nodes",
                    "-days",
                    "1",
                    "-subj",
                    f"/CN={hostname}",
                    "-addext",
                    f"subjectAltName=DNS:{hostname}",
                    "-keyout",
                    str(private_key),
                    "-out",
                    str(certificate),
                ],
                capture_output=True,
            )
            self.assertEqual(generated.returncode, 0, "synthetic TLS generation failed")
            received = []
            response = {"redirect": False}

            class Handler(http.server.BaseHTTPRequestHandler):
                def do_GET(self):
                    received.append((self.path, self.headers.get("Authorization")))
                    if response["redirect"]:
                        self.send_response(302)
                        self.send_header("Location", "https://elsewhere.invalid/steal")
                        self.end_headers()
                        return
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.write(b'{"synthetic":true}')

                def log_message(self, *_arguments):
                    pass

            server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.load_cert_chain(certificate, private_key)
            server.socket = context.wrap_socket(server.socket, server_side=True)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            pod = {"status": {"podIP": "127.0.0.1"}}
            try:
                reader = observer.peer_reader(
                    "synthetic-private-token", certificate, hostname, server.server_port
                )
                self.assertEqual(reader(pod), {"synthetic": True})
                self.assertEqual(
                    received,
                    [
                        (
                            "/internal/v1/platform-configuration/loaded",
                            "Bearer synthetic-private-token",
                        )
                    ],
                )
                wrong = observer.peer_reader(
                    "synthetic-private-token",
                    certificate,
                    "another.invalid",
                    server.server_port,
                )
                with self.assertRaises(ssl.SSLCertVerificationError):
                    wrong(pod)
                response["redirect"] = True
                with self.assertRaises(observer.ObservationError):
                    reader(pod)
                self.assertEqual(len(received), 2)
            finally:
                server.shutdown()
                server.server_close()
                thread.join(timeout=5)


if __name__ == "__main__":
    unittest.main()
