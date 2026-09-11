# xv6 RISC-V lab package

This example is a generic LabWeaver package for a small xv6 user-program lab. It
uses the ordinary `container` runtime and the approved program runner. The
course label does not select a backend: the package binds an explicit
RISC-V toolchain profile and test inputs instead.

The profile invokes two checked-in POSIX scripts through `/usr/bin/sh` with
direct argv vectors; no shell command is assembled from student input. The
compile script extracts the pinned xv6 source archive into the writable attempt
directory, installs the submitted user program as `user/student.c`, and builds
the real kernel and filesystem image. The run script boots that image with
`qemu-system-riscv64` in TCG mode, applies a five-second timeout, and returns
only the student's tagged line for deterministic checking. The worker only
substitutes its four approved path tokens (`{source}`, `{binary}`,
`{submission_dir}`, and `{evaluator_dir}`).

The checked-in files are public teaching material. Hidden tests, the reference
program, and the exact toolchain image are controlled artifacts bound by the
teacher during publication. They are deliberately represented by evaluator
locators in `evaluation.yaml` and are not copied into this repository.

The image places the initial public workspace below
`/opt/labweaver/workspace-seed`. The platform mounts the persistent workspace
at `/workspace` and copies that seed only when it is empty, so retained student
files survive a restart. The image runs as UID/GID 65534 with a read-only root
filesystem; `/workspace` and `/tmp` are the writable runtime mounts. The
platform terminal is `/bin/sh` in `/workspace`, and the declared HTTP entry
serves the public workspace with the image's persistent Python standard-library
server.

## Package contents

- `environment.yaml` binds a non-root, read-only container environment with no
  public endpoint exposure.
- `evaluation.yaml` runs a file gate, compiles the submitted user program, and
  executes smoke and filesystem test groups through the same approved profile.
- `profiles/xv6-riscv64.json` contains the direct compile and run argv vectors.
- `student/student.c` is a minimal xv6 user program that can be replaced by a
  student submission.
- `scripts/` contains the bounded build and QEMU TCG entry points.
- `tests/xv6-riscv64/` contains the concrete public input/output cases.
- `xv6/` vendors the MIT-licensed reference source and its source archive.
- `manifest.json` lists every package payload file and its digest.

`Dockerfile` builds the same ordinary runner image locally with a pinned Debian
base, the RISC-V cross compiler, and `qemu-system-riscv64`; it does not require
privileged mode, KVM, or a host toolchain. Run `docker build -t labweaver-xv6
examples/xv6-lab` followed by the command below to build the package and
execute the public cases under the runtime's UID, read-only root, and writable
temporary mounts:

```sh
docker run --rm --read-only \
  --tmpfs /work:rw,exec,uid=65534,gid=65534,mode=0777,size=1g \
  --tmpfs /tmp:uid=65534,gid=65534,mode=1777 \
  --tmpfs /workspace:uid=65534,gid=65534,mode=0777 \
  labweaver-xv6 \
  /bin/sh -c 'set -eu; cp -R /opt/labweaver/workspace-seed/. /workspace/; exec /opt/labweaver/scripts/local-test.sh'
```

The runner copies the compiled `fs.img` into a private temporary directory below
the evaluator's current working directory for each test case, runs QEMU against
that copy without `-snapshot`, and removes the temporary disk after QEMU stops.
The shared compiled image remains unchanged; the local smoke also checks its
SHA-256 after every case. The explicit seed copy models the platform's
empty-workspace init container, and the default image command starts the HTTP
file service. Before publishing, replace the example object references and
digest-pinned runner image with the real artifacts produced by the Control and
Agent pipelines. A successful local validation of this example does not approve
or publish those artifacts.
