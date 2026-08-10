import {createHash} from 'node:crypto';
import {spawn} from 'node:child_process';
import {mkdir, readFile, writeFile} from 'node:fs/promises';
import path from 'node:path';
import {chromium, expect} from '@playwright/test';
import {capture} from './capture.js';
import {DemoVideoError, invariant} from './errors.js';
import {SCENES} from './model.js';
import {run} from './process.js';
import {render} from './render.js';
import {validateLocalClusterReport} from './schema.js';
import {verify} from './verify.js';

type LocalAction = 'deploy' | 'verify' | 'demo' | 'teardown';
type LocalPolicy = {
  spec: {
    releaseEligible: false;
    writePolicy: {apply: false; delete: false};
    fixtureDemoPolicy: {namespace: string; releaseName: string; apply: true; delete: true; releaseEligible: false};
  };
};

const CONTEXT = 'docker-desktop';
const BASE_URL = 'http://localhost:4173';
const REPORT_LOCATOR = 'artifacts/demo-video/local-cluster/deployment-report.v1.json';

async function policy(root: string): Promise<LocalPolicy> {
  const value = JSON.parse(await readFile(path.join(root, 'deploy/config/local-hostpath-stack.overlay.json'), 'utf8')) as LocalPolicy;
  invariant(value.spec.releaseEligible === false && value.spec.writePolicy.apply === false && value.spec.writePolicy.delete === false,
    'LW_DEMO_VIDEO_LOCAL_POLICY_INVALID', 'application-stack writes must remain disabled');
  const fixture = value.spec.fixtureDemoPolicy;
  invariant(fixture?.apply === true && fixture.delete === true && fixture.releaseEligible === false,
    'LW_DEMO_VIDEO_LOCAL_POLICY_INVALID', 'Fixture demo policy must explicitly authorize only non-release apply/delete');
  invariant(fixture.namespace === 'labweaver-local-demo' && fixture.releaseName === 'labweaver-fixture-demo',
    'LW_DEMO_VIDEO_LOCAL_POLICY_INVALID', 'Fixture demo target identity changed');
  return value;
}

async function assertContext(): Promise<void> {
  const context = await run('kubectl', ['config', 'current-context'], 'LW_DEMO_VIDEO_KUBECTL_CONTEXT_FAILED');
  invariant(context === CONTEXT, 'LW_DEMO_VIDEO_KUBECTL_CONTEXT_INVALID', `expected ${CONTEXT}, received ${context}`);
  const dockerContext = await run('docker', ['context', 'show'], 'LW_DEMO_VIDEO_DOCKER_CONTEXT_FAILED');
  invariant(dockerContext === 'desktop-linux', 'LW_DEMO_VIDEO_DOCKER_CONTEXT_INVALID', `expected desktop-linux, received ${dockerContext}`);
}

async function hardwareEncoder(): Promise<'h264_nvenc' | 'h264_qsv' | 'h264_amf'> {
  const candidates = ['h264_nvenc', 'h264_qsv', 'h264_amf'] as const;
  const diagnostics: string[] = [];
  for (const encoder of candidates) {
    try {
      await run('ffmpeg', [
        '-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i', 'color=c=black:s=256x256:r=1',
        '-frames:v', '1', '-c:v', encoder, '-f', 'null', '-',
      ], 'LW_DEMO_VIDEO_HARDWARE_ENCODER_PROBE_FAILED');
      return encoder;
    } catch (error) {
      diagnostics.push(`${encoder}:${error instanceof Error ? error.message.split('\n')[0] : String(error)}`);
    }
  }
  throw new DemoVideoError('LW_DEMO_VIDEO_HARDWARE_ENCODER_UNAVAILABLE', diagnostics.join('; '));
}

async function nodeIdentity(): Promise<{name: string; ready: true; kubernetesVersion: string}> {
  const nodes = JSON.parse(await run('kubectl', ['get', 'nodes', '-o', 'json'], 'LW_DEMO_VIDEO_KUBECTL_NODES_FAILED')) as any;
  invariant(nodes.items?.length === 1, 'LW_DEMO_VIDEO_LOCAL_NODE_COUNT_INVALID', 'Docker Desktop demo requires exactly one node');
  const node = nodes.items[0];
  const ready = node.status?.conditions?.find((condition: any) => condition.type === 'Ready')?.status === 'True';
  invariant(ready, 'LW_DEMO_VIDEO_LOCAL_NODE_NOT_READY', 'Docker Desktop node is not Ready');
  return {name: node.metadata.name, ready: true, kubernetesVersion: node.status.nodeInfo.kubeletVersion};
}

async function crdCapabilities(): Promise<{kubevirt: boolean; cdi: boolean}> {
  const value = JSON.parse(await run('kubectl', ['get', 'crd', '-o', 'json'], 'LW_DEMO_VIDEO_KUBECTL_CRD_FAILED')) as any;
  const names = new Set<string>((value.items ?? []).map((item: any) => item.metadata.name));
  return {
    kubevirt: names.has('virtualmachines.kubevirt.io') && names.has('virtualmachineinstances.kubevirt.io'),
    cdi: names.has('datavolumes.cdi.kubevirt.io'),
  };
}

async function withPortForward<T>(namespace: string, callback: () => Promise<T>): Promise<T> {
  const child = spawn('kubectl', [
    '--context', CONTEXT, '-n', namespace, 'port-forward', '--address', '127.0.0.1',
    'service/labweaver-demo-web', '4173:8080',
  ], {stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true});
  let output = '';
  const ready = new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => reject(new DemoVideoError('LW_DEMO_VIDEO_PORT_FORWARD_TIMEOUT', output.trim())), 30_000);
    const consume = (chunk: Buffer) => {
      output += chunk.toString('utf8');
      if (output.includes('Forwarding from 127.0.0.1:4173')) {
        clearTimeout(timer);
        resolve();
      }
    };
    child.stdout.on('data', consume);
    child.stderr.on('data', consume);
    child.once('error', (error) => { clearTimeout(timer); reject(new DemoVideoError('LW_DEMO_VIDEO_PORT_FORWARD_FAILED', error.message)); });
    child.once('exit', (code) => { clearTimeout(timer); reject(new DemoVideoError('LW_DEMO_VIDEO_PORT_FORWARD_FAILED', `kubectl exited ${code}: ${output.trim()}`)); });
  });
  try {
    await ready;
    return await callback();
  } finally {
    if (!child.killed) child.kill();
  }
}

async function fixtureHealth(namespace: string): Promise<void> {
  await withPortForward(namespace, async () => {
    const health = await fetch(`${BASE_URL}/health/ready`);
    invariant(health.status === 200, 'LW_DEMO_VIDEO_LOCAL_HEALTH_FAILED', `health returned ${health.status}`);
    const browser = await chromium.launch({headless: true});
    try {
      const page = await browser.newPage({viewport: {width: 1920, height: 1080}});
      await page.goto(BASE_URL);
      await expect(page.getByTestId('fixture-banner')).toContainText('FIXTURE MODE');
    } finally {
      await browser.close();
    }
  });
}

async function deploymentReport(root: string, image: string, imageId: string, chartManifest: string) {
  const configured = await policy(root);
  const fixture = configured.spec.fixtureDemoPolicy;
  await assertContext();
  const node = await nodeIdentity();
  const capabilities = await crdCapabilities();
  const encoder = await hardwareEncoder();
  const namespace = JSON.parse(await run('kubectl', ['--context', CONTEXT, 'get', 'namespace', fixture.namespace, '-o', 'json'], 'LW_DEMO_VIDEO_LOCAL_NAMESPACE_FAILED')) as any;
  invariant(namespace.metadata?.labels?.['labweaver.io/local-profile'] === 'local-hostpath'
    && namespace.metadata?.labels?.['labweaver.io/owner'] === 'labweaver',
  'LW_DEMO_VIDEO_LOCAL_NAMESPACE_OWNERSHIP_INVALID', 'local demo namespace ownership labels are missing');
  await run('helm', ['status', fixture.releaseName, '--namespace', fixture.namespace, '--output', 'json'], 'LW_DEMO_VIDEO_HELM_STATUS_FAILED');
  await run('kubectl', ['--context', CONTEXT, '-n', fixture.namespace, 'rollout', 'status', 'deployment/labweaver-demo-web', '--timeout=120s'], 'LW_DEMO_VIDEO_LOCAL_ROLLOUT_FAILED');
  const deployment = JSON.parse(await run('kubectl', ['--context', CONTEXT, '-n', fixture.namespace, 'get', 'deployment/labweaver-demo-web', '-o', 'json'], 'LW_DEMO_VIDEO_LOCAL_DEPLOYMENT_FAILED')) as any;
  invariant(deployment.status?.readyReplicas === 1 && deployment.status?.availableReplicas === 1,
    'LW_DEMO_VIDEO_LOCAL_DEPLOYMENT_NOT_READY', 'Fixture deployment does not have exactly one ready replica');
  invariant(deployment.spec?.template?.spec?.automountServiceAccountToken === false,
    'LW_DEMO_VIDEO_LOCAL_SERVICE_ACCOUNT_TOKEN_ENABLED', 'Fixture Pod must not mount a service-account token');
  invariant(deployment.spec?.template?.spec?.containers?.[0]?.image === image,
    'LW_DEMO_VIDEO_LOCAL_IMAGE_MISMATCH', 'deployed image differs from requested image');
  await fixtureHealth(fixture.namespace);
  const report = {
    schemaVersion: 'demo-video-local-cluster-report.v1', status: 'verified', releaseEligible: false,
    profile: 'fixture-preview', sourceCommit: image.split(':').at(-1), context: CONTEXT, dockerContext: 'desktop-linux',
    namespace: fixture.namespace, releaseName: fixture.releaseName,
    image: {reference: image, id: imageId},
    chartManifestSha256: `sha256:${createHash('sha256').update(chartManifest).digest('hex')}`,
    node,
    capabilities: {...capabilities, containerFixture: true, hardwareVideoEncoding: 'required', hardwareEncoder: encoder, renderConcurrency: 1},
    checks: ['context', 'docker-context', 'node-ready', 'namespace-owned', 'helm-release', 'deployment-ready', 'health-ready', 'fixture-banner', 'hardware-encoder'],
    createdAt: new Date().toISOString(),
  };
  await validateLocalClusterReport(root, report);
  const absolute = path.join(root, REPORT_LOCATOR);
  await mkdir(path.dirname(absolute), {recursive: true});
  await writeFile(absolute, `${JSON.stringify(report, null, 2)}\n`);
  return REPORT_LOCATOR;
}

async function deploy(root: string): Promise<string> {
  const configured = await policy(root);
  const fixture = configured.spec.fixtureDemoPolicy;
  await assertContext();
  await nodeIdentity();
  const commit = await run('git', ['rev-parse', 'HEAD'], 'LW_DEMO_VIDEO_GIT_FAILED', {cwd: root});
  const dirty = await run('git', ['status', '--porcelain'], 'LW_DEMO_VIDEO_GIT_FAILED', {cwd: root});
  invariant(dirty === '', 'LW_DEMO_VIDEO_SOURCE_DIRTY', 'commit the candidate before building the local demo image');
  const image = `labweaver/demo-fixture:${commit}`;
  await run('docker', ['build', '--file', 'containers/Containerfile.demo-fixture', '--build-arg', `SOURCE_COMMIT=${commit}`, '--tag', image, '.'], 'LW_DEMO_VIDEO_LOCAL_IMAGE_BUILD_FAILED', {cwd: root});
  const imageId = await run('docker', ['image', 'inspect', '--format', '{{.Id}}', image], 'LW_DEMO_VIDEO_LOCAL_IMAGE_INSPECT_FAILED');
  const chart = path.join(root, 'deploy/helm/labweaver-demo');
  const chartManifest = await run('helm', ['template', fixture.releaseName, chart, '--namespace', fixture.namespace, '--set-string', `image.tag=${commit}`], 'LW_DEMO_VIDEO_LOCAL_HELM_RENDER_FAILED');
  await run('helm', [
    'upgrade', '--install', fixture.releaseName, chart, '--namespace', fixture.namespace, '--create-namespace',
    '--atomic', '--wait', '--timeout', '5m', '--set-string', `image.tag=${commit}`,
  ], 'LW_DEMO_VIDEO_LOCAL_HELM_APPLY_FAILED');
  await run('kubectl', [
    '--context', CONTEXT, 'label', 'namespace', fixture.namespace,
    'labweaver.io/local-profile=local-hostpath', 'labweaver.io/owner=labweaver', '--overwrite',
  ], 'LW_DEMO_VIDEO_LOCAL_NAMESPACE_LABEL_FAILED');
  return await deploymentReport(root, image, imageId, chartManifest);
}

async function verifyDeployment(root: string): Promise<string> {
  const configured = await policy(root);
  const fixture = configured.spec.fixtureDemoPolicy;
  const commit = await run('git', ['rev-parse', 'HEAD'], 'LW_DEMO_VIDEO_GIT_FAILED', {cwd: root});
  const image = `labweaver/demo-fixture:${commit}`;
  const imageId = await run('docker', ['image', 'inspect', '--format', '{{.Id}}', image], 'LW_DEMO_VIDEO_LOCAL_IMAGE_INSPECT_FAILED');
  const chartManifest = await run('helm', ['get', 'manifest', fixture.releaseName, '--namespace', fixture.namespace], 'LW_DEMO_VIDEO_HELM_STATUS_FAILED');
  return await deploymentReport(root, image, imageId, chartManifest);
}

async function demonstrate(root: string): Promise<string> {
  const configured = await policy(root);
  const fixture = configured.spec.fixtureDemoPolicy;
  await verifyDeployment(root);
  await run('pnpm', ['auth:fixture'], 'LW_DEMO_VIDEO_FIXTURE_AUTH_FAILED', {cwd: path.join(root, 'web')});
  return await withPortForward(fixture.namespace, async () => {
    for (const scene of SCENES) {
      process.stderr.write(`${JSON.stringify({event: 'demo_video_local_scene_capture', sceneId: scene.id})}\n`);
      await capture({root, profile: 'fixture-preview', sceneId: scene.id, baseUrl: BASE_URL});
    }
    const manifest = 'artifacts/demo-video/preview/demo-video-manifest.v1.json';
    await render({root, cut: 'preview', manifestLocator: manifest});
    await verify({root, cut: 'preview', manifestLocator: manifest});
    return manifest;
  });
}

async function teardown(root: string): Promise<string> {
  const configured = await policy(root);
  const fixture = configured.spec.fixtureDemoPolicy;
  await assertContext();
  await run('helm', ['uninstall', fixture.releaseName, '--namespace', fixture.namespace, '--wait', '--timeout', '2m'], 'LW_DEMO_VIDEO_LOCAL_HELM_DELETE_FAILED');
  await run('kubectl', ['--context', CONTEXT, 'delete', 'namespace', fixture.namespace, '--wait=true', '--timeout=2m'], 'LW_DEMO_VIDEO_LOCAL_NAMESPACE_DELETE_FAILED');
  return fixture.namespace;
}

export async function localCluster(root: string, action: LocalAction): Promise<string> {
  if (action === 'deploy') return await deploy(root);
  if (action === 'verify') return await verifyDeployment(root);
  if (action === 'demo') return await demonstrate(root);
  if (action === 'teardown') return await teardown(root);
  throw new DemoVideoError('LW_DEMO_VIDEO_LOCAL_ACTION_INVALID', `invalid local action: ${String(action)}`);
}
