#!/usr/bin/env python3
"""Run the pinned Linux controller image without SSH or WSL.

The default command is read-only local preflight.  The script never forwards
the host environment wholesale; the ECNU key is mounted as an explicit private
env file and is not copied into the repository or report output.
"""

from __future__ import annotations

import argparse
import subprocess
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--image", default="labweaver/controller:local")
    parser.add_argument("--kubeconfig", type=Path, required=True)
    parser.add_argument("--env-file", type=Path, default=Path(".env"))
    parser.add_argument("--profile", default="local-hostpath")
    parser.add_argument("--build", action="store_true")
    args = parser.parse_args()

    root = Path.cwd().resolve()
    kubeconfig = args.kubeconfig.resolve()
    env_file = args.env_file.resolve()
    if not kubeconfig.is_file() or not env_file.is_file():
        raise SystemExit("LW_LOCAL_CONTROLLER_PRIVATE_INPUT_MISSING")
    if args.build:
        subprocess.run(
            [
                "docker",
                "build",
                "--file",
                "containers/Containerfile.controller",
                "--tag",
                args.image,
                "--load",
                ".",
            ],
            cwd=root,
            check=True,
        )

    artifacts = root / "artifacts"
    artifacts.mkdir(exist_ok=True)
    target = root / "target"
    target.mkdir(exist_ok=True)
    command = [
        "docker",
        "run",
        "--rm",
        "--read-only",
        "--tmpfs",
        "/tmp:rw,noexec,nosuid,size=512m",
        "--mount",
        f"type=bind,src={root},dst=/workspace,readonly",
        "--mount",
        f"type=bind,src={artifacts},dst=/workspace/artifacts",
        "--mount",
        f"type=bind,src={target},dst=/workspace/target",
        "--mount",
        f"type=bind,src={kubeconfig},dst=/run/secrets/kubeconfig,readonly",
        "--mount",
        f"type=bind,src={env_file},dst=/run/secrets/ecnu.env,readonly",
        "--env",
        "KUBECONFIG=/run/secrets/kubeconfig",
        "--env",
        "LABWEAVER_LOCAL_ECNU_ENV_FILE=/run/secrets/ecnu.env",
        "--env",
        "CARGO_HOME=/tmp/cargo",
        "--env",
        "RUSTUP_HOME=/usr/local/rustup",
        "--env",
        "CARGO_TARGET_DIR=/workspace/target/controller",
        "--env",
        "CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse",
        "--env",
        "ANSIBLE_HOME=/tmp/ansible",
        "--env",
        "ANSIBLE_LOCAL_TEMP=/tmp/ansible/tmp",
        "--env",
        "ANSIBLE_REMOTE_TEMP=/tmp/ansible/tmp",
        "--workdir",
        "/workspace",
        args.image,
        "local",
        "preflight",
        "--profile",
        args.profile,
    ]
    return subprocess.run(command, cwd=root, check=False).returncode


if __name__ == "__main__":
    raise SystemExit(main())
