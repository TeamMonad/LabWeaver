# Issue 197 bounded freeze test support

This directory prepares test inputs and a reviewable plan. It applies no cluster resources.
The existing N100 validation thread owns real tests.
Use only the parent's selected, existing test namespace. Do not use a business namespace.
Supply the namespace locally. This public bundle contains no real cluster locator or credential.

## Fetch and preserve the source candidate

Fetch `codex/197-upload-content-identity` through the existing authorized Git connection.
Checkout the support commit in the PR240 handoff.
Use `tools/issue197_freeze_test` from that checkout.
The support commit is a delivery change. It is not the image source candidate.

The frozen source is `f6e16444ddfaefc3b63c058a40a42393201d253a`.
Its tree is `148584b29e44b3e31d6ff08865938b118ac388ac`.
`candidate.json` records the existing Containerfile and immutable base pins.
It records no built OCI digest. Do not build branch HEAD as the source candidate.

From the repository root:

```sh
git rev-parse f6e16444ddfaefc3b63c058a40a42393201d253a^{tree}
python3 -m unittest discover -s tools/issue197_freeze_test -p 'test_*.py' -v
```

The first result must equal the frozen tree.
The scripts need Python 3.11 or newer and `jsonschema` from `requirements.txt`.
They do not install dependencies automatically.
They read the existing versioned repository schemas and check their candidate hashes.
No schema copy, binary, image archive, or environment credential is supplied.

## Source and authority gates

The worker needs an admitted command and an authoritative Environment source binding.
Export those only through the existing authorized producer and binding interface.
Do not invent an actor, Project, Environment, AgentRun, submission, retention, or source identity.
Do not remove fields to force an older producer to match the candidate contract.

The current frozen Container binding derives its namespace as `lw-env-<environmentId>`.
The implementation is in `services/environment-service/src/freeze_binding.rs` and
`services/evaluation-service/src/coordinator.rs`.
That rule conflicts with the separately selected test namespace.
`render_worker.py` refuses this mismatch. The current full Container chain is therefore blocked.
Do not rewrite an exported binding or use a copied fixture as evidence of the original Work source.
PVC references are namespace-local. An existing source PVC must be in the selected namespace.
A separately reviewed authoritative source solution is required. This bundle does not change it.

Complete a local copy of `facts.example.json` only after the parent reviews the missing prerequisites.
All approval flags start false, and `existingNamespace` starts null.
Set that field to the exact existing namespace selected by the parent.
The required CLI namespace must match both reviewed facts and the authoritative source binding.
Only the dedicated `lw-bundle-` namespace family is accepted.
These flags are operator attestations. The script cannot certify permissions or connectivity.
The example object endpoint uses the reserved `.invalid` domain. It is not a usable target.

## Bounded resources and missing approvals

Each plan contains one ConfigMap and one Job. There is no namespace setup or deletion.
Run at most one worker Pod at a time. A, replay A, and B need at most three Jobs and ConfigMaps.
Each Pod requests 100m CPU and 128Mi memory and has limits of one CPU and 512Mi memory.
It has a 900-second deadline and no automatic retry.
The parent-reported namespace ceiling is one CPU and 512Mi. Verify free quota before apply.
Memory use at this limit has not been tested on N100. Record OOM or quota rejection as failure.
Do not increase the ceiling to make the test pass.

The Pod has no GPU, host device, host network, privileged mode, or mounted ServiceAccount token.
It uses UID/GID 65532, RuntimeDefault seccomp, a read-only root filesystem, and dropped capabilities.
It mounts the existing authoritative source PVC read only at `/workspace`.
Existing source read access must work without changing ownership or permissions.
The renderer creates no Namespace, PVC, Secret, ServiceAccount, RBAC, or NetworkPolicy.
It changes no business workload. It does not read credential values.

Give the parent the following exact differences before real execution:

| Prerequisite | Required bounded scope; namespace setup alone does not authorize it |
| --- | --- |
| Source binding | Resolve the namespace conflict without editing the frozen candidate or exported source identity. |
| Existing source | Verify the named source PVC and UID/GID 65532 read access in the selected namespace. No new PVC or permission change is supplied. |
| Existing account | Verify a selected-namespace ServiceAccount and existing operator rights for the named test Job and ConfigMap. No new RBAC is supplied. |
| Existing Secret | Five existing file keys: database URL, object access key, object secret key, object CA and database CA. No value export, Secret copy, or creation is authorized. |
| DNS egress | If needed, only the reviewed resolver on UDP and TCP 53, scoped to the test Pod labels. The worker requires no ingress. |
| Database egress | Only the reviewed isolated database endpoint and configured TLS TCP port, normally 5432. A deny-all policy does not permit this. |
| Object egress | Only the reviewed existing object endpoint and configured HTTPS TCP port. Verify test-prefix PUT, HEAD, version GET and retention access. Do not create a bucket or open all destinations. |
| Database isolation | Existing candidate schema and normal runtime access, with no active outbox dispatcher. Shared consumers can start evaluation work from a frozen event. |
| Exact image | Approved local OCI build, actual manifest digest and chosen-node import. No image is built or imported by preparation. |
| Admission and quota | Verify restricted admission and remaining quota. A dry run does not prove connectivity or source provenance. |
| VM/SFTP | Not supported by this PVC renderer. No guest, certificate, SFTP egress or new VM resource is authorized. |

Use existing read-only operator tools to verify prerequisites. Do not print Secret values.
Keep actual endpoint, namespace, identity and credential references in locally reviewed inputs.
Do not change deny-all rules, grant roles, create Secrets, or import images from this bundle.

## Exact image preparation

Enter `tools/issue197_freeze_test` for the commands below.
Use a checkout that contains the frozen source commit.
Use the fixed Buildx, BuildKit, compiler and runtime pins recorded in the repository and `candidate.json`.
Do not replace a missing pin with a mutable tag.

```sh
python3 build_candidate.py --repo /path/to/LabWeaver --output /reviewed/output/lw197-evaluation.oci
```

This is a plan only. Review its source, tree, command and output path.
Only after separate build approval, repeat with `--build`.
The component is `evaluation-service`, the target is `runtime`, and the platform is `linux/amd64`.
The source epoch is `1791791504`. No registry push is performed.
An equivalent local source ref is accepted only if its complete tree equals the frozen tree.
The target revision remains the frozen candidate. N100 uses the full frozen commit by default.

Review the generated OCI receipt. A config ID is not a manifest digest.
Only after separate node-import approval, use existing runtime authority to import the archive.
Verify the exact digest on the chosen node before marking the receipt imported.
`imagePullPolicy: Never` performs no registry download. A missing digest reference is a stop condition.

## Synthetic selection and run plan

The two fixture variants contain three files and 62 source bytes.
Only `src/main.c` changes from `return 1` to `return 2`; its size stays equal.
`empty.txt` also tests an empty file.
The manifest selects those exact paths, without symlink following or LLM-readable input.
It does not increase service limits.

After the source conflict and all prerequisites have been resolved and approved:

1. Review the real binding, admitted command, retention snapshot and local facts.
2. Copy only the three known fixture paths into the owned test source. Do not change other paths.
3. Render locally. Set `LW197_TEST_NAMESPACE` to the exact existing namespace selected by the parent.

   ```sh
   python3 render_worker.py --command admitted-command.json --binding binding.json \
     --facts facts.json --receipt image-receipt.json --expected fixtures/expected-files.json \
     --namespace "$LW197_TEST_NAMESPACE" --output plan-a.json
   ```

4. Review the complete plan before a server dry run or apply. Use existing operator authority.
5. After specific approval, create only the named test ConfigMap and Job in the selected namespace.
6. Record admitted digest, Pod image ID, node, exit status, logs and resource UIDs.
7. Use `evidence.sql` with an existing authorized tool to export only synthetic test results.
8. Export the exact immutable object version and verify the actual archive bytes.

The worker uses the existing `--mode freeze-worker` entry point.
Its mounted command and config are JSON; the existing YAML reader accepts JSON.
Existing Secret files provide credentials and CA certificates. Keep strict TLS verification.

For phase A:

```sh
python3 verify_frozen.py --submission result-a.json --archive frozen-a.json \
  --expected fixtures/expected-files.json --phase a
```

Keep that result and exact object version. After completion, replace only the test `src/main.c` with B.
Replay the complete original command, including IDs, retention and idempotency key.
The replay must return the exact phase A result without another object or publication.

```sh
python3 verify_frozen.py --submission replay-a.json --archive frozen-a.json \
  --expected fixtures/expected-files.json --phase a --compare result-a.json --replay
```

For B, obtain a new admitted submission and key from the authorized producer.
The new result must have equal file sizes and different content hashes.
The original version must still return A's bytes.

```sh
python3 verify_frozen.py --submission result-b.json --archive frozen-b.json \
  --expected fixtures/expected-files.json --phase b --compare result-a.json
```

The worker has no deterministic pause between preflight and collection.
This bundle does not claim a live between-stage rewrite test. Existing Collector tests cover that case.
Worker-only checks do not replace public authorization, acceptance or coordinator validation.

## Evidence and cleanup boundary

The local support tests use only synthetic files and in-memory plans.
They check schema pins, byte hashes, equal-size edits, authority refusal, namespace mismatch,
bounded resources, archive corruption and fixture overwrite refusal.
They perform no image build, cluster call, credential access, database connection or freeze.
The full Container chain, VM guest and SFTP path remain unexecuted by this preparation.

No cleanup runs automatically.
After evidence export and separate approval, remove only the recorded test Job and ConfigMap names and UIDs.
Check namespace and test labels first. Keep source, database facts, retained versions and image for review.
Do not delete a namespace, PVC, bucket, shared database, or non-test object.
Do not bypass Object Lock, prune images, truncate tables, or reset shared data.
Only an approved owner can later remove the three verified synthetic files.

This directory reuses `services/evaluation-service/src/worker.rs` and `containers/Containerfile.rust`.
It does not duplicate schemas or implement another freeze path.
Existing Collector and PostgreSQL tests retain core behavior coverage.
`tools/user_acceptance.py` covers a broader Browser-based journey and is not this bounded preparation.
