"""Synthetic ordering tests, not Kubernetes/Kueue/KubeVirt acceptance."""
import unittest
from dataclasses import FrozenInstanceError
from preemption_model import Blocked, Phase, Recall


class RecallTests(unittest.TestCase):
    def event(self, model, event, at, **extra):
        model.apply(event, at=at, generation=7, uid="old-object", **extra)

    def model(self, runtime="container"):
        return Recall(runtime, 7, "old-object")

    def stopping(self, runtime="container"):
        model = self.model(runtime)
        self.event(model, "notify", 10)
        self.event(model, "stop", 70)
        return model

    def test_all_runtimes_require_release_before_readmission(self):
        for runtime in ("container", "vm", "task"):
            with self.subTest(runtime=runtime):
                model = self.stopping(runtime)
                with self.assertRaisesRegex(Blocked, "invalid_order"):
                    self.event(model, "readmit", 71)
                self.event(model, "observe_release", 72, cleanup="confirmed", disk_isolated=True)
                self.event(model, "readmit", 73)
                self.assertEqual(model.phase, Phase.READMITTED)

    def test_grace_boundary_and_failed_event_does_not_advance_clock(self):
        model = self.model()
        self.event(model, "notify", 10)
        with self.assertRaisesRegex(Blocked, "save_grace_active"):
            self.event(model, "stop", 69)
        self.assertEqual((model.phase, model.last_at), (Phase.SAVING, 10))
        self.event(model, "stop", 70)

    def test_pending_unknown_and_missing_cleanup_cannot_release(self):
        for cleanup in (None, "pending", "unknown", "delete_requested"):
            with self.subTest(cleanup=cleanup):
                model = self.stopping()
                with self.assertRaisesRegex(Blocked, "release_unconfirmed"):
                    self.event(model, "observe_release", 71, cleanup=cleanup, disk_isolated=True)
                self.assertEqual(model.phase, Phase.STOPPING)

    def test_confirmed_cleanup_without_disk_fence_cannot_release_vm(self):
        model = self.stopping("vm")
        with self.assertRaisesRegex(Blocked, "release_unconfirmed"):
            self.event(model, "observe_release", 71, cleanup="confirmed")

    def test_recreated_object_and_stale_generation_cannot_confirm_release(self):
        for generation, uid in ((6, "old-object"), (7, "replacement")):
            with self.subTest(generation=generation, uid=uid):
                model = self.stopping()
                with self.assertRaisesRegex(Blocked, "stale_identity"):
                    model.apply("observe_release", at=71, generation=generation, uid=uid,
                                cleanup="confirmed", disk_isolated=True)
                self.assertEqual(model.phase, Phase.STOPPING)

    def test_release_without_stop_and_stop_without_notification_fail(self):
        for event in ("stop", "observe_release", "readmit"):
            with self.subTest(event=event):
                with self.assertRaisesRegex(Blocked, "invalid_order"):
                    self.event(self.model(), event, 10, cleanup="confirmed", disk_isolated=True)

    def test_reordered_events_and_duplicate_readmission_fail(self):
        model = self.stopping()
        with self.assertRaisesRegex(Blocked, "invalid_time"):
            self.event(model, "observe_release", 69, cleanup="confirmed", disk_isolated=True)
        self.event(model, "observe_release", 71, cleanup="confirmed", disk_isolated=True)
        self.event(model, "readmit", 72)
        with self.assertRaisesRegex(Blocked, "invalid_order"):
            self.event(model, "readmit", 72)

    def test_invalid_model_identity_and_runtime_fail(self):
        for runtime, generation, uid in (("fallback", 1, "x"), ("vm", 0, "x"), ("vm", 1, "")):
            with self.subTest(runtime=runtime, generation=generation):
                with self.assertRaises(Blocked):
                    Recall(runtime, generation, uid)


    def test_constructor_rejects_injected_progress(self):
        for key, value in (("phase", Phase.RELEASED), ("notified_at", -60), ("last_at", 60)):
            with self.subTest(key=key):
                with self.assertRaises(TypeError):
                    Recall("vm", 7, "old-object", **{key: value})
        with self.assertRaises(TypeError):
            Recall("vm", 7, "old-object", Phase.RELEASED)

    def test_state_and_identity_cannot_be_assigned_directly(self):
        model = self.model()
        for key, value in (("phase", Phase.RELEASED), ("notified_at", -60),
                           ("last_at", 60), ("generation", 8), ("uid", "replacement")):
            with self.subTest(key=key):
                with self.assertRaises(FrozenInstanceError):
                    setattr(model, key, value)
        self.assertEqual(model.phase, Phase.RUNNING)

    def test_constructor_requires_strict_identity_types(self):
        for generation, uid in ((7.0, "old-object"), (True, "old-object"),
                                (7, 42), (7, True), (7, ["old-object"]),
                                (7, "   "), (7, None)):
            with self.subTest(generation=generation, uid=uid):
                with self.assertRaisesRegex(Blocked, "invalid_identity"):
                    Recall("vm", generation, uid)

    def test_event_requires_strict_identity_types_without_advancing(self):
        for generation, uid in ((7.0, "old-object"), (True, "old-object"),
                                (7, 42), (7, True), (7, ["old-object"]),
                                (7, "   "), (7, None)):
            with self.subTest(generation=generation, uid=uid):
                model = self.stopping("vm")
                before = (model.phase, model.notified_at, model.last_at)
                with self.assertRaisesRegex(Blocked, "invalid_identity"):
                    model.apply("observe_release", at=71, generation=generation, uid=uid,
                                cleanup="confirmed", disk_isolated=True)
                self.assertEqual((model.phase, model.notified_at, model.last_at), before)


if __name__ == "__main__":
    unittest.main()
