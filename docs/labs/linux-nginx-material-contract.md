# Linux Nginx Material Contract

## Status and boundary

This document describes the teaching material in [`examples/linux-nginx`](../../examples/linux-nginx). The package still needs approved VM and Probe artifacts before it can run on a configured backend.

The supported teaching target is an Ubuntu 24.04 LTS VM running Nginx on HTTP port 80. The student modifies the enabled default site at `/etc/nginx/sites-available/default` and configures its document root as `/srv/labweaver-nginx-lab`. The supplied `index.html` is the only accepted page identity: title `Nginx Lab`, heading `Nginx Lab`, and `data-lab-id="linux-nginx-v1"`.

Ubuntu 24.04 is the only OS version fixed in this package. An approved release binds the VM image, template and Probe profile; the Probe reports the installed Nginx version. A moving package version must never be represented as a fixed one.

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

The package must use an approved read-only Probe profile that can observe every required fact, including TCP/80 and the HTTP response. Package facts, service facts and file stat alone are insufficient. An unavailable module or observation must fail explicitly; it cannot be replaced with a simulated result or a weaker assertion.

## Material validation

Run the following from the repository root:

```sh
python examples/linux-nginx/verify_material_contract.py --self-test
cargo test -p contracts --locked
```

The Python validator checks public SHA-256 records, HTML identity, candidate submission limits, controlled-material boundary, normal/negative scenario mapping, missing material, altered template, restricted content, and oversized report handling. Contract tests validate the EvaluationSpec format; they do not exercise the VM or Probe.

## Backend integration

Backend tests use a KubeVirt VM with the approved Ubuntu image and Probe profile. Cover the expected target state and both negative cases: stopped/not-listening Nginx and a site/page mismatch. Missing image bindings, unsupported Probe capabilities, access failures, timeouts and malformed facts must leave the run explicitly blocked or failed with the original diagnostic retained.
