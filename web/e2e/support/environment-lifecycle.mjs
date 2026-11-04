import { expect } from '@playwright/test'
import { expectJson, pollJson } from './live.mjs'

const PAGE_TIMEOUT_MS = 120_000
const OPERATION_TIMEOUT_MS = 240_000

function environmentPath(routePrefix, projectId, environmentId) {
  return `/${routePrefix}/environments?projectId=${encodeURIComponent(projectId)}&environmentId=${encodeURIComponent(environmentId)}`
}

async function openEnvironmentPage(page, routePrefix, projectId, environmentId) {
  await page.goto(environmentPath(routePrefix, projectId, environmentId), { waitUntil: 'domcontentloaded' })
  await expect(page.getByRole('heading', { name: '项目环境控制台', exact: true })).toBeVisible({ timeout: PAGE_TIMEOUT_MS })
}

async function readEnvironment(page, environmentId, label) {
  const response = await page.request.get(`/api/v1/environments/${encodeURIComponent(environmentId)}`)
  if (response.status() === 404) return null
  return expectJson(response, label)
}

async function waitForDeleted(page, environmentId, label) {
  let latest = null
  await expect.poll(async () => {
    const response = await page.request.get(`/api/v1/environments/${encodeURIComponent(environmentId)}`)
    if (response.status() === 404) return true
    latest = await expectJson(response, `${label}_READ_FAILED`)
    return latest.observedState === 'deleted'
  }, { timeout: OPERATION_TIMEOUT_MS, intervals: [1000, 2000, 3000] }).toBe(true)
  return latest ?? { id: environmentId, observedState: 'deleted' }
}

async function waitForOperation(page, accepted, label, { allowCancelled = false } = {}) {
  const operation = await pollJson(
    page.request,
    accepted.statusUrl,
    (value) => ['succeeded', 'failed', 'cancelled'].includes(value.state),
    `${label}_STATUS_FAILED`,
    OPERATION_TIMEOUT_MS,
  )
  if (operation.state !== 'succeeded' && !(allowCancelled && operation.state === 'cancelled')) {
    throw new Error(`${label}_OPERATION_FAILED:${operation.state}`)
  }
  return operation
}

const SETTLING_STATES = new Set([
  'requested',
  'validating',
  'building',
  'provisioning',
  'stopping',
  'updating',
  'expiring',
])

async function waitForSettledEnvironment(page, routePrefix, projectId, environmentId, current, label, {
  cancelActive = false,
} = {}) {
  if (!current || !SETTLING_STATES.has(current.observedState)) return current

  // A provisioning or update operation can be cancelled from the same
  // operations tab a user would use. Try that control once when cleanup is
  // already requested; otherwise observe the operation until it settles.
  if (cancelActive && current.desiredState !== 'deleted') {
    await openEnvironmentPage(page, routePrefix, projectId, environmentId)
    const operationsTab = page.getByRole('tab', { name: '异步操作与诊断', exact: true })
    await expect(operationsTab).toBeVisible({ timeout: PAGE_TIMEOUT_MS })
    await operationsTab.click()
    const cancelButton = page.getByRole('button', { name: '取消操作', exact: true })
    try {
      await expect(cancelButton).toBeVisible({ timeout: 5_000 })
      await expect(cancelButton).toBeEnabled({ timeout: 5_000 })
      const responsePromise = page.waitForResponse((response) => {
        const url = new URL(response.url())
        return response.request().method() === 'POST'
          && url.pathname === `/api/v1/environments/${environmentId}/cancel`
      })
      await cancelButton.click()
      const accepted = await expectJson(await responsePromise, `${label}_CANCEL_ACCEPT_FAILED`)
      expect(accepted).toMatchObject({ environmentId, operationId: expect.any(String), statusUrl: expect.any(String) })
      // A cancellation operation reaches its own terminal `cancelled` state
      // after the provider has completed the requested cleanup. The
      // environment poll below remains authoritative for physical release.
      await waitForOperation(page, accepted, `${label}_CANCEL`, { allowCancelled: true })
    } catch (error) {
      // The control is intentionally optional: an operation may already be
      // past its cancellation window. Only a missing/hidden control falls
      // through to observation; a submitted request still fails loudly.
      if (!String(error?.message ?? error).includes('Timeout')) throw error
    }
  }

  return await pollJson(
    page.request,
    `/api/v1/environments/${encodeURIComponent(environmentId)}`,
    (value) => ['ready', 'stopped', 'failed', 'deleting', 'deleted'].includes(value.observedState),
    `${label}_SETTLE_FAILED`,
    OPERATION_TIMEOUT_MS,
  )
}

const LIFECYCLE_CONFIRMATION = Object.freeze({
  stop: { title: '停止环境', confirm: '停止' },
  restart: { title: '重启环境', confirm: '重启' },
})

/**
 * Stop and restart interrupt sessions, so the console asks first. Confirm the
 * visible dialog the way a user would; no request is sent before this click.
 */
export async function confirmEnvironmentLifecycleByUi(page, action) {
  const labels = LIFECYCLE_CONFIRMATION[action]
  if (!labels) throw new Error(`ENVIRONMENT_LIFECYCLE_CONFIRMATION_UNKNOWN:${action}`)
  const dialog = page.getByRole('alertdialog', { name: labels.title, exact: true })
  await expect(dialog).toBeVisible({ timeout: PAGE_TIMEOUT_MS })
  await dialog.getByRole('button', { name: labels.confirm, exact: true }).click()
  await expect(dialog).toBeHidden({ timeout: PAGE_TIMEOUT_MS })
}

/**
 * Stop an environment through the visible environment control bar. Reads and
 * operation polling remain API observations; the state-changing request is
 * emitted only by the rendered Stop button.
 */
export async function stopEnvironmentByUi(page, {
  routePrefix = 'student',
  projectId,
  environmentId,
  label = 'ENVIRONMENT_STOP',
} = {}) {
  if (!projectId || !environmentId) throw new Error(`${label}_CONTEXT_REQUIRED`)
  const current = await readEnvironment(page, environmentId, `${label}_READ_FAILED`)
  if (!current || current.observedState === 'deleted') return current
  if (current.observedState !== 'ready') return current

  await openEnvironmentPage(page, routePrefix, projectId, environmentId)
  const stopButton = page.getByRole('button', { name: '停止', exact: true })
  await expect(stopButton).toBeEnabled({ timeout: PAGE_TIMEOUT_MS })
  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && url.pathname === `/api/v1/environments/${environmentId}/stop`
  })
  await stopButton.click()
  await confirmEnvironmentLifecycleByUi(page, 'stop')
  const accepted = await expectJson(await responsePromise, `${label}_ACCEPT_FAILED`)
  expect(accepted).toMatchObject({ environmentId, operationId: expect.any(String), statusUrl: expect.any(String) })
  await waitForOperation(page, accepted, label)
  return await pollJson(
    page.request,
    `/api/v1/environments/${encodeURIComponent(environmentId)}`,
    (value) => ['stopped', 'deleted', 'failed'].includes(value.observedState),
    `${label}_ENVIRONMENT_STATUS_FAILED`,
    OPERATION_TIMEOUT_MS,
  )
}

/**
 * Stop when necessary and then delete/retry-reclaim an environment through
 * the visible lifecycle controls. This helper deliberately has no request
 * mutation fallback so acceptance cannot pass by bypassing the public UI.
 */
export async function deleteEnvironmentByUi(page, {
  routePrefix = 'student',
  projectId,
  environmentId,
  label = 'ENVIRONMENT_DELETE',
} = {}) {
  if (!projectId || !environmentId) throw new Error(`${label}_CONTEXT_REQUIRED`)
  let current = await readEnvironment(page, environmentId, `${label}_READ_FAILED`)
  if (!current || current.observedState === 'deleted') return current

  current = await waitForSettledEnvironment(
    page,
    routePrefix,
    projectId,
    environmentId,
    current,
    label,
    { cancelActive: true },
  )
  if (!current || current.observedState === 'deleted') return current

  if (current.observedState === 'ready') {
    current = await stopEnvironmentByUi(page, { routePrefix, projectId, environmentId, label: `${label}_STOP` })
    if (!current || current.observedState === 'deleted') return current
  }
  if (current.observedState === 'deleting') {
    return await waitForDeleted(page, environmentId, label)
  }
  if (current.desiredState === 'deleted' && current.observedState !== 'failed') {
    return await waitForDeleted(page, environmentId, label)
  }

  await openEnvironmentPage(page, routePrefix, projectId, environmentId)
  const deleteButton = page.getByRole('button', { name: /^(删除|重试回收)$/, exact: true })
  await expect(deleteButton).toBeEnabled({ timeout: PAGE_TIMEOUT_MS })
  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'DELETE'
      && url.pathname === `/api/v1/environments/${environmentId}`
  })
  await deleteButton.click()
  const dialog = page.getByRole('alertdialog')
  await expect(dialog).toBeVisible({ timeout: PAGE_TIMEOUT_MS })
  const confirm = dialog.getByRole('button', { name: /^(删除|重试回收)$/, exact: true })
  await expect(confirm).toBeEnabled()
  await confirm.click()
  const accepted = await expectJson(await responsePromise, `${label}_ACCEPT_FAILED`)
  expect(accepted).toMatchObject({ environmentId, operationId: expect.any(String), statusUrl: expect.any(String) })
  await waitForOperation(page, accepted, label)
  return await waitForDeleted(page, environmentId, label)
}

/**
 * Restore a deleted environment's retained disk through the visible "磁盘保留与恢复"
 * panel and its confirmation. The server accepts restore as one `restore`
 * operation that leaves a stopped instance on a newer generation, keeping the
 * original retention deadline. No request fallback exists.
 */
export async function restoreEnvironmentByUi(page, {
  routePrefix = 'student',
  projectId,
  environmentId,
  label = 'ENVIRONMENT_RESTORE',
} = {}) {
  if (!projectId || !environmentId) throw new Error(`${label}_CONTEXT_REQUIRED`)
  const before = await readEnvironment(page, environmentId, `${label}_READ_FAILED`)
  if (!before) throw new Error(`${label}_ENVIRONMENT_NOT_FOUND`)
  const disk = before.diskRetention
  if (before.desiredState !== 'deleted' || disk?.state !== 'retained') {
    throw new Error(`${label}_DISK_NOT_RETAINED:${disk?.state ?? 'none'}`)
  }

  await openEnvironmentPage(page, routePrefix, projectId, environmentId)
  const restoreButton = page.getByRole('button', { name: '恢复磁盘', exact: true })
  await expect(restoreButton).toBeEnabled({ timeout: PAGE_TIMEOUT_MS })
  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST'
      && url.pathname === `/api/v1/environments/${environmentId}/recover`
  })
  await restoreButton.click()
  const dialog = page.getByRole('alertdialog', { name: '恢复磁盘到新的运行代次', exact: true })
  await expect(dialog).toBeVisible({ timeout: PAGE_TIMEOUT_MS })
  await dialog.getByRole('button', { name: '恢复磁盘', exact: true }).click()
  const accepted = await expectJson(await responsePromise, `${label}_ACCEPT_FAILED`)
  expect(accepted).toMatchObject({ environmentId, operationId: expect.any(String), statusUrl: expect.any(String) })
  await waitForOperation(page, accepted, label)
  const restored = await pollJson(
    page.request,
    `/api/v1/environments/${encodeURIComponent(environmentId)}`,
    (value) => value.observedState === 'stopped' && value.diskRetention?.state === 'restored',
    `${label}_ENVIRONMENT_STATUS_FAILED`,
    OPERATION_TIMEOUT_MS,
  )
  if (!(restored.generation > before.generation)) throw new Error(`${label}_GENERATION_NOT_ADVANCED`)
  if (restored.diskRetention.expiresAt !== disk.expiresAt) throw new Error(`${label}_RETENTION_WINDOW_CHANGED`)
  return restored
}

