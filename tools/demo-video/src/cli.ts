import {capture} from './capture.js';
import {pathToFileURL} from 'node:url';
import {DemoVideoError, invariant} from './errors.js';
import {repositoryRoot} from './paths.js';
import {render} from './render.js';
import {SCENE_IDS, type Cut, type Profile, type SceneId} from './model.js';
import {verify} from './verify.js';
import {localCluster} from './local-cluster.js';

export function argumentsMap(values: string[]): Map<string, string> {
  const result = new Map<string, string>();
  if (values[0] === '--') values = values.slice(1);
  for (let index = 0; index < values.length; index += 2) {
    const key = values[index];
    const value = values[index + 1];
    invariant(key?.startsWith('--') && value && !value.startsWith('--'), 'LW_DEMO_VIDEO_ARGUMENT_INVALID', `expected --key value near ${key ?? '<end>'}`);
    invariant(!result.has(key!.slice(2)), 'LW_DEMO_VIDEO_ARGUMENT_DUPLICATE', `argument ${key} was provided more than once`);
    result.set(key!.slice(2), value);
  }
  return result;
}

async function main(): Promise<void> {
  const [command, ...values] = process.argv.slice(2);
  const args = argumentsMap(values);
  const root = repositoryRoot();
  if (command === 'capture') {
    for (const key of args.keys()) invariant(['profile', 'scene', 'base-url', 'identity'].includes(key), 'LW_DEMO_VIDEO_ARGUMENT_UNKNOWN', `unknown capture argument: --${key}`);
    const profile = args.get('profile') as Profile;
    const sceneId = args.get('scene') as SceneId;
    invariant(profile === 'fixture-preview' || profile === 'connected-final', 'LW_DEMO_VIDEO_PROFILE_INVALID', `invalid profile: ${profile}`);
    invariant(SCENE_IDS.has(sceneId), 'LW_DEMO_VIDEO_SCENE_UNKNOWN', `unknown scene: ${sceneId}`);
    console.log(await capture({root, profile, sceneId, baseUrl: args.get('base-url') ?? 'http://localhost:4173', ...(args.has('identity') ? {identityLocator: args.get('identity')!} : {})}));
    return;
  }
  if (command === 'render' || command === 'verify') {
    for (const key of args.keys()) invariant(['cut', 'manifest'].includes(key), 'LW_DEMO_VIDEO_ARGUMENT_UNKNOWN', `unknown ${command} argument: --${key}`);
    const cut = args.get('cut') as Cut;
    const manifestLocator = args.get('manifest');
    invariant(cut === 'preview' || cut === 'final', 'LW_DEMO_VIDEO_CUT_INVALID', `invalid cut: ${cut}`);
    invariant(manifestLocator, 'LW_DEMO_VIDEO_MANIFEST_REQUIRED', '--manifest is required');
    console.log(command === 'render' ? await render({root, cut, manifestLocator}) : await verify({root, cut, manifestLocator}));
    return;
  }
  if (command === 'local-cluster') {
    for (const key of args.keys()) invariant(key === 'action', 'LW_DEMO_VIDEO_ARGUMENT_UNKNOWN', `unknown local-cluster argument: --${key}`);
    const action = args.get('action');
    invariant(action === 'deploy' || action === 'verify' || action === 'demo' || action === 'teardown',
      'LW_DEMO_VIDEO_LOCAL_ACTION_INVALID', `invalid local action: ${action ?? '<missing>'}`);
    console.log(await localCluster(root, action));
    return;
  }
  throw new DemoVideoError('LW_DEMO_VIDEO_COMMAND_INVALID', `expected capture, render, verify, or local-cluster; received ${command ?? '<none>'}`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error: unknown) => {
    const message = error instanceof Error ? error.message : String(error);
    process.stderr.write(`${message}\n`);
    process.exitCode = 1;
  });
}
