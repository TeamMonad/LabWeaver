import { createHash } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import { expect, test } from '@playwright/test'
import { csrfHeaders, expectJson, navigateFromHomeByUi, uuidv7 } from '../support/live.mjs'

// Destructive journey: it saves new desired versions on the target installation and restores
// the original desired settings and Bundle registration in `finally`. It never requests an apply.
//
// LABWEAVER_E2E_PLATFORM_CONFIGURATION_CONFIRM must equal `rev-<current aggregate revision>` of
// the target. Every run advances that revision, so a confirmation cannot be reused by accident.
// When the current desired has a registered Bundle, the server keeps only its digest; restoring
// it needs the original ciphertext in LABWEAVER_E2E_PLATFORM_CONFIGURATION_RESTORE_BUNDLE_FILE,
// whose SHA-256 and size must match before anything is written.
const CONFIRM = process.env.LABWEAVER_E2E_PLATFORM_CONFIGURATION_CONFIRM?.trim() || ''
const RESTORE_BUNDLE_FILE = process.env.LABWEAVER_E2E_PLATFORM_CONFIGURATION_RESTORE_BUNDLE_FILE?.trim() || ''
const STATUS_PATH = '/api/v1/admin/platform-configuration'

test.skip(!CONFIRM, 'set LABWEAVER_E2E_PLATFORM_CONFIGURATION_CONFIRM=rev-<current revision> to run this destructive journey')
test.describe.configure({ retries: 0, timeout: 240_000 })

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

async function readStatus(request) {
  return expectJson(await request.get(STATUS_PATH), 'LW_PLATFORM_CONFIGURATION_READ_FAILED')
}

async function saveGraceByUi(page, grace) {
  await page.getByTestId('pc-preemption-grace').fill(String(grace))
  const response = page.waitForResponse((candidate) => candidate.request().method() === 'PATCH' && new URL(candidate.url()).pathname === STATUS_PATH)
  await page.getByTestId('pc-save').click()
  return response
}

/** Everything that must be true before the first write; throws without touching the target. */
async function prepareBackup(request) {
  const before = await readStatus(request)
  if (CONFIRM !== `rev-${before.revision}`) {
    throw new Error(`LW_PLATFORM_CONFIGURATION_CONFIRM_MISMATCH:expected=rev-${before.revision}`)
  }
  if (!before.desired) {
    // An unconfigured installation cannot be returned to "no desired version".
    throw new Error('LW_PLATFORM_CONFIGURATION_UNCONFIGURED_NOT_RESTORABLE')
  }
  if (before.latestApply && !['succeeded', 'failed'].includes(before.latestApply.stage)) {
    throw new Error(`LW_PLATFORM_CONFIGURATION_APPLY_IN_FLIGHT:${before.latestApply.stage}`)
  }
  const registered = before.registeredBundle
  let bundleBytes = null
  if (registered) {
    if (!RESTORE_BUNDLE_FILE) throw new Error('LW_PLATFORM_CONFIGURATION_RESTORE_BUNDLE_FILE_REQUIRED')
    bundleBytes = await readFile(RESTORE_BUNDLE_FILE)
    const digest = createHash('sha256').update(bundleBytes).digest('hex')
    if (digest !== registered.encryptedBundleSha256 || bundleBytes.length !== registered.encryptedBytes) {
      throw new Error(`LW_PLATFORM_CONFIGURATION_RESTORE_BUNDLE_MISMATCH:registered=${registered.encryptedBundleSha256.slice(0, 12)}:file=${digest.slice(0, 12)}`)
    }
  }
  return { before, bundleBytes }
}

/** Return the target to the original desired settings and registration through the real API. */
async function restore(request, baseURL, { before, bundleBytes }) {
  let current = await readStatus(request)
  if (current.desired?.configurationSha256 !== before.desired.configurationSha256) {
    const response = await request.patch(STATUS_PATH, {
      headers: await csrfHeaders(request, baseURL, { 'If-Match': `"rev-${current.revision}"`, 'Idempotency-Key': uuidv7() }),
      data: { expectedRevision: current.revision, configuration: before.desired.configuration },
    })
    current = await expectJson(response, 'LW_PLATFORM_CONFIGURATION_RESTORE_DESIRED_FAILED')
  }
  const original = before.registeredBundle
  const registered = current.registeredBundle
  if (original && !(registered?.encryptedBundleSha256 === original.encryptedBundleSha256 && registered.desiredRevision === current.desired.revision)) {
    const query = new URLSearchParams({ expectedRevision: String(current.revision), configurationSha256: current.desired.configurationSha256 })
    const response = await request.post(`${STATUS_PATH}/versions/${current.desired.revision}/bundle?${query}`, {
      headers: await csrfHeaders(request, baseURL, {
        'If-Match': `"rev-${current.revision}"`,
        'Idempotency-Key': uuidv7(),
        'Content-Type': 'application/octet-stream',
      }),
      data: bundleBytes,
    })
    current = await expectJson(response, 'LW_PLATFORM_CONFIGURATION_RESTORE_BUNDLE_FAILED')
  }
  expect(current.desired.configurationSha256, 'LW_PLATFORM_CONFIGURATION_RESTORE_DESIRED_MISMATCH').toBe(before.desired.configurationSha256)
  expect(current.registeredBundle?.encryptedBundleSha256 ?? null, 'LW_PLATFORM_CONFIGURATION_RESTORE_BUNDLE_MISMATCH').toBe(original?.encryptedBundleSha256 ?? null)
  return current
}

test('platform administrator edits, conflicts and fails closed on platform configuration through the UI', async ({ page, context, baseURL }) => {
  if (!baseURL) throw new Error('PW_BASE_URL_REQUIRED')
  await openPage(page)
  const backup = await prepareBackup(page.request)
  const original = backup.before.desired.configuration.lifecycle.preemptionGraceSeconds
  let stale = null
  let restored = null
  try {
    // Settings without an execution-side consumer are not offered at all.
    const form = page.locator('.pc-form')
    await expect(form.locator('input[type="checkbox"]')).toHaveCount(0)
    await expect(form.getByTestId('pc-runtime-fixed')).toBeVisible()
    await expect(form.locator('select[data-testid="pc-privileged-mode"] option[value="virtual_machine_guest"]')).toBeDisabled()

    // Advanced YAML that the Bundle/Helm path would reject never reaches the server.
    let patchCount = 0
    const countPatches = (request) => { if (request.method() === 'PATCH' && new URL(request.url()).pathname === STATUS_PATH) patchCount += 1 }
    page.on('request', countPatches)
    await page.getByRole('tab', { name: '高级 YAML', exact: true }).click()
    const yaml = page.getByTestId('pc-yaml')
    await yaml.fill((await yaml.inputValue()).replace('allowedCapabilities: []', 'allowedCapabilities:\n      - NET_ADMIN'))
    await page.getByTestId('pc-save').click()
    await expect(page.getByTestId('pc-save-failure').locator('.diagnostic-code')).toHaveText('LW_BUNDLE_CONFIGURATION_POLICY_UNSUPPORTED')
    expect(patchCount, 'LW_PLATFORM_CONFIGURATION_UNSUPPORTED_SENT').toBe(0)
    page.off('request', countPatches)
    await page.reload()
    await expect(page.getByTestId('pc-applied-state')).toBeVisible({ timeout: 60_000 })

    // A stale second tab cannot overwrite a newer desired version: the fence fails and the draft is kept.
    stale = await context.newPage()
    await openPage(stale)
    const changed = original === 300 ? 299 : original + 1
    const saved = await expectJson(await saveGraceByUi(page, changed), 'LW_PLATFORM_CONFIGURATION_SAVE_FAILED')
    expect(saved.desired.configuration.lifecycle.preemptionGraceSeconds).toBe(changed)
    expect(saved.registeredBundle).toBeNull()

    const staleGrace = changed === 299 ? 298 : changed + 1
    const rejected = await saveGraceByUi(stale, staleGrace)
    expect([409, 412]).toContain(rejected.status())
    await expect(stale.getByTestId('pc-conflict')).toContainText(`revision ${saved.revision}`)
    await expect(stale.getByTestId('pc-save-failure').locator('.diagnostic-code')).toHaveText('LW_PLATFORM_CONFIGURATION_REVISION_CONFLICT')
    await expect(stale.getByTestId('pc-preemption-grace')).toHaveValue(String(staleGrace))
    expect((await readStatus(page.request)).desired.configuration.lifecycle.preemptionGraceSeconds).toBe(changed)

    // Keep the stale draft and save it on top of the latest revision.
    await stale.getByRole('button', { name: new RegExp(`保留我的草稿，基于 revision ${saved.revision}`) }).click()
    const rebased = await expectJson(await saveGraceByUi(stale, staleGrace), 'LW_PLATFORM_CONFIGURATION_REBASED_SAVE_FAILED')
    expect(rebased.desired.configuration.lifecycle.preemptionGraceSeconds).toBe(staleGrace)
    expect(rebased.revision).toBeGreaterThan(saved.revision)
  } finally {
    await stale?.close()
    restored = await restore(page.request, baseURL, backup)
  }
  if (backup.bundleBytes) {
    await page.reload()
    await expect(page.getByTestId('pc-registered-bundle')).toContainText(backup.before.registeredBundle.encryptedBundleSha256)
  }
  console.log(`[LW_PLATFORM_CONFIGURATION_UI_OK] revisions=${backup.before.revision}->${restored.revision} restoredBundle=${Boolean(backup.bundleBytes)}`)
})
