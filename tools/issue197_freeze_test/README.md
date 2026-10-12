# Issue 197 N100 freeze test support

This bundle prepares a bounded worker test. It does not apply cluster resources.
PR240 candidate is `f6e16444ddfaefc3b63c058a40a42393201d253a`.
Its source tree is `148584b29e44b3e31d6ff08865938b118ac388ac`.
The parent reported deployed revision `9b5c21f6`, which does not contain this candidate.

The parent verified three Ready Kubernetes 1.35.6 nodes, KubeVirt 1.8.4, and CDI 1.65.0.
The parent also verified the demo NFS PVC and a VM dry run without GPU allocation.
Reuse those results. This bundle does not repeat those experiments.

## Fetch support and preserve the source candidate

Fetch the original `codex/197-upload-content-identity` branch through the existing authorized Git connection.
Checkout the support commit given in the PR240 handoff.
The support commit is a delivery change. It is not the image source candidate.
The build script still defaults to the complete frozen `f6e16444ddfaefc3b63c058a40a42393201d253a` commit.
Do not build the support branch HEAD as the tested candidate.

From the repository root, check the frozen source tree:

```sh
git rev-parse f6e16444ddfaefc3b63c058a40a42393201d253a^{tree}
```

The result must equal `148584b29e44b3e31d6ff08865938b118ac388ac`.
Then enter `tools/issue197_freeze_test` for the preparation commands below.
No script fetches credentials, pushes assets, deploys a cluster, or imports a node image.

## Stop conditions

Candidate OCI digest, admitted test identity, and a real Work binding remain required.
An isolated Evaluation database must have no active outbox dispatcher.
The worker publishes a frozen event into that database's outbox.
A shared database could let existing consumers start an evaluation run.
Do not mount a shared database credential into this test.

The standalone `labweaver-demo` PVC is not an Environment Work binding.
The Environment service must return a Container binding for the reviewed test environment.
That binding names `lw-env-<environmentId>` and the `workspace` PVC.
Do not replace its IDs, namespace, PVC, source identity, or admitted manifest.
Stop if the deployed producer's command differs from the candidate contract.
Do not remove unknown fields to force compatibility with `9b5c21f6`.

No worker image digest has been fabricated.
The cloud preparation does not build or publish a complete OCI image.
The image remains unavailable until the controlled build and node import complete.

## Payload and files

The scripts need Python 3.11 or newer and the existing `jsonschema` package.
This bundle does not install software on N100 or a cluster node.
Confirm the approved tool environment before you run the scripts.

`candidate.json` records the exact source, Containerfile, compiler, and runtime base pins.
The scripts read the existing repository schemas and check their candidate hashes.
`fixtures/` contains two synthetic workspace variants and their expected byte hashes.
Both variants contain three files and 62 source bytes.
Only `src/main.c` changes from `return 1` to `return 2`.
The file size stays equal. `empty.txt` also tests an empty file.
The manifest selects only this run's three paths. It has no LLM-readable input.

The examples contain no credentials and no usable run identity.
All approval flags start false. No example can render an executable plan.
Authority flags are operator attestations. The script cannot certify Kubernetes or database permissions.

## Exact image build

Use an authorized checkout that contains the candidate commit.
Do not build from the deployed `9b5c21f6` checkout.
Use the fixed builder and runtime digests in `candidate.json`.
The source epoch is `1791791504`. The component is `evaluation-service`.
The target is `runtime`, not an agent or LLM target.
Use Buildx v0.35.0 and the existing builder with the pinned BuildKit image.
The repository CI pins `moby/buildkit@sha256:6b59b7df63a8cb9902736f9ddf7fcff8261613d3e7449b8ea8b7537fc399c03a`.
The exporter preserves the CI timestamp rewrite and OCI media types.
Do not replace an unavailable base or builder with a mutable tag.

1. Run the source check without a build.

   ```sh
   python3 build_candidate.py --repo /path/to/LabWeaver --output /reviewed/output/lw197-evaluation.oci
   ```

2. Review the printed tree, source ref, command, and output path.
3. After build approval, repeat the command with `--build`.
4. Review the OCI manifest digest and revision label in its receipt.
5. After node-import approval, import that archive through the existing container runtime authority.
6. Read the chosen node's CRI image metadata.
7. Confirm the exact repository digest and `linux/amd64` platform before you mark the import verified.

The local image name is `localhost/labweaver-197/evaluation-service:f6e16444`.
No script pushes to a registry or creates an external asset target.
The Pod uses the receipt's manifest digest and `imagePullPolicy: Never`.
An image config ID is not a registry or OCI manifest digest.
Stop if the node cannot resolve the exact digest reference.

An equivalent source ref is allowed only when its complete tree equals the reviewed tree.
Record that source ref. The candidate label and epoch still refer to the reviewed commit.
This rule can check an authorized local equivalent tree. N100 uses the frozen public candidate by default.

## Existing permissions and facts

| Authority | Required existing scope | Current evidence |
| --- | --- | --- |
| N100 operator | Read nodes, named PVC, namespace quota, Jobs and Pod status | Parent verified cluster and demo storage. Target Work permissions remain unverified. |
| Test actor | Own the test Project and approved Work. Permit `submission:freeze` for that environment. | No cluster test actor or admitted run was supplied. |
| Evaluation caller | Existing `labweaver-evaluation` service identity and `evaluation.environment.freeze` permission | Use existing identity. Do not export tokens or add service roles. |
| Worker ServiceAccount | Existing account in the bound Work namespace | Must exist. The Pod does not mount its token or call Kubernetes. |
| Database | Existing runtime access to the isolated Evaluation schema and its candidate migrations | Must be confirmed. No active dispatcher can share that database. |
| Object store | Existing locked bucket. Allow test-prefix PUT, HEAD, version GET and retention verification | Must be confirmed. Do not create a bucket or change IAM with this bundle. |
| Network | Existing DNS, PostgreSQL and MinIO HTTPS egress for the exact test Pod labels | Must be confirmed. No NetworkPolicy is supplied. |
| Node image import | Existing authority to import the local OCI archive on the chosen node | Requires parent approval. Do not change node configuration. |

The existing runtime role needs normal read/write access to `submission_freeze_requests`,
`submission_freeze_attempts`, `frozen_submissions`, and `outbox_events` in the isolated Evaluation database.
Use existing grants. Do not grant new rights or run baseline migrations against a shared database.
If a test database is absent, ask the parent to approve a separate provisioning plan first.

Use existing read-only tools to check these facts. Do not read or print Secret values.
For Kubernetes permissions, use `kubectl auth can-i` as the existing operator.
Do not use impersonation, create RBAC, or alter security settings.
A server dry run checks admission. It does not prove runtime connectivity or source binding.

For the exact bound namespace, check existing `create` permission on Jobs and ConfigMaps.
Also check `get` on Jobs, Pods, PVCs, ServiceAccounts and ResourceQuotas.
Check `list` on Pods and `get` on `pods/log` for observations.
Cleanup needs existing `delete` permission on the recorded test Jobs and ConfigMaps.
Permission does not replace the parent's approval of this specific operation.

## Actions that need approval

The parent must approve the rendered names, namespace, existing Secret reference, and data scope.
The plan creates one ConfigMap and one Job for each phase.
Run at most one worker Pod at a time.
Phase A, replay A, and phase B need at most three Jobs and three ConfigMaps.
Each Pod requests 100m CPU and 128Mi memory. Its limits are one CPU and 1Gi memory.
Its deadline is 900 seconds. Its retry count is zero.
It requests no GPU, host device, host network, or privileged access.
It mounts the existing Work PVC read only at `/workspace`.
It uses UID/GID 65532, RuntimeDefault seccomp, and a read-only root filesystem.

The plan creates no Namespace, PVC, ServiceAccount, RBAC, Secret, or NetworkPolicy.
It changes none of the original 14 Pods or their GPU allocations.
The selected namespace must already have sufficient quota and source read permissions.
Stop on a missing account, Secret, image, quota, or network permission.
Do not raise limits or change existing policies to make the test pass.

The test actor also needs approval to copy the three synthetic files into the owned Work.
Write only the `lw197-n100-20261012/` directory in that Work workspace.
Do not modify other paths or permissions. Do not use an init container with root access.
If no approved Work exists, prepare a separate API-based Work creation plan for the parent.

## Input preparation

1. Export `binding.json` through the authorized Environment freeze-binding API.
2. Export `admitted-command.json` from the authorized candidate freeze coordinator.
3. Use the approved synthetic Evaluation manifest selection for the three fixture paths.
4. Review actor, Project, environment, AgentRun, submission, retention, and idempotency identities.
5. Complete `facts.example.json` with reviewed facts. Rename the file `facts.json`.
6. Complete the image receipt after build and node import.
7. Copy only `fixtures/workspace-a/lw197-n100-20261012/` into the owned Work workspace.

The request manifest name and bounds can come from the approved Evaluation projection.
The renderer preserves them. It requires the three exact test paths and no LLM input.
Do not submit the local synthetic parse example as a cluster identity.
Worker-only execution does not replace public acceptance, authorization, or coordinator validation.

## Run plan and observations

1. Render the plan without cluster access.

   ```sh
   python3 render_worker.py --command admitted-command.json --binding binding.json \
     --facts facts.json --receipt image-receipt.json --expected fixtures/expected-files.json --output plan-a.json
   ```

2. Give the parent the plan and reviewed inputs before any apply operation.
3. Use existing operator permission for a server dry run in the bound namespace.
4. After approval, create only the named test ConfigMap and Job.
5. Record Pod image ID, admitted digest, node, exit status, logs, and resource UIDs.
6. Read the isolated database result with `evidence.sql` and an existing authorized database tool.
7. Export only the synthetic FrozenSubmission and its exact object version.
8. Verify the result with `verify_frozen.py` for phase A.

The program is `/usr/local/bin/labweaver-service --mode freeze-worker`.
It reads `LABWEAVER_EVALUATION_CONFIG_FILE` and `LABWEAVER_FREEZE_COMMAND_FILE`.
The rendered ConfigMap supplies JSON, which the existing YAML reader accepts.
Mounted Secret files supply existing credentials and the CA. The bundle contains none.
The Secret must also provide `postgres-ca.pem` for the existing strict database URL.
Keep its `sslrootcert` file reference. Do not disable database TLS verification.

For exported phase A evidence, run:

```sh
python3 verify_frozen.py --submission result-a.json --archive frozen-a.json \
  --expected fixtures/expected-files.json --phase a
```

Keep the completed phase A contract and exact object version.
After phase A completes, replace only the owned test `src/main.c` with variant B.
Reuse the complete original command, including retention, IDs and idempotency key, for replay A.
The durable result and object version must remain exactly phase A.
Do not create a fresh retention snapshot for a replay.

```sh
python3 verify_frozen.py --submission replay-a.json --archive frozen-a.json \
  --expected fixtures/expected-files.json --phase a --compare result-a.json --replay
```

For phase B, obtain a new admitted submission and idempotency key from the authorized producer.
Use the same synthetic selection and the current real source binding.
The new result must have equal file sizes and different content hashes.
The phase A object version must still return its original bytes.

```sh
python3 verify_frozen.py --submission result-b.json --archive frozen-b.json \
  --expected fixtures/expected-files.json --phase b --compare result-a.json
```

The existing worker has no deterministic pause between preflight and collection.
This bundle does not claim a live between-stage rewrite rejection test.
The existing local Collector test covers that behavior. A live timing race is not a deterministic pass.

## Expected writes and cleanup

Two new successful freezes write two isolated requests, attempts, submissions, and outbox events.
Replay A returns its existing result. It must not create a second frozen object or publication.
The object keys stay under `frozen-submissions/<run-id>/<submission-id>/attempt-<n>.json`.
The existing locked bucket retains each exact version under the admitted retention policy.
The normal coordinator example uses 90 days. Do not shorten a real admitted policy for cleanup.

No cleanup script runs automatically.
After evidence export and parent approval, remove only recorded test Jobs and ConfigMaps by name and UID.
Check the test-run label and namespace before each delete.
Keep the PVC, namespace, database facts, retained object versions, and imported image for review.
An owner can later remove the three known synthetic files after byte and path checks.
Stop if that directory contains unexpected files or a symlink.
Do not delete a namespace, PVC, bucket, shared database, or non-test object.
Do not bypass Object Lock, prune node images, truncate tables, or reset shared data.

## VM and SFTP boundary

A VM dry run does not provide a running guest or SFTP Collector binding.
The parent must supply an approved test VM, guest IP, source root, host-key identity, and short-lived Collector certificate.
Use the existing Environment binding and coordinator to provide those values.
Do not invent an SSH username, key, principal, or source hash.
This PVC renderer rejects VM bindings. No VM/SFTP execution has occurred in this preparation.
The full cluster freeze chain remains unexecuted until candidate deployment and real run binding exist.

## Checks on test support

Run these checks from the repository root:

```sh
python3 -m unittest discover -s tools/issue197_freeze_test -p 'test_*.py' -v
python3 tools/issue197_freeze_test/build_candidate.py --repo . --output /reviewed/output/lw197-evaluation.oci
```

The tests use synthetic files and in-memory plans. They do not build images or call a cluster.
The original cloud preparation also compiled the equivalent candidate source offline.
Its Worker parsed a synthetic command and stopped at a noncredential file boundary.
It did not connect to a database or object store and did not execute a freeze.
The debug executable is not a deployment image and is not committed.

The exact OCI image still needs build, digest validation, and authorized node import.
No image archive, candidate deployment, VM guest, or SFTP freeze has run in this preparation.

## Existing entry points

This directory is test support, not a second freeze implementation.
It uses `services/evaluation-service/src/worker.rs` and `containers/Containerfile.rust`.
It reads the existing versioned schemas. It does not duplicate or edit them.
Existing Collector and PostgreSQL tests retain core freeze behavior coverage.
`tools/user_acceptance.py` covers a larger Browser-based acceptance journey.
It is not the bounded N100 command-line preparation described here.

`SubmissionFrozenConsumer.process_next` can reserve an admitted evaluation run after a frozen event.
The isolated database and absent dispatcher prevent these test facts from entering that active consumer path.
