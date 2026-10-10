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
    const operationsTab = page.getByRole('button', { name: '异步操作与诊断', exact: true })
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

