import {createHash} from 'node:crypto';
import {mkdir, readFile, writeFile} from 'node:fs/promises';
import path from 'node:path';
import {bundle} from '@remotion/bundler';
import {renderMedia, selectComposition} from '@remotion/renderer';
import {DemoVideoError, invariant} from './errors.js';
import {fileEvidence, resolveLocator, sha256File, toLocator} from './paths.js';
import {probeVideo, run} from './process.js';
import {SCENES, TOTAL_SECONDS, VIDEO, type CaptureReceipt, type Cut} from './model.js';
import {validateManifest, validateReceipt, validateReleaseGate} from './schema.js';
import {validateSrt} from './srt.js';
import type {DemoVideoProps, RenderScene} from './composition.js';

type RenderOptions = {root: string; cut: Cut; manifestLocator: string};

export const RENDERER_PROFILE = {
  codec: 'h264',
  hardwareAcceleration: 'required',
  videoBitrate: '12M',
  pixelFormat: 'yuv420p',
  concurrency: 1,
} as const;

async function loadReceipts(root: string, cut: Cut): Promise<Array<{receipt: CaptureReceipt; receiptPath: string}>> {
  const profile = cut === 'preview' ? 'fixture-preview' : 'connected-final';
  return await Promise.all(SCENES.map(async (scene) => {
    const receiptPath = path.join(root, 'artifacts/demo-video', profile, 'captures', scene.id, 'capture-receipt.v1.json');
    const receipt = JSON.parse(await readFile(receiptPath, 'utf8')) as CaptureReceipt;
    await validateReceipt(root, receipt);
    invariant(receipt.sceneId === scene.id && receipt.profile === profile, 'LW_DEMO_VIDEO_SCENE_RECEIPT_MISMATCH', `receipt mismatch for ${scene.id}`);
    return {receipt, receiptPath};
  }));
}

export function assertIdentity(receipts: CaptureReceipt[], cut: Cut): void {
  const first = receipts[0]!;
  const duplicates = new Set(receipts.map(({sceneId}) => sceneId));
  invariant(duplicates.size === SCENES.length, 'LW_DEMO_VIDEO_SCENE_DUPLICATE', 'scene IDs must be unique');
  for (const receipt of receipts) {
    invariant(receipt.sourceCommit === first.sourceCommit, 'LW_DEMO_VIDEO_COMMIT_MISMATCH', 'all scenes must share one source commit');
    invariant(receipt.runId === first.runId, 'LW_DEMO_VIDEO_RUN_MISMATCH', 'all scenes must share one Run ID');
    invariant(JSON.stringify(receipt.identity) === JSON.stringify(first.identity), 'LW_DEMO_VIDEO_IDENTITY_MISMATCH', 'all scenes must share one frozen identity');
  }
  if (cut === 'final') invariant(first.identity && first.runId, 'LW_DEMO_VIDEO_CONNECTED_EVIDENCE_REQUIRED', 'final cut requires connected identity');
  else invariant(first.identity === null && first.runId === null, 'LW_DEMO_VIDEO_FIXTURE_CONTAMINATED', 'preview cannot contain connected identity');
}

export function assertReleaseGateIdentity(gate: any, receipt: CaptureReceipt): void {
  const identity = receipt.identity!;
  invariant(gate.sourceCommit === receipt.sourceCommit && gate.runId === receipt.runId, 'LW_DEMO_VIDEO_RELEASE_GATE_IDENTITY_MISMATCH', 'Release Gate source or Run differs from captures');
  invariant(gate.deploymentManifest.sha256 === identity.deploymentSha256, 'LW_DEMO_VIDEO_RELEASE_GATE_IDENTITY_MISMATCH', 'deployment digest differs');
  invariant(gate.migrationCatalog.sha256 === identity.migrationCatalogSha256, 'LW_DEMO_VIDEO_RELEASE_GATE_IDENTITY_MISMATCH', 'migration digest differs');
  const imageDigests = [...gate.platformImages, ...gate.resourceImages].map((image: {reference: string}) => `sha256:${image.reference.split('@sha256:')[1]}`);
  invariant(JSON.stringify([...imageDigests].sort()) === JSON.stringify([...identity.imageDigests].sort()), 'LW_DEMO_VIDEO_RELEASE_GATE_IDENTITY_MISMATCH', 'image digest set differs');
  invariant(JSON.stringify(gate.runtimeArtifacts.map((artifact: {digest: string}) => artifact.digest).sort()) === JSON.stringify([...identity.runtimeArtifacts].sort()), 'LW_DEMO_VIDEO_RELEASE_GATE_IDENTITY_MISMATCH', 'runtime artifact set differs');
}

export async function render(options: RenderOptions): Promise<string> {
  const profile = options.cut === 'preview' ? 'fixture-preview' : 'connected-final';
  const loaded = await loadReceipts(options.root, options.cut);
  const receipts = loaded.map(({receipt}) => receipt);
  assertIdentity(receipts, options.cut);
  const outputDir = path.join(options.root, 'artifacts/demo-video', options.cut);
  await mkdir(outputDir, {recursive: true});
  const videoPath = path.join(outputDir, options.cut === 'preview'
    ? 'labweaver-fixture-preview-not-release-evidence.mp4'
    : 'labweaver-connected-final.mp4');
  const inputProps: DemoVideoProps = {
    cut: options.cut,
    scenes: receipts.map((receipt, index): RenderScene => {
      const scene = SCENES[index]!;
      return {
        sceneId: scene.id, label: scene.label,
        clip: receipt.clip.path.replace(/^artifacts\/demo-video\//, ''),
        durationInFrames: scene.seconds * VIDEO.fps,
        sourceFrames: Math.max(1, Math.floor(receipt.clip.durationSeconds * VIDEO.fps)),
      };
    }),
  };
  const serveUrl = await bundle({
    entryPoint: path.join(options.root, 'tools/demo-video/src/remotion-entry.tsx'),
    publicDir: path.join(options.root, 'artifacts/demo-video'),
    webpackOverride: (configuration) => configuration,
  });
  const composition = await selectComposition({serveUrl, id: 'LabWeaverDemoVideo', inputProps});
  const chunkDir = path.join(outputDir, 'chunks');
  await mkdir(chunkDir, {recursive: true});
  const chunkPaths: string[] = [];
  let fromFrame = 0;
  for (const [index, scene] of SCENES.entries()) {
    const chunkPath = path.join(chunkDir, `${String(index + 1).padStart(2, '0')}-${scene.id}.mp4`);
    const provenancePath = `${chunkPath}.input-sha256`;
    const inputSha256 = createHash('sha256').update(JSON.stringify({
      cut: options.cut, sceneId: scene.id, fromFrame, durationInFrames: scene.seconds * VIDEO.fps,
      receiptSha256: await sha256File(loaded[index]!.receiptPath),
      nextReceiptSha256: loaded[index + 1] ? await sha256File(loaded[index + 1]!.receiptPath) : null,
      sceneInput: inputProps.scenes[index], nextSceneInput: inputProps.scenes[index + 1] ?? null,
      timeline: SCENES.map(({id, seconds, label}) => ({id, seconds, label})),
      rendererProfile: RENDERER_PROFILE,
    })).digest('hex');
    let reusable = false;
    try {
      const [saved, chunkProbe] = await Promise.all([readFile(provenancePath, 'utf8'), probeVideo(chunkPath)]);
      reusable = saved.trim() === inputSha256
        && chunkProbe.codec === 'h264' && chunkProbe.width === VIDEO.width && chunkProbe.height === VIDEO.height
        && Math.abs(chunkProbe.fps - VIDEO.fps) < 0.001 && chunkProbe.audioStreams === 0
        && Math.abs(chunkProbe.durationSeconds - scene.seconds) < 0.1;
    } catch (error) {
      reusable = false;
      process.stderr.write(`${JSON.stringify({event: 'demo_video_scene_cache_invalid', cut: options.cut, sceneId: scene.id, diagnostic: error instanceof Error ? error.message : String(error)})}\n`);
    }
    if (!reusable) {
      let reportedProgress = -1;
      await renderMedia({
        serveUrl, composition, codec: RENDERER_PROFILE.codec, outputLocation: chunkPath, inputProps,
        frameRange: [fromFrame, fromFrame + scene.seconds * VIDEO.fps - 1],
        muted: true, audioCodec: null, crf: null,
        hardwareAcceleration: RENDERER_PROFILE.hardwareAcceleration,
        videoBitrate: RENDERER_PROFILE.videoBitrate,
        pixelFormat: RENDERER_PROFILE.pixelFormat,
        concurrency: RENDERER_PROFILE.concurrency,
        onProgress: ({progress}) => {
          const bucket = Math.floor(progress * 10);
          if (bucket > reportedProgress) {
            reportedProgress = bucket;
            process.stderr.write(`${JSON.stringify({event: 'demo_video_scene_render_progress', cut: options.cut, sceneId: scene.id, progress: Number(progress.toFixed(3))})}\n`);
          }
        },
      });
      await writeFile(provenancePath, `${inputSha256}\n`);
    } else {
      process.stderr.write(`${JSON.stringify({event: 'demo_video_scene_reused', cut: options.cut, sceneId: scene.id})}\n`);
    }
    chunkPaths.push(chunkPath);
    fromFrame += scene.seconds * VIDEO.fps;
  }
  const concatPath = path.join(chunkDir, 'concat.txt');
  await writeFile(concatPath, chunkPaths.map((chunk) => `file '${path.basename(chunk).replaceAll("'", "'\\''")}'`).join('\n') + '\n');
  await run('ffmpeg', ['-y', '-f', 'concat', '-safe', '0', '-i', concatPath, '-c', 'copy', '-movflags', '+faststart', videoPath], 'LW_DEMO_VIDEO_CONCAT_FAILED');
  const probe = await probeVideo(videoPath);
  invariant(probe.durationSeconds >= 810 && probe.durationSeconds <= 870, 'LW_DEMO_VIDEO_DURATION_INVALID', `duration is ${probe.durationSeconds}s`);

  const subtitleEntries = await Promise.all((['zh-CN', 'en-US'] as const).map(async (language) => {
    const subtitlePath = path.join(options.root, `tools/demo-video/captions/${language}.srt`);
    const cues = await validateSrt(subtitlePath, probe.durationSeconds);
    return {...await fileEvidence(options.root, subtitlePath), language, format: 'srt', lastCueSeconds: cues.at(-1)!.endSeconds};
  }));
  const sceneEntries = await Promise.all(loaded.map(async ({receipt, receiptPath}, index) => {
    const scene = SCENES[index]!;
    return {
      sceneId: scene.id, role: scene.role, profile,
      receipt: await fileEvidence(options.root, receiptPath),
      clip: await fileEvidence(options.root, resolveLocator(options.root, receipt.clip.path, ['artifacts/demo-video'])),
      screenshot: await fileEvidence(options.root, resolveLocator(options.root, receipt.screenshot.path, ['artifacts/demo-video'])),
      fromFrame: SCENES.slice(0, index).reduce((sum, item) => sum + item.seconds * VIDEO.fps, 0),
      durationInFrames: scene.seconds * VIDEO.fps,
    };
  }));
  const checksums = [await fileEvidence(options.root, videoPath), ...subtitleEntries.map(({path: p, sha256, bytes}) => ({path: p, sha256, bytes}))];
  for (const [index, entry] of sceneEntries.entries()) {
    const trace = loaded[index]!.receipt.trace;
    checksums.push(
      entry.receipt,
      entry.clip,
      entry.screenshot,
      await fileEvidence(options.root, resolveLocator(options.root, trace.path, ['artifacts/demo-video'])),
    );
  }
  const now = new Date().toISOString();
  let releaseGate = null;
  let rehearsals: Array<Record<string, unknown>> = [
    {id: 'fixture-flow', mode: 'fixture', status: 'passed', completedAt: now, evidenceSha256: checksums[0]!.sha256},
  ];
  if (options.cut === 'final') {
    const previewManifestPath = path.join(options.root, 'artifacts/demo-video/preview/demo-video-manifest.v1.json');
    const previewManifest = JSON.parse(await readFile(previewManifestPath, 'utf8')) as Record<string, any>;
    await validateManifest(options.root, previewManifest);
    invariant(previewManifest.cut === 'preview' && previewManifest.status === 'verified', 'LW_DEMO_VIDEO_PREVIEW_REHEARSAL_REQUIRED', 'final render requires a verified Fixture preview manifest');
    invariant(previewManifest.rehearsals.length === 2, 'LW_DEMO_VIDEO_PREVIEW_REHEARSAL_REQUIRED', 'both Fixture rehearsals must pass before final render');
    rehearsals = previewManifest.rehearsals.map((rehearsal: Record<string, unknown>) => ({...rehearsal}));
    checksums.push(await fileEvidence(options.root, previewManifestPath));
    const gatePath = path.join(options.root, 'artifacts/demo-video/connected-final/release-gate-report.v3.json');
    const gate = JSON.parse(await readFile(gatePath, 'utf8')) as any;
    await validateReleaseGate(options.root, gate);
    assertReleaseGateIdentity(gate, receipts[0]!);
    const evidence = await fileEvidence(options.root, gatePath);
    checksums.push(evidence);
    releaseGate = {...evidence, status: 'passed', sourceCommit: gate.sourceCommit, runId: gate.runId, dVerify: 'pending'};
  }
  const manifest = {
    schemaVersion: 'demo-video-manifest.v1', status: 'rendered', cut: options.cut,
    releaseEligible: false, sourceCommit: receipts[0]!.sourceCommit,
    runId: receipts[0]!.runId, identity: receipts[0]!.identity, createdAt: now,
    video: {...checksums[0], width: probe.width, height: probe.height, fps: probe.fps, codec: probe.codec, audioStreams: probe.audioStreams, durationSeconds: probe.durationSeconds},
    subtitles: subtitleEntries, scenes: sceneEntries,
    rehearsals,
    releaseGate,
    privacy: {automatedScan: 'pending', humanReview: 'pending', containsSecrets: false, containsRawUserContent: false, containsTerminalTranscript: false, containsAbsolutePaths: false},
    knownLimits: options.cut === 'preview' ? [
      'Fixture preview / not release evidence; all runtime shots must be replaced by one #126 connected identity.',
      'Local working directory is the only media archive; no remote backup or portable archive exists.',
    ] : ['Local working directory is the only media archive; no remote backup or portable archive exists.'],
    goNoGo: {outcome: 'blocked', diagnostic: options.cut === 'preview' ? 'LW_DEMO_VIDEO_CONNECTED_EVIDENCE_PENDING' : 'LW_DEMO_VIDEO_D_VERIFY_PENDING'}, checksums,
  };
  await validateManifest(options.root, manifest);
  const manifestPath = resolveLocator(options.root, options.manifestLocator, ['artifacts/demo-video']);
  await mkdir(path.dirname(manifestPath), {recursive: true});
  await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  return toLocator(options.root, manifestPath);
}
