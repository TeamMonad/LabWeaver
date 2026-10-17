"""Synthetic API snapshots; these tests do not contact a cluster."""
import copy
import importlib.util
import io
import json
from contextlib import redirect_stdout
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    "check_storage_binding", Path(__file__).resolve().parents[2] / "tools/check_storage_binding.py")
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


def snapshots():
    pvc = {"apiVersion": "v1", "kind": "PersistentVolumeClaim",
           "metadata": {"name": "work", "namespace": "project", "uid": "claim-uid", "resourceVersion": "11"},
           "spec": {"volumeName": "volume", "storageClassName": "existing-class", "accessModes": ["ReadWriteOnce"]},
           "status": {"phase": "Bound"}}
    pv = {"apiVersion": "v1", "kind": "PersistentVolume",
          "metadata": {"name": "volume", "uid": "volume-uid", "resourceVersion": "22"},
          "spec": {"claimRef": {"name": "work", "namespace": "project", "uid": "claim-uid"},
                   "storageClassName": "existing-class", "accessModes": ["ReadWriteOnce"],
                   "csi": {"driver": "example.csi.invalid", "volumeHandle": "volume-handle"}},
          "status": {"phase": "Bound"}}
    sc = {"apiVersion": "storage.k8s.io/v1", "kind": "StorageClass",
          "metadata": {"name": "existing-class", "uid": "class-uid", "resourceVersion": "33"},
          "provisioner": "example.csi.invalid"}
    return pvc, pv, sc


class BindingTests(unittest.TestCase):
    def reject(self, objects, code):
        with self.assertRaisesRegex(CHECK.ObservationError, "^" + code + "$"):
            CHECK.inspect_binding(*objects)

    def test_consistency_does_not_mutate_input_or_claim_ha(self):
        objects = snapshots()
        before = copy.deepcopy(objects)
        self.assertEqual(CHECK.inspect_binding(*objects), "BINDING_CONSISTENT_ONLY")
        self.assertEqual(objects, before)

    def test_recreated_claim_and_wrong_scope_fail(self):
        for key in ("uid", "name", "namespace"):
            with self.subTest(key=key):
                objects = snapshots()
                objects[1]["spec"]["claimRef"][key] = "other"
                self.reject(objects, "CLAIM_REFERENCE_MISMATCH")

    def test_unbound_or_unknown_phase_fails(self):
        for index in (0, 1):
            for phase in (None, "Pending", "Released", "Lost"):
                with self.subTest(index=index, phase=phase):
                    objects = snapshots()
                    objects[index]["status"]["phase"] = phase
                    self.reject(objects, "BINDING_NOT_BOUND")

    def test_wrong_volume_class_or_driver_fails(self):
        for index, section, key, code in (
            (0, "spec", "volumeName", "VOLUME_NAME_MISMATCH"),
            (1, "spec", "storageClassName", "STORAGE_CLASS_MISMATCH"),
            (2, "metadata", "name", "STORAGE_CLASS_MISMATCH"),
        ):
            with self.subTest(code=code, index=index):
                objects = snapshots()
                objects[index][section][key] = "other"
                self.reject(objects, code)
        objects = snapshots()
        objects[2]["provisioner"] = "other"
        self.reject(objects, "CSI_DRIVER_MISMATCH")

    def test_missing_identity_deleting_and_wrong_kind_fail(self):
        for index in range(3):
            for key in ("uid", "resourceVersion"):
                with self.subTest(index=index, key=key):
                    objects = snapshots()
                    del objects[index]["metadata"][key]
                    self.reject(objects, "OBJECT_IDENTITY_MISSING")
            objects = snapshots()
            objects[index]["metadata"]["deletionTimestamp"] = "2026-10-17T00:00:00Z"
            self.reject(objects, "OBJECT_DELETING")
            objects = snapshots()
            objects[index]["kind"] = "Secret"
            self.reject(objects, "OBJECT_KIND_INVALID")

    def test_missing_csi_handle_fails(self):
        objects = snapshots()
        del objects[1]["spec"]["csi"]["volumeHandle"]
        self.reject(objects, "CSI_IDENTITY_MISSING")

    def test_access_and_volume_modes_are_checked_without_choosing_policy(self):
        objects = snapshots()
        objects[0]["spec"]["accessModes"] = ["ReadWriteMany"]
        self.reject(objects, "ACCESS_MODES_MISMATCH")
        objects = snapshots()
        objects[0]["spec"]["accessModes"] = ["ReadWriteOnce", "ReadWriteOnce"]
        self.reject(objects, "ACCESS_MODES_INVALID")
        objects = snapshots()
        objects[0]["spec"]["volumeMode"] = "Block"
        self.reject(objects, "VOLUME_MODE_MISMATCH")
        objects[1]["spec"]["volumeMode"] = "Block"
        self.assertEqual(CHECK.inspect_binding(*objects), "BINDING_CONSISTENT_ONLY")

    def test_missing_and_malformed_objects_fail(self):
        for value in (None, [], "unknown"):
            self.reject((value, *snapshots()[1:]), "OBJECT_INVALID")
        objects = snapshots()
        objects[1]["spec"]["claimRef"] = None
        self.reject(objects, "CLAIM_REFERENCE_MISSING")

    def test_cli_reads_without_writing_and_rejects_duplicate_keys(self):
        with tempfile.TemporaryDirectory() as temporary:
            paths = [Path(temporary) / str(index) for index in range(3)]
            for path, obj in zip(paths, snapshots()):
                path.write_text(json.dumps(obj), encoding="utf-8")
            before = [path.read_bytes() for path in paths]
            output = io.StringIO()
            with redirect_stdout(output):
                self.assertEqual(CHECK.main([str(path) for path in paths]), 0)
            self.assertEqual(output.getvalue(), "BINDING_CONSISTENT_ONLY\n")
            self.assertEqual([path.read_bytes() for path in paths], before)
            paths[0].write_text('{"kind":"first","kind":"second"}', encoding="utf-8")
            output = io.StringIO()
            with redirect_stdout(output):
                self.assertEqual(CHECK.main([str(path) for path in paths]), 1)
            self.assertEqual(output.getvalue(), "JSON_DUPLICATE_KEY\n")

    def test_cli_missing_file_does_not_echo_path(self):
        output = io.StringIO()
        with redirect_stdout(output):
            self.assertEqual(CHECK.main(["/missing/private-input"] * 3), 1)
        self.assertEqual(output.getvalue(), "OBSERVATION_INPUT_UNREADABLE\n")


if __name__ == "__main__":
    unittest.main()
