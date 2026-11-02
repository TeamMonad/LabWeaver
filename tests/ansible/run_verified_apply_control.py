#!/usr/bin/env python3
"""Drive the production executor state machine against a real PG-backed Control.

Only Kubernetes roster transport is replaced; all Control reads, observations,
and stage reports go over its real TLS router and signed service JWT verifier.
"""
import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "tools"))
import verified_apply as executor
import observe_control_configuration as observer


def main():
    settings = json.loads(Path(sys.argv[1]).read_text())
    source = settings["source"]
    endpoint = settings["endpoint"]
    token = settings["token"]
    ca = Path(settings["caFile"]).read_bytes()

    class LiveControl:
        def __init__(self):
            self.source = source
            self.apply_id = settings["applyId"]

        def request(self, method, path, payload=None, revision=None, key=None):
            headers = {"Authorization": f"Bearer {token}", "Content-Type": "application/json"}
            if revision is not None:
                headers["If-Match"] = f'"rev-{revision}"'
            if key:
                headers["Idempotency-Key"] = key
            return executor.https_json(endpoint.rstrip("/") + path, ca, method=method,
                                       payload=None if payload is None else json.dumps(payload), headers=headers)

        def observe(self):
            def read_peer(_pod):
                if settings["case"] == "unreachable":
                    raise OSError("fixture Kubernetes transport has no route")
                actual = self.request("GET", executor.PREFIX + "/loaded")
                return actual

            expected = dict(source)
            if settings["case"] == "mismatch":
                # Real Control is loaded with a different source than the
                # roster observer expects, rather than returning fake success.
                expected["configurationSha256"] = "0" * 64
            return observer.collect(expected, lambda: settings["roster"], read_peer)

    control = LiveControl()
    if settings["case"] == "success":
        # The executor-only view carries the bound attempt and its immutable digest only.
        intent = control.request("GET", f"{executor.PREFIX}/applies/{control.apply_id}")
        assert set(intent) == {"revision", "attempt", "configurationSha256"}, sorted(intent)
        assert set(intent["attempt"]) == {"id", "revision", "desiredRevision", "encryptedBundleSha256",
                                          "stage", "executorClientId"}, sorted(intent["attempt"])
        mismatched = type(control)()
        mismatched.source = dict(source, configurationSha256="f" * 64)
        try:
            executor.read_intent(mismatched)
        except executor.ApplyError as error:
            assert "intent-binding" in str(error)
        else:
            raise AssertionError("executor accepted a different immutable desired settings digest")
    executor.prepare(control)
    try:
        result = executor.finish(control)
    except executor.ApplyError:
        if settings["case"] == "success":
            raise
        print("VERIFIED_APPLY_FAIL_CLOSED")
        return
    if settings["case"] != "success":
        raise AssertionError("executor accepted mismatched or unreachable replica")
    assert result["attempt"]["stage"] == "succeeded"
    assert set(result) == {"revision", "attempt", "configurationSha256"}, sorted(result)
    # Re-entry after success only observes; it reports nothing and never reaches deployment.
    assert executor.already_verified(control)
    assert executor.finish(control)["attempt"]["revision"] == result["attempt"]["revision"]
    try:
        executor.prepare(control)
    except executor.ApplyError as error:
        assert "LW_PLATFORM_CONFIGURATION_APPLY_CONFLICT:prepare" in str(error)
    else:
        raise AssertionError("a succeeded attempt was accepted for another deployment")
    print("VERIFIED_APPLY_CONTROL_OK")


if __name__ == "__main__":
    main()
