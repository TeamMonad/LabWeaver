import {createHash} from 'node:crypto';
import {existsSync, realpathSync, statSync} from 'node:fs';
import {readFile, stat} from 'node:fs/promises';
import path from 'node:path';
import {DemoVideoError, invariant} from './errors.js';
import type {FileEvidence} from './model.js';

const REPO_MARKER = path.join('tools', 'demo-video', 'package.json');

export function repositoryRoot(start = process.cwd()): string {
  let cursor = path.resolve(start);
  while (true) {
    try {
      if (requireExists(path.join(cursor, REPO_MARKER))) return cursor;
    } catch { /* handled below */ }
    const parent = path.dirname(cursor);
    if (parent === cursor) throw new DemoVideoError('LW_DEMO_VIDEO_REPO_NOT_FOUND', 'run inside the LabWeaver repository');
    cursor = parent;
  }
}

function requireExists(candidate: string): boolean {
  try {
    return Boolean(statSync(candidate));
  } catch {
    return false;
  }
}

export function resolveLocator(root: string, locator: string, allowedPrefixes: string[]): string {
  invariant(locator.length > 0 && !path.isAbsolute(locator), 'LW_DEMO_VIDEO_PATH_INVALID', 'locator must be repository-relative');
  const normalized = locator.replaceAll('\\', '/');
  invariant(!normalized.split('/').includes('..'), 'LW_DEMO_VIDEO_PATH_ESCAPE', `locator escapes repository: ${locator}`);
  invariant(allowedPrefixes.some((prefix) => normalized === prefix || normalized.startsWith(`${prefix}/`)), 'LW_DEMO_VIDEO_PATH_SCOPE', `locator is outside the allowed scope: ${locator}`);
  const absolute = path.resolve(root, normalized);
  invariant(absolute.startsWith(`${path.resolve(root)}${path.sep}`), 'LW_DEMO_VIDEO_PATH_ESCAPE', `locator escapes repository: ${locator}`);
  const realRoot = realpathSync.native(root);
  let existingAncestor = absolute;
  while (!existsSync(existingAncestor)) {
    const parent = path.dirname(existingAncestor);
    invariant(parent !== existingAncestor, 'LW_DEMO_VIDEO_PATH_ESCAPE', `locator has no repository ancestor: ${locator}`);
    existingAncestor = parent;
  }
  const realAncestor = realpathSync.native(existingAncestor);
  invariant(realAncestor === realRoot || realAncestor.startsWith(`${realRoot}${path.sep}`), 'LW_DEMO_VIDEO_PATH_ESCAPE', `locator resolves outside repository: ${locator}`);
  return absolute;
}

export function toLocator(root: string, absolute: string): string {
  const locator = path.relative(root, absolute).replaceAll('\\', '/');
  invariant(locator && !locator.startsWith('../') && locator !== '..', 'LW_DEMO_VIDEO_PATH_ESCAPE', 'file is outside repository');
  return locator;
}

export async function sha256File(absolute: string): Promise<string> {
  const bytes = await readFile(absolute);
  return `sha256:${createHash('sha256').update(bytes).digest('hex')}`;
}

export async function fileEvidence(root: string, absolute: string): Promise<FileEvidence> {
  const metadata = await stat(absolute);
  invariant(metadata.isFile() && metadata.size > 0, 'LW_DEMO_VIDEO_FILE_EMPTY', `evidence file is empty: ${toLocator(root, absolute)}`);
  return {path: toLocator(root, absolute), sha256: await sha256File(absolute), bytes: metadata.size};
}
