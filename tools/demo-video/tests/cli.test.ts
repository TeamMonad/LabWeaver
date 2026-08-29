import assert from 'node:assert/strict';
import test from 'node:test';
import {argumentsMap} from '../src/cli.js';

test('CLI argument parser accepts an optional separator and complete key/value pairs', () => {
  assert.deepEqual([...argumentsMap(['--', '--cut', 'preview', '--manifest', 'artifacts/demo-video/preview/manifest.json'])], [
    ['cut', 'preview'],
    ['manifest', 'artifacts/demo-video/preview/manifest.json'],
  ]);
});

test('CLI argument parser rejects missing values and duplicate keys', () => {
  assert.throws(() => argumentsMap(['--cut']), /LW_DEMO_VIDEO_ARGUMENT_INVALID/);
  assert.throws(() => argumentsMap(['--cut', 'preview', '--cut', 'final']), /LW_DEMO_VIDEO_ARGUMENT_DUPLICATE/);
});
