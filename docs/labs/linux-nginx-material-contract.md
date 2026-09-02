# Linux Nginx Material Contract

## Status and boundary

This document and [`examples/linux-nginx`](../../examples/linux-nginx) are E1 material-contract evidence for Issue #11. They do not prove that a KubeVirt VM, SSH/Ansible Probe, Collector, Evaluation Service, teacher approval path, or production EvaluationRun exists.

The supported teaching target is an Ubuntu 24.04 LTS VM running Nginx on HTTP port 80. The student modifies the enabled default site at `/etc/nginx/sites-available/default` and configures its document root as `/srv/labweaver-nginx-lab`. The supplied `index.html` is the only accepted page identity: title `Nginx Lab`, heading `Nginx Lab`, and `data-lab-id="linux-nginx-v1"`.

Ubuntu 24.04 is the only version identity fixed in this package. The later E3 run record must bind the VM image identity, observed Nginx package version, template SHA-256, material-manifest SHA-256, approved Probe profile version, and build/deployment identity. A moving package version must never be represented as a fixed one.

## Public and controlled material

The public package contains the student statement, page template, candidate submission manifest, expected fact mappings, and a manifest with SHA-256 for every public artifact. It contains no VM image, Probe implementation, private key, token, credential, hidden evidence, or local-machine path.

`material-manifest.json` deliberately records controlled VM and Probe entries as `unbound` and `blocked-*`. Their `private://` locators identify controlled storage classes, not retrievable public URIs. A controlled artifact becomes usable only when its approved version and SHA-256 are recorded outside Git and bound to the same EvaluationRun identity.

`submission.yaml` is a candidate, not a formal runtime `SubmissionManifest`: the repository currently has no schema or Reader for it. It permits only `report.md` as LLM-readable input, caps that file at 64 KiB, and excludes it from deterministic scoring. Missing `report.md` is an advisory failure that requires manual review; it must not change deterministic score, Gate status, or release eligibility. Report text must not include credentials, tokens, private keys, or full logs.

## Planned diagnostics

The following identifiers are material-contract reservations, not implemented runtime diagnostics:

| Candidate diagnostic | Intended disposition |
| --- | --- |
| `LW_LINUX_LAB_MATERIAL_MISSING` | Block material validation before a run. |
| `LW_LINUX_LAB_TEMPLATE_HASH_MISMATCH` | Block use of altered public material. |
| `LW_LINUX_LAB_TEMPLATE_MARKER_MISMATCH` | Fail the page-identity assertion. |
| `LW_LINUX_LAB_RESTRICTED_CONTENT` | Block publication or collection. |
| `LW_LINUX_LAB_NGINX_NOT_LISTENING` | Fail the deterministic Probe step. |
| `LW_LINUX_LAB_SITE_MISMATCH` | Fail the deterministic Probe step. |
| `LW_LINUX_LAB_REPORT_MISSING` | Advisory failure and manual-review requirement only. |
| `LW_LINUX_LAB_REPORT_TOO_LARGE` | Reject report collection before advisory review. |

## Probe contract and blocker

The future approved Probe must be read-only and emit versioned, sanitized facts for host reachability, Nginx installation and observed version, default-site configuration and root, systemd state, TCP/80 listener, HTTP status/body, and the three required HTML markers. It must not restart Nginx, write configuration, repair the VM, invoke shell, or fall back to an unapproved provider.

The current `evaluation-spec/v1` allowlist permits only package facts, service facts, and file stat. It cannot yet observe TCP/80 or the HTTP response needed by this contract. B must approve and implement a versioned, read-only Probe profile, its minimum capability contract, SubmissionManifest runtime Reader, and stable runtime diagnostics before any full Probe or E3 claim. Until then, incomplete capability is an explicit blocker, not a reason to use a Mock or weaken the expected facts.

## E1 validation

Run the following from the repository root:

```sh
python examples/linux-nginx/verify_material_contract.py --self-test
cargo xtask test --suite contract
```

The Python validator checks public SHA-256 records, HTML identity, candidate submission limits, controlled-material boundary, normal/negative scenario mapping, missing material, altered template, restricted content, and oversized report handling. The Rust test only preserves the existing EvaluationSpec E1 contract; it does not validate the planned Probe behavior.

## E3 exit condition

E3 requires a real KubeVirt VM using the approved Ubuntu image, an approved Probe profile, and same-identity evidence for the normal target state plus both negative cases: stopped/not-listening Nginx and a site/page mismatch. Missing preflight, image binding, Probe capability, access, timeout, malformed fact, or missing evidence must leave the run explicitly blocked or failed with the original diagnostic retained.
