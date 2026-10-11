# xv6 RISC-V Lab Material Contract

## Status and boundary

This document describes the teaching material in
[`examples/xv6-lab`](../../examples/xv6-lab). The package is a container-only
RISC-V systems experiment. It runs through the ordinary `container` runtime and
the direct `program` runner with the `xv6-riscv64` toolchain profile.

The package carries two build recipes. The root `Dockerfile` is the student
environment image and contains no evaluator fixtures. `evaluation/Dockerfile`
is the mandatory per-experiment Evaluation runner image: it installs the RISC-V
cross toolchain, host `gcc` for the on-host `mkfs` target, QEMU, and Python,
then obtains the platform worker with
`COPY --from=${LABWEAVER_SERVICE_IMAGE} /usr/local/bin/labweaver-service`.

## Runner-only fixtures

The concrete evaluator inputs under `tests/xv6-riscv64/` are runner-only. They
are baked into the Evaluation runner image at `/opt/labweaver/hidden-tests` and
materialized from the frozen package at execution time, but they are never
copied into the student environment image. The student source, the direct-argv
scripts, and the vendored xv6 tree remain public teaching material.

## Execution budget

The OJ runner derives `compile_wall`, `run_wall`, and `cpu` from the step's
`wallTimeSeconds` and enforces `run_wall <= 30s`, `cpu <= 30s`, and
`compile_wall <= 120s`. Because the runner recompiles the program at the start
of the test phase, both the `compile` and `tests` steps must fit the full xv6
build inside 30 seconds. The checked-in build completes well inside that budget
under the pinned toolchain. The `tests` step also gives QEMU its own per-case
allowance; `run-xv6.py` self-terminates at 20 seconds.

## Verification

Run the package validator and the container self-check from the repository
root:

```sh
python tools/validate_approved_package.py examples/xv6-lab
docker build --network=host -t labweaver-xv6 examples/xv6-lab
docker run --rm --read-only \
  --tmpfs /work:rw,exec,uid=65534,gid=65534,mode=0777,size=1g \
  --tmpfs /tmp:uid=65534,gid=65534,mode=1777 \
  --tmpfs /workspace:uid=65534,gid=65534,mode=0777 \
  labweaver-xv6 \
  /bin/sh -c 'set -eu; cp -R /opt/labweaver/workspace-seed/. /workspace/; exec /opt/labweaver/scripts/local-test.sh'
```

The self-check builds the real kernel and filesystem image, then runs the smoke
and filesystem cases under QEMU. The Evaluation runner additionally embeds the
platform worker so the platform can execute the same profile inside the OJ
sandbox.
