import assert from 'node:assert/strict';
import test from 'node:test';
import {assertNoForbiddenText} from '../src/privacy.js';

test('privacy scanner accepts safe relative evidence metadata', () => {
  assert.doesNotThrow(() => assertNoForbiddenText(['artifacts/demo-video/preview/video.mp4', 'Fixture preview']));
});

test('privacy scanner rejects credentials, private domains, keys, and absolute user paths', () => {
  for (const value of [
    'token=abc123',
    'https://control.internal/api',
    'C:\\Users\\operator\\capture.webm',
    '/home/operator/capture.webm',
    '-----BEGIN PRIVATE KEY-----',
  ]) {
    assert.throws(() => assertNoForbiddenText([value]), /LW_DEMO_VIDEO_PRIVACY_SCAN_FAILED/);
  }
});
