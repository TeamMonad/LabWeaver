import assert from 'node:assert/strict';
import test from 'node:test';
import {mkdtemp, rm, writeFile} from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {sha256File} from '../src/paths.js';

test('content tampering changes the evidence hash', async (context) => {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'lw-demo-hash-'));
  context.after(() => rm(dir, {recursive: true, force: true}));
  const file = path.join(dir, 'evidence.bin');
  await writeFile(file, 'before');
  const before = await sha256File(file);
  await writeFile(file, 'after');
  assert.notEqual(await sha256File(file), before);
});
