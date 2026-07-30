# Fixture Console Preview

Issue #143 provides a deterministic, browser-only preview for reviewing the
xterm and noVNC console surfaces without starting a backend. It reuses the EX3
Fixture adapter and is deliberately separate from live runtime evidence.

## Start

From the repository root, run:

```sh
pnpm --dir web preview:console:fixture
```

Open `http://localhost:4173/student/environments`. Create a Container or VM
environment and issue an access grant to expose its eligible console surface.

- The Container surface uses the deterministic in-memory terminal and labels it
  as having no real runtime attached.
- The VM surface never fabricates an RFB stream. Its deterministic unavailable
  scenario reports `CONSOLE_UPSTREAM_UNAVAILABLE`.

The visible `FIXTURE MODE` banner is mandatory. Fixture mode is enabled only by
the explicit build command above; production and live builds cannot use it as a
fallback.

## Evidence boundary

The preview proves layout, accessibility, and frontend state transitions only.
It does not prove Access proxy admission, one-time handoff consumption, a
Container exec session, KubeVirt VNC, revocation delivery, or Release Gate
readiness. Those require the separately identified connected-runtime evidence.

## Regression replay

```sh
pnpm --dir web test:e2e:fixture
```

This runs the same Fixture browser path and retains normal Playwright failure
artifacts. It is not an alternative to `pnpm --dir web test:e2e:live`.
