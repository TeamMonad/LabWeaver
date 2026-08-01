#!/usr/bin/python3
"""Apply a fixed LabWeaver bootstrap SQL file through a bounded local tunnel.

The deployment controller cannot assume cluster DNS.  This module owns the only
temporary PostgreSQL port-forward needed by Resource bootstrap and always tears
it down before returning.  It never accepts SQL text, database credentials, or
an arbitrary command from Ansible variables.
"""

from __future__ import annotations

import os
import re
import socket
import subprocess
import time
from pathlib import Path

from ansible.module_utils.basic import AnsibleModule


SERVICE = re.compile(r"^[a-z0-9][a-z0-9-]{0,62}$")


def fail(module: AnsibleModule, code: str) -> None:
    module.fail_json(msg=code, diagnostic_code=code)


def regular_file(module: AnsibleModule, value: str, code: str) -> Path:
    path = Path(value)
    try:
        status = path.lstat()
    except OSError:
        fail(module, code)
    if not path.is_file() or path.is_symlink() or status.st_uid != 0 or status.st_mode & 0o077:
        fail(module, code)
    return path


def local_port_ready() -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as connection:
        connection.settimeout(0.2)
        return connection.connect_ex(("127.0.0.1", 15432)) == 0


def process_diagnostic(process: subprocess.Popen[str], fallback: str) -> str:
    output = ""
    if process.stderr is not None:
        output = process.stderr.read(4096)
    match = re.search(r"LW_[A-Z0-9_]+", output)
    return match.group(0) if match else fallback


def main() -> None:
    module = AnsibleModule(
        argument_spec={
            "kubeconfig": {"type": "path", "required": True},
            "psql": {"type": "path", "required": True},
            "service_file": {"type": "path", "required": True},
            "service": {"type": "str", "required": True},
            "sql_file": {"type": "path", "required": True},
        },
        supports_check_mode=False,
    )
    kubeconfig = regular_file(module, module.params["kubeconfig"], "RESOURCE_APPLICATION_KUBECONFIG_INVALID")
    service_file = regular_file(module, module.params["service_file"], "RESOURCE_APPLICATION_POSTGRES_SERVICE_INVALID")
    sql_file = regular_file(module, module.params["sql_file"], "RESOURCE_APPLICATION_SQL_INVALID")
    psql = Path(module.params["psql"])
    if not psql.is_file() or not os.access(psql, os.X_OK):
        fail(module, "RESOURCE_APPLICATION_PSQL_INVALID")
    service = module.params["service"]
    if not SERVICE.fullmatch(service):
        fail(module, "RESOURCE_APPLICATION_POSTGRES_SERVICE_INVALID")
    if local_port_ready():
        fail(module, "RESOURCE_APPLICATION_POSTGRES_TUNNEL_CONFLICT")

    tunnel = subprocess.Popen(
        [
            "/usr/bin/kubectl", "--kubeconfig", str(kubeconfig), "--namespace", "labweaver-data",
            "port-forward", "service/postgres", "15432:5432",
        ],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if local_port_ready():
                break
            if tunnel.poll() is not None:
                fail(module, process_diagnostic(tunnel, "RESOURCE_APPLICATION_POSTGRES_TUNNEL_UNAVAILABLE"))
            time.sleep(0.2)
        else:
            fail(module, "RESOURCE_APPLICATION_POSTGRES_TUNNEL_UNAVAILABLE")

        result = subprocess.run(
            [str(psql), f"service={service}", "--no-psqlrc", "--file", str(sql_file)],
            env={"PGSERVICEFILE": str(service_file), "PATH": "/usr/local/bin:/usr/bin:/bin"},
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
            timeout=45,
            check=False,
        )
        if result.returncode != 0:
            match = re.search(r"LW_[A-Z0-9_]+", result.stderr)
            fail(module, match.group(0) if match else "RESOURCE_APPLICATION_ACCESS_SEED_APPLY_FAILED")
    except subprocess.TimeoutExpired:
        fail(module, "RESOURCE_APPLICATION_ACCESS_SEED_TIMEOUT")
    finally:
        if tunnel.poll() is None:
            tunnel.terminate()
            try:
                tunnel.wait(timeout=5)
            except subprocess.TimeoutExpired:
                tunnel.kill()
                tunnel.wait(timeout=5)
    module.exit_json(changed=True, diagnostic_code="RESOURCE_APPLICATION_ACCESS_SEED_APPLIED")


if __name__ == "__main__":
    main()
