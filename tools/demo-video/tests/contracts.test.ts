import assert from 'node:assert/strict';
import test from 'node:test';
import path from 'node:path';
import {repositoryRoot, resolveLocator} from '../src/paths.js';
import {SCENES, VIDEO} from '../src/model.js';
import {validateManifest, validateReceipt} from '../src/schema.js';
import {assertIdentity, assertReleaseGateIdentity} from '../src/render.js';

const root = repositoryRoot();
const digest = (value: string) => `sha256:${value.repeat(64).slice(0, 64)}`;
const evidence = (path: string, value = 'a') => ({path, sha256: digest(value), bytes: 1});

function receipt() {
  return {
    schemaVersion: 'demo-video-capture-receipt.v1', sceneId: 'opening', role: 'system',
    profile: 'fixture-preview', releaseEligible: false, sourceCommit: 'a'.repeat(40), runId: null, identity: null,
    clip: {...evidence('artifacts/demo-video/fixture-preview/captures/opening/scene.webm'), durationSeconds: 2},
    trace: evidence('artifacts/demo-video/fixture-preview/captures/opening/trace.zip', 'b'),
    screenshot: evidence('artifacts/demo-video/fixture-preview/captures/opening/final.png', 'c'),
    browser: {name: 'chromium', version: '1', playwrightVersion: '1.61.1'},
    viewport: {width: 1920, height: 1080}, capturedAt: '2026-08-09T00:00:00Z',
    privacy: {automatedScan: 'passed', humanReview: 'pending', containsSecrets: false, containsRawUserContent: false, containsTerminalTranscript: false, containsAbsolutePaths: false},
  };
}

function manifest() {
  const checksums = Array.from({length: 11}, (_, index) => evidence(`artifacts/demo-video/preview/file-${index}.bin`, (index + 1).toString(16)));
  return {
    schemaVersion: 'demo-video-manifest.v1', status: 'verified', cut: 'preview', releaseEligible: false,
    sourceCommit: 'a'.repeat(40), runId: null, identity: null, createdAt: '2026-08-09T00:00:00Z',
    video: {...checksums[0], width: VIDEO.width, height: VIDEO.height, fps: VIDEO.fps, codec: 'h264', audioStreams: 0, durationSeconds: 840},
    subtitles: [
      {...evidence('tools/demo-video/captions/zh-CN.srt', 'c'), language: 'zh-CN', format: 'srt', lastCueSeconds: 839.5},
      {...evidence('tools/demo-video/captions/en-US.srt', 'd'), language: 'en-US', format: 'srt', lastCueSeconds: 839.5},
    ],
    scenes: SCENES.map((scene, index) => ({
      sceneId: scene.id, role: scene.role, profile: 'fixture-preview',
      receipt: evidence(`artifacts/demo-video/fixture-preview/captures/${scene.id}/receipt.json`, 'e'),
      clip: evidence(`artifacts/demo-video/fixture-preview/captures/${scene.id}/scene.webm`, 'f'),
      screenshot: evidence(`artifacts/demo-video/fixture-preview/captures/${scene.id}/final.png`, '1'),
      fromFrame: SCENES.slice(0, index).reduce((sum, item) => sum + item.seconds * 60, 0),
      durationInFrames: scene.seconds * 60,
    })),
    rehearsals: [
      {id: 'fixture-flow', mode: 'fixture', status: 'passed', completedAt: '2026-08-09T00:00:00Z', evidenceSha256: digest('2')},
      {id: 'fixture-playback', mode: 'playback', status: 'passed', completedAt: '2026-08-09T00:00:00Z', evidenceSha256: digest('3')},
    ],
    releaseGate: null,
    privacy: {automatedScan: 'passed', humanReview: 'pending', containsSecrets: false, containsRawUserContent: false, containsTerminalTranscript: false, containsAbsolutePaths: false},
    knownLimits: ['Fixture preview / not release evidence.'],
    goNoGo: {outcome: 'blocked', diagnostic: 'LW_DEMO_VIDEO_CONNECTED_EVIDENCE_PENDING'}, checksums,
  };
}

test('capture receipt rejects unknown fields and Fixture release eligibility', async () => {
  await validateReceipt(root, receipt());
  await assert.rejects(validateReceipt(root, {...receipt(), unexpected: true}), /RECEIPT_INVALID/);
  await assert.rejects(validateReceipt(root, {...receipt(), releaseEligible: true}), /RECEIPT_INVALID/);
  await assert.rejects(validateReceipt(root, {...receipt(), profile: 'fixture'}), /RECEIPT_INVALID/);
});

test('resolver rejects duplicate scenes, cross-commit and cross-Run receipts', () => {
  const receipts = SCENES.map((scene) => ({...receipt(), sceneId: scene.id, role: scene.role})) as any[];
  assert.doesNotThrow(() => assertIdentity(receipts, 'preview'));
  const duplicate = structuredClone(receipts); duplicate[1].sceneId = duplicate[0].sceneId;
  assert.throws(() => assertIdentity(duplicate, 'preview'), /SCENE_DUPLICATE/);
  const commit = structuredClone(receipts); commit[1].sourceCommit = 'b'.repeat(40);
  assert.throws(() => assertIdentity(commit, 'preview'), /COMMIT_MISMATCH/);
  const connected = structuredClone(receipts);
  connected.forEach((item: any) => { item.profile = 'connected-final'; item.runId = 'b69528e1-6a2c-4ab1-8617-7fd20db2925d'; item.identity = {}; });
  connected[2].runId = '87681c31-e16a-4b19-b17a-b33a92ab9cf5';
  assert.throws(() => assertIdentity(connected, 'final'), /RUN_MISMATCH/);
});

test('final resolver rejects a Release Gate digest from another identity', () => {
  const id = digest('a');
  const finalReceipt: any = {
    ...receipt(), profile: 'connected-final', releaseEligible: true,
    runId: 'b69528e1-6a2c-4ab1-8617-7fd20db2925d',
    identity: {packageSha256: id, configurationSha256: id, migrationCatalogSha256: id, deploymentSha256: id, imageDigests: [id], runtimeArtifacts: [id]},
  };
  const gate = {
    sourceCommit: finalReceipt.sourceCommit, runId: finalReceipt.runId,
    deploymentManifest: {sha256: digest('b')}, migrationCatalog: {sha256: id},
    platformImages: [], resourceImages: [], runtimeArtifacts: [],
  };
  assert.throws(() => assertReleaseGateIdentity(gate, finalReceipt), /RELEASE_GATE_IDENTITY_MISMATCH/);
});

test('manifest rejects missing scenes, duplicate IDs, duration overflow, and Fixture contamination', async () => {
  await validateManifest(root, manifest());
  const rendered = manifest(); rendered.status = 'rendered'; rendered.rehearsals.pop(); rendered.privacy.automatedScan = 'pending';
  await validateManifest(root, rendered);
  const prematurePlayback = manifest(); prematurePlayback.status = 'rendered'; prematurePlayback.privacy.automatedScan = 'pending';
  await assert.rejects(validateManifest(root, prematurePlayback), /MANIFEST_INVALID/);
  const missingPlayback = manifest(); missingPlayback.rehearsals.pop();
  await assert.rejects(validateManifest(root, missingPlayback), /MANIFEST_INVALID/);
  const missing = manifest(); missing.scenes.pop();
  await assert.rejects(validateManifest(root, missing), /MANIFEST_INVALID/);
  const duplicate = manifest(); duplicate.scenes[1]!.sceneId = duplicate.scenes[0]!.sceneId;
  await assert.rejects(validateManifest(root, duplicate), /MANIFEST_INVALID/);
  const duplicateChecksum = manifest(); duplicateChecksum.checksums[1]!.path = duplicateChecksum.checksums[0]!.path;
  await assert.rejects(validateManifest(root, duplicateChecksum), /MANIFEST_INVALID/);
  const long = manifest(); long.video.durationSeconds = 871;
  await assert.rejects(validateManifest(root, long), /MANIFEST_INVALID/);
  const mixed = manifest(); mixed.scenes[0]!.profile = 'connected-final';
  await assert.rejects(validateManifest(root, mixed), /MANIFEST_INVALID/);
});

test('repository locator rejects absolute paths, traversal, and unrelated trees', () => {
  assert.throws(() => resolveLocator(root, '../secret', ['artifacts/demo-video']), /PATH_ESCAPE/);
  assert.throws(() => resolveLocator(root, 'docs/secret', ['artifacts/demo-video']), /PATH_SCOPE/);
  assert.throws(() => resolveLocator(root, path.resolve(root, 'secret'), ['artifacts/demo-video']), /PATH_INVALID/);
});
