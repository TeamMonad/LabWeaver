import {readFile, writeFile} from 'node:fs/promises';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import {chromium} from '@playwright/test';
import {DemoVideoError, invariant} from './errors.js';
import {fileEvidence, resolveLocator, sha256File} from './paths.js';
import {probeVideo} from './process.js';
import {assertNoForbiddenText} from './privacy.js';
import {SCENES, TOTAL_SECONDS, VIDEO, type Cut, type FileEvidence} from './model.js';
import {validateManifest} from './schema.js';
import {validateSrt} from './srt.js';

type VerifyOptions = {root: string; cut: Cut; manifestLocator: string};
type Manifest = Record<string, any> & {checksums: FileEvidence[]; video: FileEvidence & Record<string, unknown>; subtitles: Array<FileEvidence & {lastCueSeconds: number}>};

async function verifyPlayback(videoPath: string, screenshotPath: string | null): Promise<void> {
  const browser = await chromium.launch({headless: true});
  try {
    const page = await browser.newPage({viewport: {width: 1280, height: 720}});
    await page.goto(pathToFileURL(videoPath).href, {waitUntil: 'domcontentloaded'});
    const videoLocator = page.locator('video');
    await videoLocator.evaluate(async (element) => {
      const video = element as HTMLVideoElement;
      if (video.readyState < HTMLMediaElement.HAVE_METADATA) {
        await new Promise<void>((resolve, reject) => {
          const timeout = window.setTimeout(() => reject(new Error('video loadedmetadata timed out')), 30_000);
          video.addEventListener('loadedmetadata', () => { window.clearTimeout(timeout); resolve(); }, {once: true});
          video.addEventListener('error', () => { window.clearTimeout(timeout); reject(new Error('video loadedmetadata failed')); }, {once: true});
        });
      }
      for (const point of [0.01, video.duration / 2, Math.max(0, video.duration - 1)]) {
        const seeked = new Promise<void>((resolve, reject) => {
          const timeout = window.setTimeout(() => reject(new Error('video seeked timed out')), 30_000);
          video.addEventListener('seeked', () => { window.clearTimeout(timeout); resolve(); }, {once: true});
          video.addEventListener('error', () => { window.clearTimeout(timeout); reject(new Error('video seeked failed')); }, {once: true});
        });
        video.currentTime = point;
        await seeked;
      }
    });
    if (screenshotPath !== null) await page.screenshot({path: screenshotPath});
  } finally {
    await browser.close();
  }
}

export async function verify(options: VerifyOptions): Promise<string> {
  const manifestPath = resolveLocator(options.root, options.manifestLocator, ['artifacts/demo-video']);
  const manifestText = await readFile(manifestPath, 'utf8');
  const manifest = JSON.parse(manifestText) as Manifest;
  const wasVerified = manifest.status === 'verified';
  await validateManifest(options.root, manifest);
  invariant(manifest.cut === options.cut, 'LW_DEMO_VIDEO_CUT_MISMATCH', `manifest cut is ${manifest.cut}`);
  invariant(new Set((manifest.scenes as Array<{sceneId: string}>).map(({sceneId}) => sceneId)).size === SCENES.length, 'LW_DEMO_VIDEO_SCENE_DUPLICATE', 'scene IDs are duplicated');

  for (const evidence of manifest.checksums) {
    const absolute = resolveLocator(options.root, evidence.path, ['artifacts/demo-video', 'tools/demo-video/captions']);
    invariant(await sha256File(absolute) === evidence.sha256, 'LW_DEMO_VIDEO_HASH_MISMATCH', `hash mismatch: ${evidence.path}`);
  }
  const videoPath = resolveLocator(options.root, manifest.video.path, ['artifacts/demo-video']);
  const probe = await probeVideo(videoPath);
  invariant(probe.codec === 'h264', 'LW_DEMO_VIDEO_CODEC_INVALID', `codec is ${probe.codec}`);
  invariant(probe.width === VIDEO.width && probe.height === VIDEO.height, 'LW_DEMO_VIDEO_RESOLUTION_INVALID', `resolution is ${probe.width}x${probe.height}`);
  invariant(Math.abs(probe.fps - VIDEO.fps) < 0.001, 'LW_DEMO_VIDEO_FPS_INVALID', `fps is ${probe.fps}`);
  invariant(probe.audioStreams === 0, 'LW_DEMO_VIDEO_AUDIO_FORBIDDEN', `found ${probe.audioStreams} audio streams`);
  invariant(probe.durationSeconds >= 810 && probe.durationSeconds <= 870 && Math.abs(probe.durationSeconds - TOTAL_SECONDS) < 0.1, 'LW_DEMO_VIDEO_DURATION_INVALID', `duration is ${probe.durationSeconds}s`);
  for (const subtitle of manifest.subtitles) {
    const cues = await validateSrt(resolveLocator(options.root, subtitle.path, ['tools/demo-video/captions']), probe.durationSeconds);
    invariant(Math.abs(cues.at(-1)!.endSeconds - subtitle.lastCueSeconds) < 0.001, 'LW_DEMO_VIDEO_SRT_MANIFEST_MISMATCH', `subtitle timing differs: ${subtitle.path}`);
  }
  const [subtitleTexts, receiptTexts] = await Promise.all([
    Promise.all(manifest.subtitles.map(({path: locator}) => readFile(resolveLocator(options.root, locator, ['tools/demo-video/captions']), 'utf8'))),
    Promise.all((manifest.scenes as Array<{receipt: FileEvidence}>).map(({receipt}) => readFile(resolveLocator(options.root, receipt.path, ['artifacts/demo-video']), 'utf8'))),
  ]);
  assertNoForbiddenText([manifestText, ...subtitleTexts, ...receiptTexts, ...manifest.checksums.map(({path: value}) => value)]);
  const playbackEvidence = path.join(path.dirname(manifestPath), 'playback-seek-verification.png');
  // Preserve the evidence identity after the first successful rehearsal. A
  // repeated verification still performs every seek, but Chromium screenshots
  // are not byte-deterministic and must not silently rewrite the manifest.
  await verifyPlayback(videoPath, wasVerified ? null : playbackEvidence);
  const playbackFile = await fileEvidence(options.root, playbackEvidence);
  const playbackChecksum = manifest.checksums.findIndex(({path: value}) => value === playbackFile.path);
  if (playbackChecksum >= 0) manifest.checksums[playbackChecksum] = playbackFile;
  else manifest.checksums.push(playbackFile);
  if (options.cut === 'preview') {
    const playbackRehearsal = manifest.rehearsals.find((item: Record<string, unknown>) => item.id === 'fixture-playback');
    if (playbackRehearsal) {
      invariant(wasVerified, 'LW_DEMO_VIDEO_REHEARSAL_DUPLICATE', 'rendered manifest already claims a playback rehearsal');
      playbackRehearsal.evidenceSha256 = playbackFile.sha256;
    } else {
      invariant(!wasVerified, 'LW_DEMO_VIDEO_REHEARSAL_MISSING', 'verified preview is missing its playback rehearsal');
      manifest.rehearsals.push({id: 'fixture-playback', mode: 'playback', status: 'passed', completedAt: new Date().toISOString(), evidenceSha256: playbackFile.sha256});
    }
  } else {
    invariant(manifest.rehearsals.length === 2 && manifest.rehearsals.some((item: Record<string, unknown>) => item.id === 'fixture-playback'), 'LW_DEMO_VIDEO_PREVIEW_REHEARSAL_REQUIRED', 'final verification requires both Fixture rehearsals');
    const reviewPath = path.join(options.root, 'artifacts/demo-video/connected-final/d-verify.json');
    const review = JSON.parse(await readFile(reviewPath, 'utf8')) as Record<string, unknown>;
    const keys = Object.keys(review).sort();
    invariant(JSON.stringify(keys) === JSON.stringify(['completedAt', 'humanPrivacyReview', 'reviewer', 'schemaVersion', 'status', 'videoSha256']), 'LW_DEMO_VIDEO_D_VERIFY_INVALID', 'D Verify receipt has unknown or missing fields');
    invariant(review.schemaVersion === 'demo-video-d-verify.v1' && review.status === 'passed', 'LW_DEMO_VIDEO_D_VERIFY_INVALID', 'D Verify did not pass');
    invariant(review.reviewer === 'Nova-Lciop-J' && review.humanPrivacyReview === 'passed', 'LW_DEMO_VIDEO_D_VERIFY_INVALID', 'D and privacy review are required');
    invariant(review.videoSha256 === manifest.video.sha256, 'LW_DEMO_VIDEO_D_VERIFY_VIDEO_MISMATCH', 'D reviewed a different video');
    invariant(typeof review.completedAt === 'string' && !Number.isNaN(Date.parse(review.completedAt)), 'LW_DEMO_VIDEO_D_VERIFY_INVALID', 'D Verify timestamp is invalid');
    const reviewFile = await fileEvidence(options.root, reviewPath);
    const reviewChecksum = manifest.checksums.findIndex(({path: value}) => value === reviewFile.path);
    if (reviewChecksum >= 0) manifest.checksums[reviewChecksum] = reviewFile;
    else manifest.checksums.push(reviewFile);
    manifest.releaseEligible = true;
    manifest.releaseGate.dVerify = 'passed';
    manifest.privacy.humanReview = 'passed';
    manifest.goNoGo = {outcome: 'go', diagnostic: 'LW_DEMO_VIDEO_RELEASE_VERIFIED'};
    if (!manifest.rehearsals.some((item: Record<string, unknown>) => item.id === 'connected-final-playback')) {
      manifest.rehearsals.push({id: 'connected-final-playback', mode: 'connected-final', status: 'passed', completedAt: review.completedAt, evidenceSha256: manifest.video.sha256});
    }
  }
  manifest.privacy.automatedScan = 'passed';
  manifest.status = 'verified';
  await validateManifest(options.root, manifest);
  await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  return manifestPath;
}
