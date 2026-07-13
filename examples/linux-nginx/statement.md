# Nginx Lab

## Goal

Configure the Ubuntu 24.04 LTS virtual machine so that Nginx serves the
provided static page over HTTP on port 80. This lab evaluates the observed VM
state; do not replace it with a container, a screenshot, or a locally produced
report.

## Required target state

- Install Nginx from the Ubuntu 24.04 configured package sources. The package
  version is recorded with later runtime evidence; this material package does
  not claim a fixed Debian package version.
- Modify the enabled Ubuntu default site at
  `/etc/nginx/sites-available/default`. Keep that site enabled and set its
  document root to `/srv/labweaver-nginx-lab`.
- Place the supplied `materials/index.html` at
  `/srv/labweaver-nginx-lab/index.html` without changing its required title,
  heading, or `data-lab-id` attribute.
- Enable and start the `nginx` system service. The service must listen on TCP
  port 80 and `http://127.0.0.1/` must return the supplied page.
- Write `report.md` in the submission location defined by the approved
  SubmissionManifest. It may be free-form Markdown but must be no larger than
  64 KiB and must not contain credentials, tokens, private keys, or full logs.

## What is checked

The approved runtime Probe will eventually check host reachability, package and
observed version, default-site configuration and root, service state, TCP/80,
the HTTP response, and the three required HTML markers. The current repository
does not implement that Probe; see the material contract for the blocking
dependency.

## Failure examples

The following are deliberate negative cases, not alternative solutions:

1. Stop `nginx` or make it stop listening on TCP/80. Expected future evidence:
   `LW_LINUX_LAB_NGINX_NOT_LISTENING`.
2. Keep Nginx running but use another root or change a required page marker.
   Expected future evidence: `LW_LINUX_LAB_SITE_MISMATCH`.

`report.md` is advisory-only. A missing report must create the planned
`LW_LINUX_LAB_REPORT_MISSING` advisory diagnostic and manual-review requirement;
it must not change deterministic score, Gate status, or release eligibility.
