# Offline authentication remediation lab

This package is a bounded, offline security exercise that runs through the same
container and direct `program` runner used by ordinary C labs. The student
receives a small command-line authentication service with a deliberate logic
flaw: it accepts a non-empty prefix of the synthetic operator password as if it
were the complete password. The task is to make authentication require the
complete password while preserving the input format and normal login behavior.

The only account, password, and flag in this exercise are public teaching data:
`operator`, `training-password-42`, and
`TRAINING_FLAG_OFFLINE_AUTH_DEMO`. They are not credentials for any real
system. A successful login prints the synthetic flag; a rejected record prints
`access-denied`.

The target reads newline-terminated `user:password` records from standard input.
It has no network client, filesystem escape, shell invocation, privilege
operation, or host interaction. The exercise threat is limited to an
unauthorized login caused by the prefix comparison. Inputs are bounded to 64
records and a fixed-size line buffer. The container runs as a non-root user with
a read-only root filesystem, denied privilege escalation, and restricted
egress. The writable state is limited to the supplied `/workspace` and
per-attempt working directory.

The image serves the public exercise files with the standard-library
`python3 -m http.server` on the declared HTTP port 8080. This is only a file
browser for instructions and fixtures; it is not the authentication target and
does not expose a network attack surface beyond the controlled environment
entry. The platform terminal uses the existing platform-controlled `/bin/sh`
terminal contract in `/workspace`.

The initial public workspace is baked into
`/opt/labweaver/workspace-seed`. The platform mounts the persistent workspace
at `/workspace` and copies that seed only when the mount is empty, so a restart
does not overwrite student files. The seed is readable by the runtime UID
65534. Local validation mounts writable `/work`, `/tmp`, and an empty
`/workspace` tmpfs and performs the same seed copy before running the checks.

## Package contents

- `environment.yaml` binds the non-root, read-only container, restricted egress,
  interactive workspace terminal, and the actual HTTP file service.
- `evaluation.yaml` collects only `student/auth.c`, compiles it with the
  approved `security-cpp17` direct-argv profile, and scores public deterministic
  login, bypass, and parser-boundary cases. No advisory review can change the
  numeric score.
- `student/auth.c` is the intentionally vulnerable starting template.
- `reference/vulnerable_auth.c` and `reference/fixed_auth.c` are small public
  references used by the local demonstration. The demonstration proves that
  the prefix exploit grants access in the vulnerable template and is rejected
  by the fixed reference.
- `tests/security-controlled/*.in` and `*.out` are public synthetic fixtures.
  The `authentication-bypass` group contains the exploit input and requires it
  to be rejected after remediation.
- `profiles/security-cpp17.json` is the existing direct `gcc` profile. It
  permits only the compiler and binary paths defined by the platform profile.
- `manifest.json` is the single package file inventory and records the SHA-256
  for every listed public artifact. This example does not claim a hidden oracle
  or a private credential store.

## Local verification

Build the pinned Debian-based image from the repository root, then run the test
command explicitly:

```sh
docker build --tag labweaver-security-controlled examples/security-controlled
docker run --rm --read-only \
  --tmpfs /work:rw,exec,uid=65534,gid=65534,mode=0777 \
  --tmpfs /tmp:uid=65534,gid=65534,mode=1777 \
  --tmpfs /workspace:uid=65534,gid=65534,mode=0777 \
  labweaver-security-controlled \
  /bin/sh -c 'set -eu; cp -R /opt/labweaver/workspace-seed/. /workspace/; exec /opt/labweaver/scripts/local-test.sh'
```

The test compiles the starter, vulnerable reference, and fixed reference. It
asserts that the starter and vulnerable reference accept the prefix exploit and
that the fixed reference rejects it, then runs every public case against the
fixed reference to validate the fixtures. The platform evaluation runs those
same deterministic cases against `student/auth.c` and awards points only after
the student repairs the bypass.

To inspect the public files through the declared environment service, run the
image normally and open `http://localhost:8080/` from the controlled local
machine:

```sh
docker run --rm --read-only \
  --tmpfs /tmp:uid=65534,gid=65534,mode=1777 \
  --tmpfs /workspace:uid=65534,gid=65534,mode=0777 \
  labweaver-security-controlled \
  /bin/sh -c 'set -eu; cp -R /opt/labweaver/workspace-seed/. /workspace/; exec /usr/bin/python3 -m http.server 8080 --bind 0.0.0.0 --directory /workspace'
```
