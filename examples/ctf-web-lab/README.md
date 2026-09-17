# CTF web lab: Flag Vault

This package is a self-contained web vulnerability exercise. The student
environment runs a small intentionally vulnerable HTTP service ("Flag Vault").
The service stores a deterministic flag in an operations account and builds its
login query by string concatenation. A student who injects into the login form
can read the operations account's secret without credentials.

The package deliberately ships its own vulnerable application module on top of
a digest-pinned Debian base rather than a third-party challenge image. That
keeps the seeded flag, the HTTP surface, and the evaluation contract reviewable
in this repository and reproducible offline. If the operator prefers a
third-party vulnerable image such as OWASP Juice Shop, the root `Dockerfile` is
the single place to swap the base, but the operator must then re-derive the seed
mechanism and pin the exact image digest before publishing.

## Flag contract

- The seeded flag is `FLAG{ctf_web_sqli_admin_secret_v1}`.
- It is stored only in the operations account row of the seeded SQLite database.
- The service writes the student's submitted flag to `student/flag.txt` in the
  writable workspace through `POST /submit`.
- `evaluation.yaml` collects only `student/flag.txt`, checks the `FLAG{...}`
  shape, and compares it byte-for-byte against the hidden evaluator value. The
  expected value is not copied into the student environment image.

The student environment has no interactive terminal. All interaction is through
the single declared HTTP entry, so the flag cannot be read from the packaged
database file through a shell.

## Package contents

- `environment.yaml` binds the non-root, read-only container with a single HTTP
  entry and restricted egress.
- `evaluation.yaml` collects the proof file, gates on its shape, and scores an
  exact flag comparison.
- `app/server.py` and `seed/seed.py` are the vulnerable service and its
  deterministic seed.
- `profiles/ctf-python.json` is the direct-argv Python profile.
- `scripts/ctf_check.py` is the compile-phase format gate and the test-phase
  exact comparison.
- `scripts/local-test.sh` runs the offline self-check.
- `tests/ctf-python/` contains the hidden evaluator case and expected value.
- `evaluation/Dockerfile` is the per-experiment Evaluation runner image with the
  platform worker binary.
- `manifest.json` records the SHA-256 of every payload file.

## Local verification

Build the student environment image and run the self-check. The check seeds a
temporary database, starts the service, tries the injection, verifies that a
guest login is denied, submits the captured flag, and reads back the proof file.

```sh
docker build -t labweaver-ctf-web examples/ctf-web-lab
docker run --rm --read-only --network=none \
  --tmpfs /tmp:uid=65534,gid=65534,mode=1777 \
  --tmpfs /workspace:uid=65534,gid=65534,mode=0777 \
  labweaver-ctf-web \
  /opt/labweaver/scripts/local-test.sh
```

To exercise the service by hand, publish port 8080 and open
`http://localhost:8080/`:

```sh
docker run --rm --read-only --network=none -p 8080:8080 \
  --tmpfs /tmp:uid=65534,gid=65534,mode=1777 \
  --tmpfs /workspace:uid=65534,gid=65534,mode=0777 \
  labweaver-ctf-web
```

The platform build uses BuildKit egress only to install the pinned Debian
`python3` package. The runtime service performs no network egress and writes
only below the mounted `/workspace`.
