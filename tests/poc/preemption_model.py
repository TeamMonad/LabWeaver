"""Test-only recall model for #211; no Kubernetes client or production imports.

This explores Sprint5 ordering constraints. It does not implement Kueue admission,
verify a KubeVirt integration, authorize resources, or persist business state.
"""
from dataclasses import dataclass, field
from enum import Enum


class Phase(Enum):
    RUNNING = "running"
    SAVING = "saving"
    STOPPING = "stopping"
    RELEASED = "released"
    READMITTED = "readmitted"


class Blocked(ValueError):
    """A simulated event violates the recall ordering contract."""


@dataclass(frozen=True)
class Recall:
    runtime: str
    generation: int
    uid: str
    phase: Phase = field(default=Phase.RUNNING, init=False)
    notified_at: int | None = field(default=None, init=False)
    last_at: int = field(default=0, init=False)

    def __post_init__(self):
        if type(self.runtime) is not str or self.runtime not in {"container", "vm", "task"}:
            raise Blocked("unsupported_runtime")
        self._validate_identity(self.generation, self.uid)

    @staticmethod
    def _validate_identity(generation, uid):
        if (type(generation) is not int or generation < 1
                or type(uid) is not str or not uid.strip()):
            raise Blocked("invalid_identity")

    def apply(self, event, *, at, generation, uid, cleanup=None, disk_isolated=False):
        """Apply synthetic observations, fenced to the exact old object.

        For task, disk isolation represents cleanup of all execution resources.
        Observations are supplied by tests, never inferred from a delete request.
        """
        if type(at) is not int or at < self.last_at:
            raise Blocked("invalid_time")
        self._validate_identity(generation, uid)
        if generation != self.generation or uid != self.uid:
            raise Blocked("stale_identity")
        if event == "notify" and self.phase == Phase.RUNNING:
            next_phase = Phase.SAVING
        elif event == "stop" and self.phase == Phase.SAVING:
            if at - self.notified_at < 60:
                raise Blocked("save_grace_active")
            next_phase = Phase.STOPPING
        elif event == "observe_release" and self.phase == Phase.STOPPING:
            if cleanup != "confirmed" or disk_isolated is not True:
                raise Blocked("release_unconfirmed")
            next_phase = Phase.RELEASED
        elif event == "readmit" and self.phase == Phase.RELEASED:
            next_phase = Phase.READMITTED
        else:
            raise Blocked("invalid_order")
        if event == "notify":
            object.__setattr__(self, "notified_at", at)
        object.__setattr__(self, "phase", next_phase)
        object.__setattr__(self, "last_at", at)
