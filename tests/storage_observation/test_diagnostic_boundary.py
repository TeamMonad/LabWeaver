"""Counterexamples: consistent snapshots do not certify storage capability."""
import unittest
from test_storage_binding import CHECK, snapshots


class DiagnosticBoundaryTests(unittest.TestCase):
    def test_uninstalled_driver_can_still_be_internally_consistent(self):
        objects = snapshots()
        objects[1]["spec"]["csi"]["driver"] = "not-installed.csi.invalid"
        objects[2]["provisioner"] = "not-installed.csi.invalid"
        self.assertEqual(CHECK.inspect_binding(*objects), "BINDING_CONSISTENT_ONLY")

    def test_matching_rwx_is_not_a_filesystem_capability_check(self):
        objects = snapshots()
        for obj in objects[:2]:
            obj["spec"]["accessModes"] = ["ReadWriteMany"]
            obj["spec"]["volumeMode"] = "Filesystem"
        self.assertEqual(CHECK.inspect_binding(*objects), "BINDING_CONSISTENT_ONLY")

    def test_delete_reclaim_is_not_retention_authorization(self):
        objects = snapshots()
        objects[1]["spec"]["persistentVolumeReclaimPolicy"] = "Delete"
        objects[2]["reclaimPolicy"] = "Delete"
        self.assertEqual(CHECK.inspect_binding(*objects), "BINDING_CONSISTENT_ONLY")

    def test_resource_versions_do_not_prove_snapshot_freshness(self):
        objects = snapshots()
        for index, obj in enumerate(objects):
            obj["metadata"]["resourceVersion"] = str(index + 1)
            obj["metadata"]["creationTimestamp"] = "2020-01-01T00:00:00Z"
        self.assertEqual(CHECK.inspect_binding(*objects), "BINDING_CONSISTENT_ONLY")


if __name__ == "__main__":
    unittest.main()
