#!/usr/bin/env python3
"""Run one xv6 evaluator input after the real shell is ready.

The interaction pattern follows xv6's ``test-xv6.py`` driver: QEMU's console
is consumed incrementally and commands are written only after the shell prompt
has arrived.  A private shell sentinel lets the runner stop as soon as the
complete input has been processed instead of guessing with a startup sleep.
Each invocation gives QEMU a private copy of the immutable filesystem image in
the evaluator working directory, so QEMU does not need its own `/var/tmp`
snapshot file.
"""

from __future__ import annotations

import os
import re
import selectors
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path


READY_MARKER = b"init: starting sh"
READY_PROMPT = b"$ "
DONE_MARKER = b"__LABWEAVER_XV6_DONE__"
MAX_OUTPUT_BYTES = 1_048_576
RUN_DEADLINE_SECONDS = 5.0
TERMINATE_GRACE_SECONDS = 1.0


def fail(message: str, raw_output: bytes = b"", exit_code: int = 67) -> int:
    if raw_output:
        sys.stderr.buffer.write(raw_output)
        if not raw_output.endswith(b"\n"):
            sys.stderr.write("\n")
    sys.stderr.write(f"run-xv6: {message}\n")
    return exit_code


def drain(process: subprocess.Popen[bytes], output: bytearray) -> bool:
    stream = process.stdout
    if stream is None:
        return False
    while True:
        try:
            chunk = os.read(stream.fileno(), 16 * 1024)
        except BlockingIOError:
            return True
        if not chunk:
            return False
        output.extend(chunk)
        if len(output) > MAX_OUTPUT_BYTES:
            raise RuntimeError("QEMU output exceeded the evaluator limit")


def stop(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=TERMINATE_GRACE_SECONDS)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=TERMINATE_GRACE_SECONDS)


def result_lines(raw_output: bytes) -> list[str]:
    lines: list[str] = []
    text = raw_output.decode("utf-8", "replace").replace("\r\n", "\n").replace("\r", "\n")
    for line in text.splitlines():
        student = line.find("xv6-student:")
        if student >= 0:
            lines.append(line[student:].rstrip())
            continue
        if re.search(r"filesystem-ready[ \t]*$", line):
            lines.append("filesystem-ready")
    return lines


def done_output_seen(raw_output: bytes) -> bool:
    """Recognize the sentinel's command output, not the shell's input echo."""

    text = raw_output.decode("utf-8", "replace").replace("\r\n", "\n").replace("\r", "\n")
    for line in text.splitlines():
        candidate = line.lstrip(" \t")
        if candidate.startswith("$ "):
            candidate = candidate[2:]
        if candidate.strip() == DONE_MARKER.decode("ascii"):
            return True
    return False


def validate_paths(arguments: list[str]) -> tuple[Path, Path]:
    if len(arguments) != 3:
        raise ValueError("expected binary, submission directory, evaluator directory")
    binary_path = Path(arguments[0])
    if not binary_path.is_absolute() or not binary_path.as_posix().startswith("/work/build/"):
        raise ValueError("binary is outside the build directory")
    build_dir = binary_path.parent
    xv6_dir = build_dir / "xv6"
    if not binary_path.is_file() or not (xv6_dir / "kernel/kernel").is_file() or not (xv6_dir / "fs.img").is_file():
        raise ValueError("compile output is incomplete")
    return binary_path, xv6_dir


def run(arguments: list[str]) -> int:
    try:
        binary_path, xv6_dir = validate_paths(arguments)
    except ValueError as error:
        return fail(str(error), exit_code=64 if "expected" in str(error) else 65)

    raw_output_path = Path.cwd() / "xv6-console.txt"
    # QEMU's -snapshot mode creates a temporary disk below /var/tmp. Keep the
    # private copy beside the evaluator case instead, where the runner already
    # has its approved writable working directory.
    scratch_directory = tempfile.TemporaryDirectory(prefix=".xv6-", dir=Path.cwd())
    scratch_disk = Path(scratch_directory.name) / "fs.img"
    try:
        shutil.copyfile(xv6_dir / "fs.img", scratch_disk)
    except OSError as error:
        scratch_directory.cleanup()
        return fail(f"could not prepare private filesystem image: {error}")

    command = [
        "/usr/bin/qemu-system-riscv64",
        "-machine",
        "virt",
        "-bios",
        "none",
        "-kernel",
        str(xv6_dir / "kernel/kernel"),
        "-m",
        "128M",
        "-smp",
        "1",
        "-nographic",
        "-monitor",
        "none",
        "-global",
        "virtio-mmio.force-legacy=false",
        "-drive",
        f"file={scratch_disk},if=none,format=raw,id=x0",
        "-device",
        "virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0",
    ]
    input_data = sys.stdin.buffer.read()
    if not input_data.endswith(b"\n"):
        input_data += b"\n"
    input_data += b"echo " + DONE_MARKER + b"\n"

    output = bytearray()
    sent = False
    completed = False
    early_exit_code: int | None = None
    process: subprocess.Popen[bytes] | None = None
    selector = selectors.DefaultSelector()
    try:
        process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            close_fds=True,
        )
        if process.stdout is None or process.stdin is None:
            return fail("QEMU pipes are unavailable")
        os.set_blocking(process.stdout.fileno(), False)
        selector.register(process.stdout, selectors.EVENT_READ)
        deadline = time.monotonic() + RUN_DEADLINE_SECONDS
        while time.monotonic() < deadline:
            if process.poll() is not None:
                drain(process, output)
                early_exit_code = process.returncode
                break
            events = selector.select(max(0.0, min(0.2, deadline - time.monotonic())))
            if not events:
                continue
            for key, _ in events:
                if not drain(process, output):
                    selector.unregister(key.fileobj)
                    break
                if not sent and READY_MARKER in output and READY_PROMPT in output:
                    process.stdin.write(input_data)
                    process.stdin.flush()
                    sent = True
                if sent and done_output_seen(bytes(output)):
                    completed = True
                    break
            if completed:
                break
    except (OSError, RuntimeError, selectors.error) as error:
        return fail(str(error), bytes(output))
    finally:
        selector.close()
        if process is not None:
            if completed:
                stop(process)
            elif process.poll() is None:
                stop(process)
        scratch_directory.cleanup()

    raw_bytes = bytes(output)
    raw_output_path.write_bytes(raw_bytes)
    if not completed:
        if early_exit_code is not None:
            return fail(
                f"QEMU exited with status {early_exit_code} before evaluator input completed",
                raw_bytes,
            )
        return fail("shell did not process the evaluator input before the deadline", raw_bytes, 68)
    if process is not None and process.returncode not in (0, -15, -9):
        return fail(f"QEMU exited with status {process.returncode}", raw_bytes)

    lines = result_lines(raw_bytes)
    if not any(line.startswith("xv6-student:") for line in lines):
        return fail("xv6 did not produce a student result", raw_bytes)
    sys.stdout.write("\n".join(lines))
    if lines:
        sys.stdout.write("\n")
    raw_output_path.unlink(missing_ok=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(run(sys.argv[1:]))
