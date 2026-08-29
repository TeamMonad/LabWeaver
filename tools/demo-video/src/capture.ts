import {mkdtemp, mkdir, readFile, rm, writeFile} from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {createRequire} from 'node:module';
import {chromium, expect, type Page} from '@playwright/test';
import {DemoVideoError, invariant} from './errors.js';
import {fileEvidence, resolveLocator} from './paths.js';
import {probeVideo, run} from './process.js';
import {assertNoForbiddenText} from './privacy.js';
import {SCENES, SCENE_IDS, VIDEO, type CaptureReceipt, type FrozenIdentity, type Profile, type SceneId} from './model.js';
import {validateReceipt} from './schema.js';

type CaptureOptions = {root: string; profile: Profile; sceneId: SceneId; baseUrl: string; identityLocator?: string};
type IdentityInput = {sourceCommit: string; runId: string; identity: FrozenIdentity};

const require = createRequire(import.meta.url);
const playwrightVersion = (require('@playwright/test/package.json') as {version: string}).version;

async function createPublicMaterial(): Promise<string> {
  const root = await mkdtemp(path.join(os.tmpdir(), 'labweaver-demo-material-'));
  await writeFile(path.join(root, 'README.md'), '# Public C/C++ lab\n\nBuild and run the public sample.\n');
  await writeFile(path.join(root, 'main.cpp'), '#include <iostream>\nint main(){std::cout << "LabWeaver public demo\\n";}\n');
  return root;
}

async function createEnvironment(page: Page, runtime: '容器' | '虚拟机'): Promise<void> {
  await page.goto('/student/environments');
  await expect(page.locator('.environment-entry')).toBeVisible();
  await page.locator(`tr:has-text("${runtime}") button:has-text("创建环境")`).last().click();
  await expect(page).toHaveURL(/environmentId=/);
  await expect(page.locator('.env-state')).toHaveText('ready', {timeout: 20_000});
}

async function issueGrant(page: Page): Promise<void> {
  await page.locator('button:has-text("签发访问授权")').click();
  await expect(page.locator('.grant-card')).toBeVisible();
  await expect(page.locator('.grant-card')).toContainText('active');
}

async function driveScene(page: Page, profile: Profile, sceneId: SceneId): Promise<void> {
  switch (sceneId) {
    case 'opening':
      await page.goto('/student/environments');
      await expect(page.locator('#app')).toContainText('LabWeaver');
      await expect(page.locator('tr:has-text("容器")')).toBeVisible({timeout: 20_000});
      await expect(page.locator('tr:has-text("虚拟机")')).toBeVisible({timeout: 20_000});
      return;
    case 'teacher-authoring': {
      const material = await createPublicMaterial();
      try {
        await page.goto('/teacher/materials');
        await expect(page.locator('.material-upload')).toBeVisible();
        await expect(page.locator('.policy-card')).toBeVisible();
        await page.setInputFiles('input[type="file"]', material);
        await page.locator('button:has-text("上传材料包")').click();
        await expect(page.locator('.package-summary')).toContainText('材料包已归档');
        await page.locator('button:has-text("启动 AgentRun")').click();
        await expect(page.locator('.run-state')).toHaveText('succeeded', {timeout: 20_000});
        await page.locator('a:has-text("进入候选审批")').click();
        await expect(page.locator('.candidate-approval')).toBeVisible();
        await page.fill('textarea[aria-label="Environment 审批理由"]', '公开演示候选已完成确定性验证');
        await page.locator('button:has-text("批准")').first().click();
        await expect(page.locator('.approval-status--approved')).toBeVisible();
        await page.fill('textarea[aria-label="Evaluation 审批理由"]', '只确认冻结协调与终态投影');
        await page.locator('button:has-text("批准")').last().click();
        await page.locator('button:has-text("发布 EvaluationRelease")').click();
        await page.getByRole('alertdialog').getByRole('button', {name: '发布', exact: true}).click();
        await expect(page.getByText('EvaluationRelease 已发布')).toBeVisible();
        await page.locator('button:has-text("发布 EnvironmentTemplateRelease")').click();
        await page.getByRole('button', {name: '发布', exact: true}).click();
        await expect(page.getByText('已接受发布请求')).toBeVisible();
      } finally {
        await rm(material, {recursive: true, force: true});
      }
      return;
    }
    case 'admin-resource':
      await page.goto('/admin/resource-approval');
      await expect(page.locator('.resource-approval')).toBeVisible();
      await page.locator('.request-table .data-table__row', {hasText: 'cpu-lab-request'}).click();
      await page.fill('textarea[aria-label="资源申请操作理由"]', 'Approve the public lab demonstration');
      await page.getByRole('button', {name: '批准', exact: true}).click();
      await page.getByRole('alertdialog').getByRole('button', {name: '确认'}).click();
      await expect(page.locator('.diagnostic-banner--info')).toContainText('RESOURCE_REQUEST_APPROVED');
      await expect(page.locator('.lease-table .data-table__row')).toHaveCount(3);
      return;
    case 'student-container':
      await createEnvironment(page, '容器');
      await issueGrant(page);
      await page.locator('button:has-text("打开终端")').click();
      await expect(page.locator('.xterm-host')).toBeVisible({timeout: 20_000});
      if (profile === 'fixture-preview') {
        await expect(page.getByTestId('fixture-banner')).toContainText('FIXTURE MODE');
      }
      await page.locator('.xterm-helper-textarea').focus();
      await page.keyboard.type("printf '#include <iostream>\\nint main(){std::cout << \\\"LabWeaver\\\\n\\\";}\\n' > main.cpp");
      await page.keyboard.press('Enter');
      await page.keyboard.type('g++ -std=c++17 -O2 main.cpp -o lab-demo && ./lab-demo');
      await page.keyboard.press('Enter');
      await page.setViewportSize({width: 1680, height: 945});
      await expect(page.locator('.xterm-host')).toBeVisible();
      await page.setViewportSize({width: VIDEO.width, height: VIDEO.height});
      await page.getByRole('button', {name: '断开', exact: true}).click();
      await expect(page.locator('.xterm-host')).toHaveCount(0);
      await page.locator('button:has-text("打开终端")').click();
      await expect(page.locator('.xterm-host')).toBeVisible({timeout: 20_000});
      return;
    case 'student-kubevirt':
      await createEnvironment(page, '虚拟机');
      await issueGrant(page);
      await page.locator('button:has-text("打开图形控制台")').click();
      if (profile === 'fixture-preview') {
        await expect(page.getByText('CONSOLE_UPSTREAM_UNAVAILABLE')).toBeVisible({timeout: 20_000});
      } else {
        await expect(page.locator('[data-testid="novnc-connection-state"]')).toContainText('已连接', {timeout: 30_000});
        await expect(page.locator('.novnc-host')).toBeVisible();
        const canvas = page.locator('.novnc-host canvas').first();
        await expect(canvas).toBeVisible();
        await canvas.click();
        await page.keyboard.type('uname -a');
        await page.keyboard.press('Enter');
        await page.getByRole('button', {name: '断开', exact: true}).click();
      }
      const disconnect = page.getByRole('button', {name: '断开', exact: true});
      if (await disconnect.count()) await disconnect.click();
      await page.getByRole('button', {name: '停止', exact: true}).click();
      await expect(page.locator('.env-header .env-state')).toHaveText('stopped', {timeout: 20_000});
      await page.getByRole('button', {name: '启动', exact: true}).click();
      await expect(page.locator('.env-header .env-state')).toHaveText('ready', {timeout: 20_000});
      if (profile === 'connected-final') {
        await page.locator('button:has-text("打开图形控制台")').click();
        await expect(page.locator('[data-testid="novnc-connection-state"]')).toContainText('已连接', {timeout: 30_000});
      }
      return;
    case 'submission-freeze':
      await createEnvironment(page, '虚拟机');
      await issueGrant(page);
      await page.locator('button:has-text("冻结提交")').click();
      await expect(page.getByText('Object Version', {exact: true})).toBeVisible({timeout: 20_000});
      await expect(page.getByText('SHA-256', {exact: true})).toBeVisible();
      return;
    case 'access-revoke':
      await createEnvironment(page, '容器');
      await issueGrant(page);
      await page.locator('button:has-text("打开终端")').click();
      await expect(page.locator('.xterm-host')).toBeVisible({timeout: 20_000});
      await page.locator('button:has-text("撤销授权")').click();
      await expect(page.locator('.xterm-host')).toHaveCount(0);
      await expect(page.getByText('ACCESS_GRANT_REVOKED')).toBeVisible();
      return;
    case 'cleanup':
      await createEnvironment(page, '容器');
      await page.getByRole('button', {name: '删除', exact: true}).click();
      await page.getByRole('alertdialog').getByRole('button', {name: '删除', exact: true}).click();
      await page.reload({waitUntil: 'domcontentloaded'});
      await expect(page.getByText(/ENVIRONMENT_|不存在|not found/i).first()).toBeVisible({timeout: 20_000});
      return;
  }
}

function storageState(root: string, sceneId: SceneId): string {
  if (sceneId === 'opening') return path.join(root, 'web/.auth/student.json');
  if (sceneId === 'teacher-authoring') return path.join(root, 'web/.auth/teacher.json');
  if (sceneId === 'admin-resource') return path.join(root, 'web/.auth/platform-admin.json');
  return path.join(root, 'web/.auth/student.json');
}

export async function capture(options: CaptureOptions): Promise<string> {
  invariant(SCENE_IDS.has(options.sceneId), 'LW_DEMO_VIDEO_SCENE_UNKNOWN', `unknown scene: ${options.sceneId}`);
  const response = await fetch(options.baseUrl, {redirect: 'manual'}).catch((error: unknown) => {
    throw new DemoVideoError('LW_DEMO_VIDEO_BASE_URL_UNREACHABLE', String(error));
  });
  invariant(response.status < 500, 'LW_DEMO_VIDEO_BASE_URL_UNREACHABLE', `${options.baseUrl} returned ${response.status}`);

  let frozen: IdentityInput | null = null;
  if (options.profile === 'connected-final') {
    invariant(options.identityLocator, 'LW_DEMO_VIDEO_IDENTITY_REQUIRED', 'connected-final requires --identity');
    const absolute = resolveLocator(options.root, options.identityLocator, ['artifacts/demo-video']);
    frozen = JSON.parse(await readFile(absolute, 'utf8')) as IdentityInput;
  } else {
    invariant(!options.identityLocator, 'LW_DEMO_VIDEO_FIXTURE_IDENTITY_FORBIDDEN', 'fixture-preview must not bind connected identity');
  }
  const sourceCommit = await run('git', ['rev-parse', 'HEAD'], 'LW_DEMO_VIDEO_GIT_FAILED');
  if (frozen) invariant(frozen.sourceCommit === sourceCommit, 'LW_DEMO_VIDEO_COMMIT_MISMATCH', 'connected identity does not match HEAD');

  const scene = SCENES.find(({id}) => id === options.sceneId)!;
  const outputDir = path.join(options.root, 'artifacts/demo-video', options.profile, 'captures', scene.id);
  await rm(outputDir, {recursive: true, force: true});
  await mkdir(outputDir, {recursive: true});
  const tracePath = path.join(outputDir, 'trace.zip');
  const screenshotPath = path.join(outputDir, 'final.png');
  const failureScreenshotPath = path.join(outputDir, 'failure.png');
  const clipPath = path.join(outputDir, 'scene.webm');
  const browser = await chromium.launch({headless: true, slowMo: 650});
  let browserVersion = '';
  try {
    browserVersion = browser.version();
    const context = await browser.newContext({
      baseURL: options.baseUrl,
      viewport: {width: VIDEO.width, height: VIDEO.height},
      colorScheme: 'light',
      storageState: storageState(options.root, options.sceneId),
      recordVideo: {dir: outputDir, size: {width: VIDEO.width, height: VIDEO.height}},
    });
    // Source embedding can disclose workstation paths. Screenshots and DOM
    // snapshots are sufficient for this local review artifact.
    await context.tracing.start({screenshots: true, snapshots: true, sources: false});
    const page = await context.newPage();
    invariant(page.video(), 'LW_DEMO_VIDEO_CAPTURE_NOT_STARTED', 'Playwright did not start video recording');
    try {
      await driveScene(page, options.profile, options.sceneId);
      await page.screenshot({path: screenshotPath, fullPage: false, animations: 'disabled'});
      await context.tracing.stop({path: tracePath});
      const recorded = await page.video()!.path();
      await context.close();
      await run('ffmpeg', ['-y', '-i', recorded, '-c', 'copy', clipPath], 'LW_DEMO_VIDEO_CAPTURE_FINALIZE_FAILED');
      if (recorded !== clipPath) await rm(recorded, {force: true});
    } catch (error) {
      await page.screenshot({path: failureScreenshotPath, fullPage: false, animations: 'disabled'}).catch(() => undefined);
      await context.tracing.stop({path: tracePath}).catch(() => undefined);
      const recorded = await page.video()!.path().catch(() => '');
      await context.close().catch(() => undefined);
      if (recorded) {
        await run('ffmpeg', ['-y', '-i', recorded, '-c', 'copy', path.join(outputDir, 'failure.webm')], 'LW_DEMO_VIDEO_CAPTURE_FAILURE_VIDEO_FAILED');
        await rm(recorded, {force: true});
      }
      throw error;
    }
  } finally {
    await browser.close();
  }
  const clipProbe = await probeVideo(clipPath);
  const receipt: CaptureReceipt = {
    schemaVersion: 'demo-video-capture-receipt.v1',
    sceneId: scene.id,
    role: scene.role,
    profile: options.profile,
    releaseEligible: options.profile === 'connected-final',
    sourceCommit,
    runId: frozen?.runId ?? null,
    identity: frozen?.identity ?? null,
    clip: {...await fileEvidence(options.root, clipPath), durationSeconds: clipProbe.durationSeconds},
    trace: await fileEvidence(options.root, tracePath),
    screenshot: await fileEvidence(options.root, screenshotPath),
    browser: {name: 'chromium', version: browserVersion, playwrightVersion},
    viewport: {width: VIDEO.width, height: VIDEO.height},
    capturedAt: new Date().toISOString(),
    privacy: {
      automatedScan: 'passed', humanReview: 'pending', containsSecrets: false,
      containsRawUserContent: false, containsTerminalTranscript: false, containsAbsolutePaths: false,
    },
  };
  assertNoForbiddenText([JSON.stringify(receipt)]);
  await validateReceipt(options.root, receipt);
  const receiptPath = path.join(outputDir, 'capture-receipt.v1.json');
  await writeFile(receiptPath, `${JSON.stringify(receipt, null, 2)}\n`);
  return receiptPath;
}
