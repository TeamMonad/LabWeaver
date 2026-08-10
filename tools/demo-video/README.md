# LabWeaver demo video

`tools/demo-video` is the deterministic capture, edit, and verification pipeline for Issue #128. It preserves the product UI as the source of every runtime shot and keeps media under the ignored `artifacts/demo-video/` tree.

## Profiles and evidence boundary

- `fixture-preview` produces an unlisted review cut. Its filename, manifest, and in-video scene label identify it as `Fixture preview / not release evidence`; `releaseEligible` is always `false`.
- `connected-final` accepts only one frozen #126 identity. A Fixture receipt, missing scene, mismatched source commit/Run/digest, or failed Release Gate stops resolution; there is no fallback.
- Capture receipts and manifests contain repository-relative locators and SHA-256 values only. They never contain credentials, terminal transcripts, private payloads, or workstation paths.

## Commands

Start the already-built Web application separately, then run one capture process per scene. Playwright closes its browser context before the receipt reads the recording.

```sh
pnpm --dir tools/demo-video install --frozen-lockfile
pnpm --dir tools/demo-video capture -- --profile fixture-preview --scene opening --base-url http://localhost:4173
pnpm --dir tools/demo-video render -- --cut preview --manifest artifacts/demo-video/preview/demo-video-manifest.v1.json
pnpm --dir tools/demo-video verify -- --cut preview --manifest artifacts/demo-video/preview/demo-video-manifest.v1.json
```

The eight scene IDs are `opening`, `teacher-authoring`, `admin-resource`, `student-container`, `student-kubevirt`, `submission-freeze`, `access-revoke`, and `cleanup`. Capture uses a 1920×1080 light browser viewport and one Chromium worker. It waits only on visible business state; it does not use fixed sleeps.

The rendered deliverable is 1920×1080, 60 fps, H.264, silent, and 14:00 long. Subtitles remain external as `captions/zh-CN.srt` and `captions/en-US.srt`. Rendering requires a hardware H.264 encoder, uses a fixed 12 Mbit/s video bitrate, and limits Chromium frame generation to one worker so capture/render does not starve the local Kubernetes node. Remotion is configured with `hardwareAcceleration: required`, so an unavailable or incompatible encoder is a blocking error rather than a silent software fallback. Source clips use the pinned `@remotion/media` `Video` component with `onError=fail`; this avoids `OffthreadVideo` frame-cache gaps in sparse browser recordings and explicitly forbids its fallback path. The complete renderer profile participates in every scene chunk's input hash; chunks produced with different worker, media-component, or CPU/GPU settings cannot be reused together.

## Docker Desktop Fixture environment

The local cluster command owns only the explicitly authorized `labweaver-local-demo` namespace and `labweaver-fixture-demo` Helm release. It requires the active Kubernetes context to be exactly `docker-desktop`, builds the dedicated Fixture image from a clean commit, deploys it atomically, verifies the Pod security contract and Fixture banner, and records a schema-validated non-release report. It never deploys the application services, KubeVirt/CDI, or #126 connected evidence.

```sh
pnpm --dir tools/demo-video local-cluster -- --action deploy
pnpm --dir tools/demo-video local-cluster -- --action verify
pnpm --dir tools/demo-video local-cluster -- --action demo
pnpm --dir tools/demo-video local-cluster -- --action teardown
```

`demo` refreshes the deterministic role storage states, opens a loopback-only port-forward, captures all eight scenes sequentially with one Chromium worker, renders the GPU-required preview, and verifies the resulting manifest. The port-forward closes after the command; the Helm release remains running until the explicit `teardown` action. KubeVirt/CDI absence is recorded as a capability limit and the VM scene remains visibly Fixture evidence.

## Final resolver

The final cut is intentionally unavailable until #126 supplies all eight connected receipts and the exact frozen identity. Place the schema-valid passing Gate report at `artifacts/demo-video/connected-final/release-gate-report.v3.json`; the resolver rehashes it and compares source, Run, deployment, migration, image and runtime-artifact identities before rendering. The same command and scene IDs replace the Fixture shots in place. The final resolver rejects any mixed profile or identity before rendering.

A rendered final remains `releaseEligible: false` with `LW_DEMO_VIDEO_D_VERIFY_PENDING`. After D reviews that exact MP4 frame by frame, `artifacts/demo-video/connected-final/d-verify.json` must contain exactly `schemaVersion`, `status`, `reviewer`, `humanPrivacyReview`, `completedAt`, and the reviewed `videoSha256`. `verify --cut final` rechecks media, subtitles, all hashes and seek playback before it can record the third rehearsal and change the conclusion to Go. There is no Fixture or reviewer fallback.

Media remains only in the local ignored working directory. The manifest provides per-file checksums, but no CI upload, GitHub Release asset, remote backup, or portable archive is created.
