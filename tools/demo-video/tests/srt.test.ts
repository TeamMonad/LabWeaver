import assert from 'node:assert/strict';
import test from 'node:test';
import {parseSrt} from '../src/srt.js';
import {validateSrt} from '../src/srt.js';
import {mkdtemp, rm, writeFile} from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';

test('SRT parser accepts ordered bilingual-style cues', () => {
  const cues = parseSrt('1\n00:00:00,000 --> 00:00:01,000\nhello\n\n2\n00:00:01,500 --> 00:00:02,000\nworld\n');
  assert.equal(cues.length, 2);
  assert.equal(cues[1]!.startSeconds, 1.5);
});

test('SRT parser rejects malformed timestamps', () => {
  assert.throws(() => parseSrt('1\n00:00 --> 00:01\nbad\n'), /SRT_TIMESTAMP_INVALID/);
});

test('SRT validation rejects a cue beyond the video duration', async (context) => {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'lw-demo-srt-'));
  context.after(() => rm(dir, {recursive: true, force: true}));
  const file = path.join(dir, 'bad.srt');
  await writeFile(file, '1\n00:00:09,000 --> 00:00:11,000\ntoo late\n');
  await assert.rejects(validateSrt(file, 10), /SRT_OUT_OF_RANGE/);
});
