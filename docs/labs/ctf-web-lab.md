# CTF Web Lab Material Contract

## Status and boundary

This document describes the teaching material in
[`examples/ctf-web-lab`](../../examples/ctf-web-lab). The package is a
container-only experiment that runs through the ordinary `container` runtime,
the direct `program` runner, and the per-experiment Evaluation runner image. It
does not require a VM, a GPU, or any external target.

The package ships its own intentionally vulnerable HTTP service on a
digest-pinned Debian base instead of a third-party challenge image. A
controlled application makes the seeded flag, the HTTP surface, and the
evaluation fixture reviewable in this repository and reproducible offline. An
operator may substitute a third-party vulnerable image such as OWASP Juice
Shop, but must then re-derive the seed mechanism and pin the exact image digest
before publishing.

## Intended exploitation path

The service exposes a login form. The handler builds its lookup statement by
string concatenation (`WHERE username = '<u>' AND password = '<p>'`). A student
supplies a username such as `' OR 1=1--` so the predicate becomes true for
every row and the response discloses the operations account's secret, which is
the flag. A legitimate guest login is denied.

The service has no interactive terminal in the student environment. All
interaction is through the single declared HTTP entry (`/`, `/login`,
`/submit`, `/healthz`). The student records the captured flag with
`POST /submit`; the service writes it to `student/flag.txt` in the writable
workspace.

## Flag contract

- The seeded flag is `FLAG{ctf_web_sqli_admin_secret_v1}` and is stored only in
  the operations account row of the seeded SQLite database.
- `evaluation.yaml` collects only `student/flag.txt`, gates on the `FLAG{...}`
  shape with `profiles/ctf-python.json`, and scores a byte-exact comparison
  against the hidden expected value in `tests/ctf-python/`. The expected value
  is not copied into the student environment image.
- The flag is public teaching data and is not a credential for any real system.

## Verification

Run the package validator and the container self-check from the repository
root:

```sh
python tools/validate_approved_package.py examples/ctf-web-lab
docker build --network=host -t labweaver-ctf-web examples/ctf-web-lab
docker run --rm --read-only --network=none \
  --tmpfs /tmp:uid=65534,gid=65534,mode=1777 \
  --tmpfs /workspace:uid=65534,gid=65534,mode=0777 \
  labweaver-ctf-web \
  /opt/labweaver/scripts/local-test.sh
```

The self-check seeds a temporary database, starts the service, verifies the
injection discloses the flag, verifies a guest login is denied, submits the
flag, and reads back `student/flag.txt`. A successful local run does not approve
or publish the package, and the manifest's runner image reference remains an
example placeholder until Control and Agent produce the real artifact.
