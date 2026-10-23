import { createHash } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import path from 'node:path'
import { expect, test } from '@playwright/test'
import { expectJson, navigateFromHomeByUi } from '../support/live.mjs'

// Writes new desired versions on the target installation (lifecycle preemption grace,
// restored to its original value at the end). It never requests an apply.
const ENABLED = process.env.LABWEAVER_E2E_PLATFORM_CONFIGURATION?.trim() === '1'
// Optional: an actual `.age` Bundle sealed by the operator for the current desired version.
// When set, the journey registers it instead of editing settings, so the binding stays honest.
const BUNDLE_FILE = process.env.LABWEAVER_E2E_PLATFORM_CONFIGURATION_BUNDLE_FILE?.trim() || ''
const STATUS_PATH = '/api/v1/admin/platform-configuration'

test.skip(!ENABLED, 'set LABWEAVER_E2E_PLATFORM_CONFIGURATION=1 to run this journey; it saves new desired versions on the target installation')
test.describe.configure({ retries: 0, timeout: 180_000 })

async function openPage(page) {
  await navigateFromHomeByUi(page, '平台配置')
  await expect(page.getByRole('heading', { name: '平台配置', exact: true, level: 2 })).toBeVisible()
  await expect(page.getByTestId('pc-applied-state')).toBeVisible({ timeout: 60_000 })
  const failure = page.locator('.diagnostic-banner--error')
  if (await failure.count()) {
    const code = (await failure.first().locator('.diagnostic-code').textContent())?.trim() || 'diagnostic-missing'
    throw new Error(`LW_PLATFORM_CONFIGURATION_LOAD_FAILED:${code}`)
  }
}

async function readStatus(page) {
  return expectJson(await page.request.get(STATUS_PATH), 'LW_PLATFORM_CONFIGURATION_READ_FAILED')
}

async function saveGrace(page, grace) {
  await page.getByTestId('pc-preemption-grace').fill(String(grace))
  const response = page.waitForResponse((candidate) => candidate.request().method() === 'PATCH' && new URL(candidate.url()).pathname === STATUS_PATH)
  await page.getByTestId('pc-save').click()
  return response
}

test('platform administrator edits, conflicts and fails closed on platform configuration through the UI', async ({ page, context }) => {
  await openPage(page)
  const before = await readStatus(page)
  const original = (before.desired?.configuration ?? before.defaults).lifecycle.preemptionGraceSeconds

  // Settings without an execution-side consumer are not offered at all.
  const form = page.locator('.pc-form')
  await expect(form.locator('input[type="checkbox"]')).toHaveCount(0)
  await expect(form.getByTestId('pc-runtime-fixed')).toContainText('默认容器 UID')
  await expect(form.locator('select[data-testid="pc-privileged-mode"] option[value="virtual_machine_guest"]')).toBeDisabled()

  // Advanced YAML that the Bundle/Helm path would reject never reaches the server.
  let patchCount = 0
  const countPatches = (request) => { if (request.method() === 'PATCH' && new URL(request.url()).pathname === STATUS_PATH) patchCount += 1 }
  page.on('request', countPatches)
  await page.getByRole('tab', { name: '高级 YAML', exact: true }).click()
  const yaml = page.getByTestId('pc-yaml')
  await yaml.fill((await yaml.inputValue()).replace('allowedCapabilities: []', 'allowedCapabilities:\n      - NET_ADMIN'))
  await page.getByTestId('pc-save').click()
  const unsupported = page.getByTestId('pc-save-failure')
  await expect(unsupported.locator('.diagnostic-message')).toContainText('尚无执行侧实现')
  await expect(unsupported.locator('.diagnostic-code')).toHaveText('LW_BUNDLE_CONFIGURATION_POLICY_UNSUPPORTED')
  expect(patchCount, 'LW_PLATFORM_CONFIGURATION_UNSUPPORTED_SENT').toBe(0)
  page.off('request', countPatches)
  await page.getByRole('tab', { name: '表单', exact: true }).click()
  await page.reload()
  await expect(page.getByTestId('pc-applied-state')).toBeVisible({ timeout: 60_000 })

  if (BUNDLE_FILE) {
    const bytes = await readFile(BUNDLE_FILE)
    const digest = createHash('sha256').update(bytes).digest('hex')
    await page.getByTestId('pc-bundle-file').setInputFiles({ name: path.basename(BUNDLE_FILE), mimeType: 'application/octet-stream', buffer: bytes })
    const register = page.waitForResponse((candidate) => candidate.request().method() === 'POST' && new URL(candidate.url()).pathname.endsWith('/bundle'))
    await page.getByTestId('pc-register').click()
    const registered = await expectJson(await register, 'LW_PLATFORM_CONFIGURATION_REGISTER_FAILED')
    expect(registered.registeredBundle.encryptedBundleSha256).toBe(digest)
    await expect(page.getByTestId('pc-registered-bundle')).toContainText(digest)
    await expect(page.getByTestId('pc-apply')).toBeEnabled()
    console.log(`[LW_PLATFORM_CONFIGURATION_REGISTER_OK] desired=${registered.desired.revision} digest=${digest.slice(0, 12)}`)
    return
  }

  // A stale second tab cannot overwrite a newer desired version: the fence fails and the draft is kept.
  const stale = await context.newPage()
  await openPage(stale)
  const changed = original === 300 ? 299 : original + 1
  const saved = await expectJson(await saveGrace(page, changed), 'LW_PLATFORM_CONFIGURATION_SAVE_FAILED')
  expect(saved.desired.configuration.lifecycle.preemptionGraceSeconds).toBe(changed)
  expect(saved.registeredBundle).toBeNull()
  await expect(page.getByTestId('pc-notice')).toContainText('保存不等于部署')

  const staleGrace = changed === 299 ? 298 : changed + 1
  const rejected = await saveGrace(stale, staleGrace)
  expect([409, 412]).toContain(rejected.status())
  await expect(stale.getByTestId('pc-conflict')).toContainText(`revision ${saved.revision}`)
  const conflictBanner = stale.getByTestId('pc-save-failure')
  await expect(conflictBanner.locator('.diagnostic-message')).toHaveText('配置已被其他管理员更新，本次提交未生效。')
  await expect(stale.getByTestId('pc-preemption-grace')).toHaveValue(String(staleGrace))
  expect((await readStatus(page)).desired.configuration.lifecycle.preemptionGraceSeconds).toBe(changed)

  // Keep the stale draft, rebase onto the latest revision, then restore the original value.
  await stale.getByRole('button', { name: new RegExp(`保留我的草稿，基于 revision ${saved.revision}`) }).click()
  const restored = await expectJson(await saveGrace(stale, original), 'LW_PLATFORM_CONFIGURATION_RESTORE_FAILED')
  expect(restored.desired.configuration.lifecycle.preemptionGraceSeconds).toBe(original)
  expect(restored.revision).toBeGreaterThan(saved.revision)
  await stale.close()
  console.log(`[LW_PLATFORM_CONFIGURATION_UI_OK] revisions=${before.revision}->${restored.revision}`)
})
