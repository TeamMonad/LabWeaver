import { expect } from '@playwright/test'
import { expectJson } from './live.mjs'

/** The Work capacity provider binding the platform configures for real Work runs. */
// See real-experiment.mjs: the shipped default targets the local development
// stack, while a live cluster registers its own binding.
export const WORK_PROVIDER_BINDING =
  process.env.LABWEAVER_E2E_PROVIDER_BINDING ?? 'kubernetes-work-local-hostpath'

const RESOURCE_PAGE_TIMEOUT_MS = 120_000
const RESOURCE_SETTLE_TIMEOUT_MS = 240_000
const RESOURCE_POLL_INTERVAL_MS = 5_000
const RESOURCE_APPROVAL_REASON = '平台管理员已核对资源申请的目标环境、发布版本与容量规格。'
const REQUEST_KEY_OPTION = /^([0-9a-fA-F-]{36}):(\d+)$/

/** Request states as the researcher resource page renders them. */
export const RESEARCHER_REQUEST_STATE = Object.freeze({
  reviewing: '待审批',
  allocating: '分配中',
  active: '已激活',
  expiring: '即将到期',
  expired: '已到期',
  rejected: '已拒绝',
  cancelled: '已取消',
})
/** Lease states as the researcher resource page renders them. */
export const RESEARCHER_LEASE_STATE = Object.freeze({
  allocating: '分配中',
  active: '有效',
  expiring: '即将到期',
  expired: '已到期',
  revoked: '已回收',
})
const RESEARCHER_REQUEST_TERMINAL_STATES = Object.freeze([
  RESEARCHER_REQUEST_STATE.expired,
  RESEARCHER_REQUEST_STATE.rejected,
  RESEARCHER_REQUEST_STATE.cancelled,
])
const RESEARCHER_LEASE_RELEASED_STATES = Object.freeze([
  RESEARCHER_LEASE_STATE.revoked,
  RESEARCHER_LEASE_STATE.expired,
])

/** Request and lease states as the admin resource-approval page renders them. */
export const ADMIN_REQUEST_STATE = Object.freeze({ reviewing: '待审批', allocating: '分配中', active: '使用中' })
export const ADMIN_LEASE_STATE = Object.freeze({ active: '使用中', expiring: '即将到期', expired: '已到期', revoked: '已撤销' })

/** Parse the zh-CN short-date/medium-time label the UI renders for timestamps. */
function parseVisibleTimestamp(label) {
  const match = typeof label === 'string'
    ? label.match(/(\d{4})\/(\d{1,2})\/(\d{1,2})\s+(\d{1,2}):(\d{2}):(\d{2})/)
    : null
  if (!match) return null
  const [, year, month, day, hour, minute, second] = match
  const value = new Date(Number(year), Number(month) - 1, Number(day), Number(hour), Number(minute), Number(second))
  return Number.isNaN(value.getTime()) ? null : value.toISOString()
}

/**
 * Wait until `read` reports a value the caller expects. Both resource pages
 * poll by themselves only while a request or lease is transitional and stop
 * when the tab is hidden, so every attempt re-navigates and re-reads the
 * rendered state instead of trusting a single snapshot.
 */
async function waitForRenderedState(page, { label, reload, read, expected, timeout = RESOURCE_SETTLE_TIMEOUT_MS }) {
  let latest = null
  try {
    await expect.poll(async () => {
      await reload()
      latest = await read()
      return latest !== null && expected(latest)
    }, { timeout, intervals: [RESOURCE_POLL_INTERVAL_MS] }).toBe(true)
  } catch (error) {
    throw new Error(`${label}:LW_ACCEPTANCE_STATE_TIMEOUT:${JSON.stringify(latest)}`, { cause: error })
  }
  return latest
}

/**
 * Open the researcher resource page for one project. The project identity
 * travels in the query so a later reload keeps the same scope.
 */
async function openResourcePage(page, { projectName, projectId }) {
  const target = projectId ?? null
  await page.goto(
    target ? `/researcher/resources?projectId=${encodeURIComponent(target)}` : '/researcher/resources',
    { waitUntil: 'domcontentloaded' },
  )
  await expect(page.getByRole('heading', { name: '资源申请', exact: true, level: 2 }))
    .toBeVisible({ timeout: RESOURCE_PAGE_TIMEOUT_MS })
  const select = page.locator('.project-strip select')
  await expect(select).toBeVisible({ timeout: RESOURCE_PAGE_TIMEOUT_MS })
  if (target) {
    const option = select.locator(`option[value="${target}"]`)
    await expect(option).toHaveCount(1, { timeout: RESOURCE_PAGE_TIMEOUT_MS })
    if (projectName) await expect(option).toContainText(projectName)
    await select.selectOption(target)
  } else {
    if (!projectName) throw new Error('LW_ACCEPTANCE_RESOURCE_PROJECT_REQUIRED')
    const option = select.locator('option').filter({ hasText: projectName })
    await expect(option).toHaveCount(1, { timeout: RESOURCE_PAGE_TIMEOUT_MS })
    const value = await option.getAttribute('value')
    if (!value) throw new Error(`LW_ACCEPTANCE_RESOURCE_PROJECT_OPTION_INVALID:${projectName}`)
    await select.selectOption(value)
  }
  return target
}

/**
 * Wait until both resource lists settled into rows or into their explicit empty
 * state, so a later read never mistakes "still loading" for "nothing there".
 */
async function waitForResourceLists(page) {
  for (const [heading, emptyText] of [
    ['requests-heading', '该项目暂无资源申请。'],
    ['leases-heading', '该项目暂无资源使用授权。'],
  ]) {
    const section = page.locator(`section[aria-labelledby="${heading}"]`)
    await expect.poll(
      async () => (await section.locator('.resource-row').count()) > 0
        || (await section.getByText(emptyText).isVisible()),
      { timeout: RESOURCE_PAGE_TIMEOUT_MS, intervals: [250, 500, 1000] },
    ).toBe(true)
  }
}

function researcherRequestRows(page) {
  return page.locator('section[aria-labelledby="requests-heading"] .resource-row')
}

function researcherLeaseRows(page) {
  return page.locator('section[aria-labelledby="leases-heading"] .resource-row')
}

async function readResearcherRequestRow(row) {
  const state = (await row.locator('.state-chip').textContent())?.trim() ?? ''
  const details = (await row.locator('.advanced-details small').first().textContent())?.trim() ?? ''
  const requestId = details.match(/申请 ID：([^\s·]+)/)?.[1] ?? null
  if (!requestId) throw new Error(`LW_ACCEPTANCE_RESOURCE_REQUEST_ID_UNREADABLE:${details || 'advanced details missing'}`)
  return { requestId, state }
}

async function readResearcherLeaseRow(row) {
  const state = (await row.locator('.state-chip').textContent())?.trim() ?? ''
  const expiresAtLabel = (await row.locator('.resource-row__main small').first().textContent())?.trim() ?? ''
  const details = (await row.locator('.advanced-details small').first().textContent())?.trim() ?? ''
  const leaseId = details.match(/Lease ID：([^\s·]+)/)?.[1] ?? null
  if (!leaseId) throw new Error(`LW_ACCEPTANCE_LEASE_ID_UNREADABLE:${details || 'advanced details missing'}`)
  const requestId = details.match(/申请 ID：([^\s·]+)/)?.[1] ?? null
  return { leaseId, requestId, state, expiresAt: parseVisibleTimestamp(expiresAtLabel), expiresAtLabel }
}

async function readResourceLists(page) {
  await waitForResourceLists(page)
  const requests = []
  for (const row of await researcherRequestRows(page).all()) {
    requests.push(await readResearcherRequestRow(row))
  }
  const leases = []
  for (const row of await researcherLeaseRows(page).all()) {
    leases.push(await readResearcherLeaseRow(row))
  }
  return { requests, leases }
}

/**
 * Submit one real resource request from the researcher resource page.
 *
 * `kind` selects the requested capacity: `cpu` requests CPU, memory, and
 * storage only, `gpu` additionally selects an active GPU catalog entry that
 * already has a current rate. A project without a published environment
 * template cannot express a request at all; that prerequisite gap fails with a
 * stable diagnostic instead of submitting a different request.
 */
export async function requestProjectResourceByUi(page, {
  projectName,
  kind = 'cpu',
  projectId = null,
  cpuMillicores = 1000,
  memoryGiB = 2,
  storageGiB = 10,
  durationHours = 1,
} = {}) {
  if (!['cpu', 'gpu'].includes(kind)) throw new Error(`LW_ACCEPTANCE_RESOURCE_KIND_UNSUPPORTED:${kind}`)
  const selectedProjectId = await openResourcePage(page, { projectName, projectId })
  await waitForResourceLists(page)
  const scope = projectName ?? selectedProjectId ?? 'unscoped'

  const releaseSelect = page.getByLabel('已发布版本')
  await expect(releaseSelect).toBeVisible({ timeout: RESOURCE_PAGE_TIMEOUT_MS })
  const releases = await releaseSelect.locator('option').evaluateAll((elements) => elements
    .map((element) => ({ value: element.value, label: (element.textContent ?? '').trim() }))
    .filter((option) => option.value !== ''))
  if (releases.length === 0) {
    await expect(page.getByText('当前项目没有可用的已发布版本。')).toBeVisible({ timeout: RESOURCE_PAGE_TIMEOUT_MS })
    throw new Error(`LW_ACCEPTANCE_RESOURCE_RELEASE_MISSING:${scope}`)
  }
  const release = releases[0]
  const releaseMatch = release.value.match(REQUEST_KEY_OPTION)
  if (!releaseMatch) throw new Error(`LW_ACCEPTANCE_RESOURCE_RELEASE_OPTION_INVALID:${release.value}`)
  const [, releaseId, releaseVersion] = releaseMatch
  await releaseSelect.selectOption(release.value)

  await page.getByLabel('CPU（m）').fill(String(cpuMillicores))
  await page.getByLabel('时长（小时）').fill(String(durationHours))
  await page.getByLabel('内存（GiB）').fill(String(memoryGiB))
  await page.getByLabel('存储（GiB）').fill(String(storageGiB))

  let gpuCatalogEntry = null
  if (kind === 'gpu') {
    const gpuSelect = page.getByLabel('GPU 目录项（可选）')
    const entries = await gpuSelect.locator('option').evaluateAll((elements) => elements
      .map((element) => ({ value: element.value, label: (element.textContent ?? '').trim() }))
      .filter((option) => option.value !== ''))
    if (entries.length === 0) throw new Error(`LW_ACCEPTANCE_GPU_CATALOG_EMPTY:${scope}`)
    gpuCatalogEntry = entries[0]
    await gpuSelect.selectOption(gpuCatalogEntry.value)
    const detail = page.locator('.gpu-detail')
    await expect(detail).toBeVisible({ timeout: RESOURCE_PAGE_TIMEOUT_MS })
    const detailText = (await detail.textContent()) ?? ''
    if (/尚未配置|冲突/.test(detailText)) throw new Error(`LW_ACCEPTANCE_GPU_RATE_MISSING:${gpuCatalogEntry.label}`)
  }

  const submitButton = page.getByRole('button', { name: '提交资源申请', exact: true })
  await expect(submitButton).toBeEnabled({ timeout: RESOURCE_PAGE_TIMEOUT_MS })
  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST' && url.pathname === '/api/v1/resource-requests'
  })
  await submitButton.click()
  const response = await responsePromise
  const accepted = await expectJson(response, 'LW_ACCEPTANCE_RESOURCE_REQUEST_CREATE_FAILED')
  const requestBody = response.request().postDataJSON()
  if (typeof accepted?.requestId !== 'string' || accepted.requestId === '') {
    throw new Error(`LW_ACCEPTANCE_RESOURCE_REQUEST_ID_MISSING:${scope}`)
  }
  const requestKey = requestBody?.requestKey
  const environmentId = requestBody?.target?.environmentId
  if (typeof requestKey !== 'string' || requestKey === '') {
    throw new Error(`LW_ACCEPTANCE_RESOURCE_REQUEST_KEY_MISSING:${accepted.requestId}`)
  }
  if (typeof environmentId !== 'string' || environmentId === '') {
    throw new Error(`LW_ACCEPTANCE_RESOURCE_ENVIRONMENT_ID_MISSING:${accepted.requestId}`)
  }
  if (
    requestBody?.target?.releaseId !== releaseId
    || String(requestBody?.target?.releaseVersion) !== releaseVersion
  ) {
    throw new Error(`LW_ACCEPTANCE_RESOURCE_REQUEST_RELEASE_MISMATCH:${requestBody?.target?.releaseId ?? 'missing'}`)
  }
  if (!Number.isInteger(requestBody?.durationSeconds) || requestBody.durationSeconds <= 0) {
    throw new Error(`LW_ACCEPTANCE_RESOURCE_REQUEST_DURATION_INVALID:${requestKey}`)
  }

  const requestRow = researcherRequestRows(page).filter({ hasText: requestKey })
  const rendered = await waitForRenderedState(page, {
    label: `LW_ACCEPTANCE_RESOURCE_REQUEST_STATE:${requestKey}`,
    reload: () => openResourcePage(page, { projectName, projectId: selectedProjectId }),
    read: async () => (await requestRow.count()) === 0 ? null : await readResearcherRequestRow(requestRow),
    expected: (value) => value.state === RESEARCHER_REQUEST_STATE.reviewing,
    timeout: RESOURCE_PAGE_TIMEOUT_MS,
  })

  return {
    projectId: selectedProjectId,
    requestId: accepted.requestId,
    requestKey,
    environmentId,
    releaseId,
    releaseVersion: Number(releaseVersion),
    durationSeconds: requestBody.durationSeconds,
    gpuCatalogEntry: gpuCatalogEntry?.label ?? null,
    state: rendered.state,
  }
}

function adminRequestRows(page) {
  return page.locator('.request-table tbody tr.data-table__row')
}

function adminDetailRow(page, detail, label) {
  return detail.locator('.meta-row').filter({ has: page.locator('.meta-label', { hasText: new RegExp(`^${label}$`) }) })
}

async function reloadAdminApprovalPage(page) {
  await page.reload({ waitUntil: 'domcontentloaded' })
  await expect(page.getByRole('heading', { name: '资源审批与资源使用授权管理', exact: true }))
    .toBeVisible({ timeout: RESOURCE_PAGE_TIMEOUT_MS })
  await expect.poll(
    async () => (await adminRequestRows(page).count()) > 0 || (await page.getByText('暂无资源申请').isVisible()),
    { timeout: RESOURCE_PAGE_TIMEOUT_MS, intervals: [250, 500, 1000] },
  ).toBe(true)
}

async function readAdminRequestState(row) {
  return (await row.locator('.gcp-status-pill .status-label').textContent())?.trim() ?? ''
}

async function readAdminLeaseDetail(page, { requestId, leaseId }) {
  const row = page.locator('.lease-table tbody tr.data-table__row').filter({ hasText: requestId })
  if ((await row.count()) === 0) return null
  await row.first().click()
  const detail = page.locator('.lease-detail')
  await expect(detail).toBeVisible({ timeout: RESOURCE_PAGE_TIMEOUT_MS })
  const renderedLeaseId = (await adminDetailRow(page, detail, '资源使用授权 ID').locator('.meta-value').textContent())?.trim() ?? ''
  if (renderedLeaseId !== leaseId) throw new Error(`LW_ACCEPTANCE_ADMIN_LEASE_ID_MISMATCH:${renderedLeaseId || 'missing'}`)
  const state = (await adminDetailRow(page, detail, '状态').locator('.gcp-status-pill .status-label').textContent())?.trim() ?? ''
  const expiresAtLabel = (await adminDetailRow(page, detail, '到期时间').locator('.meta-value').textContent())?.trim() ?? ''
  return { leaseId: renderedLeaseId, state, expiresAt: parseVisibleTimestamp(expiresAtLabel), expiresAtLabel }
}

/**
 * Approve one real resource request on the admin resource-approval page and
 * read the settled request and lease back from the rendered page.
 *
 * The admin surface is global rather than project-scoped, so the request is
 * selected by its user-visible request key; every identity the caller can
 * cross-check (request, project, requester, target environment) is verified on
 * the detail pane before the approval is submitted.
 */
export async function approveResourceRequestByUi(page, {
  requestKey,
  projectName = null,
  projectId = null,
  requestId = null,
  environmentId = null,
  requesterId = null,
  durationSeconds = null,
  providerBinding = WORK_PROVIDER_BINDING,
} = {}) {
  if (typeof requestKey !== 'string' || requestKey === '') {
    throw new Error('LW_ACCEPTANCE_RESOURCE_REQUEST_KEY_REQUIRED')
  }
  if (!Number.isInteger(durationSeconds) || durationSeconds <= 0) {
    throw new Error(`LW_ACCEPTANCE_RESOURCE_DURATION_REQUIRED:${requestKey}`)
  }
  const scope = projectName ?? projectId ?? requestKey

  await page.goto('/admin/resource-approval', { waitUntil: 'domcontentloaded' })
  await expect(page.getByRole('heading', { name: '资源审批与资源使用授权管理', exact: true }))
    .toBeVisible({ timeout: RESOURCE_PAGE_TIMEOUT_MS })
  const row = adminRequestRows(page).filter({ hasText: requestKey })
  await expect(row).toHaveCount(1, { timeout: RESOURCE_PAGE_TIMEOUT_MS })
  await row.click()

  const detail = page.locator('.request-detail')
  await expect(detail).toBeVisible({ timeout: RESOURCE_PAGE_TIMEOUT_MS })
  await expect(detail).toContainText(requestKey)
  if (requestId) await expect(detail).toContainText(requestId)
  if (projectId) await expect(adminDetailRow(page, detail, '课程 / 项目').locator('.meta-value')).toContainText(projectId)
  if (requesterId) await expect(adminDetailRow(page, detail, '申请人').locator('.meta-value')).toContainText(requesterId)
  if (environmentId) await expect(adminDetailRow(page, detail, '目标').locator('.meta-value')).toContainText(environmentId)
  await expect(adminDetailRow(page, detail, '状态').locator('.gcp-status-pill .status-label'))
    .toHaveText(ADMIN_REQUEST_STATE.reviewing)

  await page.getByLabel('资源申请操作理由', { exact: true }).fill(RESOURCE_APPROVAL_REASON)
  const provider = detail.locator('#provider-binding')
  await expect(provider).toHaveCount(1)
  if (await provider.evaluate((element) => element.tagName === 'SELECT')) await provider.selectOption(providerBinding)
  else await provider.fill(providerBinding)
  const duration = detail.locator('#approve-duration')
  await expect(duration).toHaveCount(1)
  await duration.fill(String(durationSeconds))

  const approveButton = detail.getByRole('button', { name: '批准', exact: true })
  await expect(approveButton).toBeEnabled()
  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST' && /^\/api\/v1\/resource-requests\/[^/]+\/approve$/.test(url.pathname)
  })
  await approveButton.click()
  const dialog = page.locator('dialog.confirm-dialog[role="alertdialog"]')
  await expect(dialog).toBeVisible()
  await dialog.locator('.filled-button').click()

  const approval = await expectJson(await responsePromise, 'LW_ACCEPTANCE_RESOURCE_REQUEST_APPROVAL_FAILED')
  if (typeof approval?.requestId !== 'string' || approval.requestId === '') {
    throw new Error(`LW_ACCEPTANCE_RESOURCE_REQUEST_APPROVAL_ID_MISSING:${scope}`)
  }
  if (typeof approval.leaseId !== 'string' || approval.leaseId === '') {
    throw new Error(`LW_ACCEPTANCE_RESOURCE_LEASE_ID_MISSING:${scope}`)
  }
  if (requestId && approval.requestId !== requestId) {
    throw new Error(`LW_ACCEPTANCE_RESOURCE_REQUEST_APPROVAL_MISMATCH:${approval.requestId}`)
  }

  const settledRequestState = await waitForRenderedState(page, {
    label: `LW_ACCEPTANCE_ADMIN_REQUEST_STATE:${scope}`,
    reload: () => reloadAdminApprovalPage(page),
    read: async () => {
      const current = adminRequestRows(page).filter({ hasText: requestKey })
      return (await current.count()) === 0 ? null : await readAdminRequestState(current)
    },
    expected: (value) => value === ADMIN_REQUEST_STATE.active,
  })
  const lease = await waitForRenderedState(page, {
    label: `LW_ACCEPTANCE_ADMIN_LEASE_STATE:${scope}`,
    reload: () => reloadAdminApprovalPage(page),
    read: () => readAdminLeaseDetail(page, { requestId: approval.requestId, leaseId: approval.leaseId }),
    expected: (value) => value.state === ADMIN_LEASE_STATE.active && value.expiresAt !== null,
  })

  return {
    requestId: approval.requestId,
    leaseId: approval.leaseId,
    requestState: settledRequestState,
    leaseState: lease.state,
    expiresAt: lease.expiresAt,
    expiresAtLabel: lease.expiresAtLabel,
  }
}

/**
 * Read the lease the project currently holds back from the researcher resource
 * page: the rendered state, the rendered expiry, and the lease identity.
 */
export async function readBackLeaseByUi(page, { projectName, projectId = null, leaseId = null, requestId = null } = {}) {
  await openResourcePage(page, { projectName, projectId })
  const lists = await readResourceLists(page)
  const matched = leaseId
    ? lists.leases.find((item) => item.leaseId === leaseId)
    : requestId
      ? lists.leases.find((item) => item.requestId === requestId)
      : lists.leases[0]
  if (!matched) {
    await expect(page.getByText('该项目暂无资源使用授权。')).toBeVisible({ timeout: RESOURCE_PAGE_TIMEOUT_MS })
    throw new Error(`LW_ACCEPTANCE_LEASE_NOT_VISIBLE:${projectName ?? projectId ?? 'project'}${leaseId ? `:${leaseId}` : requestId ? `:${requestId}` : ''}`)
  }
  return matched
}

/**
 * Release the project lease through the researcher resource page: the reclaim
 * action, its confirmation dialog, and the terminal rendered state.
 */
export async function releaseProjectLeaseByUi(page, { projectName, projectId = null, leaseId = null, requestId = null } = {}) {
  await openResourcePage(page, { projectName, projectId })
  const lists = await readResourceLists(page)
  const candidates = leaseId
    ? lists.leases.filter((item) => item.leaseId === leaseId)
    : requestId
      ? lists.leases.filter((item) => item.requestId === requestId)
      : lists.leases.filter((item) => !RESEARCHER_LEASE_RELEASED_STATES.includes(item.state))
  if (candidates.length === 0) {
    if (leaseId || requestId) {
      throw new Error(`LW_ACCEPTANCE_LEASE_NOT_VISIBLE:${leaseId ?? requestId}`)
    }
    await expect(page.getByText('该项目暂无资源使用授权。')).toBeVisible({ timeout: RESOURCE_PAGE_TIMEOUT_MS })
    throw new Error(`LW_ACCEPTANCE_LEASE_NOT_VISIBLE:${projectName ?? projectId ?? 'project'}`)
  }
  if (candidates.length > 1) {
    throw new Error(`LW_ACCEPTANCE_LEASE_AMBIGUOUS:${candidates.map((item) => item.leaseId).join(',')}`)
  }
  const matched = candidates[0]
  if (RESEARCHER_LEASE_RELEASED_STATES.includes(matched.state)) return { ...matched, released: false }

  const row = researcherLeaseRows(page).filter({ hasText: matched.leaseId })
  const releaseButton = row.getByRole('button', { name: '回收', exact: true })
  await expect(releaseButton).toBeEnabled({ timeout: RESOURCE_PAGE_TIMEOUT_MS })
  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST' && url.pathname === `/api/v1/resource-leases/${matched.leaseId}/revoke`
  })
  await releaseButton.click()
  const dialog = page.locator('dialog.confirm-dialog[role="alertdialog"]')
  await expect(dialog).toBeVisible()
  await dialog.locator('.filled-button').click()
  await expectJson(await responsePromise, 'LW_ACCEPTANCE_RESOURCE_LEASE_RELEASE_FAILED')

  const settled = await waitForRenderedState(page, {
    label: `LW_ACCEPTANCE_RESOURCE_LEASE_RELEASE:${matched.leaseId}`,
    reload: () => openResourcePage(page, { projectName, projectId }),
    read: async () => {
      const listsAfter = await readResourceLists(page)
      return listsAfter.leases.find((item) => item.leaseId === matched.leaseId) ?? null
    },
    expected: (value) => value.state === RESEARCHER_LEASE_STATE.revoked,
  })
  return { ...settled, released: true }
}

/**
 * Read the finance page for one project and report whether real cost lines are
 * rendered or the page states explicitly that there is no usage yet. A
 * fabricated zero is never acceptable.
 */
export async function assertProjectChargesByUi(page, project) {
  await page.goto('/admin/resource-finance', { waitUntil: 'domcontentloaded' })
  await expect(page.getByRole('heading', { name: '预算与费用', exact: true })).toBeVisible({ timeout: RESOURCE_PAGE_TIMEOUT_MS })
  const projectSelect = page.locator('.project-strip select')
  await expect(projectSelect.locator(`option[value="${project.id}"]`)).toHaveCount(1, { timeout: RESOURCE_PAGE_TIMEOUT_MS })
  await projectSelect.selectOption(project.id)
  const charges = page.locator('.charge-row')
  const noCharges = page.getByText('该项目暂无费用记录。')
  await expect.poll(
    async () => (await charges.count()) > 0 || (await noCharges.isVisible()),
    { timeout: RESOURCE_PAGE_TIMEOUT_MS, intervals: [500, 1000, 2000] },
  ).toBe(true)
  if (await charges.count() === 0) {
    await expect(noCharges).toBeVisible()
    return { kind: 'no-usage-yet' }
  }

  const firstCharge = charges.first()
  const total = ((await firstCharge.locator('.charge-main > strong').textContent()) ?? '').trim()
  expect(total, 'LW_ACCEPTANCE_FINANCE_CHARGE_TOTAL_INVALID').toMatch(/^\d+\.\d{6}\s+USD$/)
  const settlement = ((await firstCharge.locator('.charge-actions .state-chip').textContent()) ?? '').trim()
  expect(['已结算', '待结算', '未结算'], 'LW_ACCEPTANCE_FINANCE_SETTLEMENT_INVALID').toContain(settlement)
  if (settlement === '已结算') {
    expect(Number(total.split(/\s+/)[0]), 'LW_ACCEPTANCE_FINANCE_SETTLED_TOTAL_NOT_POSITIVE').toBeGreaterThan(0)
  }
  const budget = page.locator('.budget-summary')
  if (await budget.count() === 0) {
    await expect(page.getByText('该项目还没有预算记录。')).toBeVisible()
    return { kind: 'usage', total, settlement, spent: null }
  }
  const spent = ((await budget.locator('div').filter({ hasText: '已花费' }).locator('strong').textContent()) ?? '').trim()
  expect(spent, 'LW_ACCEPTANCE_FINANCE_SPENT_INVALID').toMatch(/^\d+\.\d{6}\s+USD$/)
  return { kind: 'usage', total, settlement, spent }
}

/**
 * The GPU catalog must render real entries; an empty catalog must show its
 * explicit empty state instead of an empty table that looks like success.
 */
export async function assertGpuCatalogByUi(page) {
  await page.goto('/admin/gpu-catalog', { waitUntil: 'domcontentloaded' })
  await expect(page.getByRole('heading', { name: 'GPU 目录', exact: true, level: 2 })).toBeVisible({ timeout: RESOURCE_PAGE_TIMEOUT_MS })
  const rows = page.locator('.catalog-table tbody tr.data-table__row')
  const emptyCatalog = page.getByText('GPU 目录为空。')
  await expect.poll(
    async () => (await rows.count()) > 0 || (await emptyCatalog.isVisible()),
    { timeout: RESOURCE_PAGE_TIMEOUT_MS, intervals: [500, 1000, 2000] },
  ).toBe(true)
  if (await rows.count() === 0) {
    await expect(emptyCatalog).toBeVisible()
    return { kind: 'empty' }
  }
  await expect(page.getByRole('region', { name: 'GPU 目录' })).toBeVisible()
  const firstRow = rows.first()
  await expect(firstRow.locator('code').first()).toHaveText(/^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/)
  await expect(firstRow.locator('.state-chip')).toHaveText(/可用|已停用/)
  return { kind: 'catalog', rows: await rows.count() }
}

export async function cancelProjectResourceRequestByUi(page, { projectName, projectId = null, requestKey } = {}) {
  if (typeof requestKey !== 'string' || requestKey === '') {
    throw new Error('LW_ACCEPTANCE_RESOURCE_REQUEST_KEY_REQUIRED')
  }
  await openResourcePage(page, { projectName, projectId })
  await waitForResourceLists(page)
  const row = researcherRequestRows(page).filter({ hasText: requestKey })
  await expect(row).toHaveCount(1, { timeout: RESOURCE_PAGE_TIMEOUT_MS })
  const rendered = await readResearcherRequestRow(row)
  if (RESEARCHER_REQUEST_TERMINAL_STATES.includes(rendered.state)) {
    return { ...rendered, requestKey, cancelled: false }
  }
  if ([RESEARCHER_REQUEST_STATE.active, RESEARCHER_REQUEST_STATE.expiring].includes(rendered.state)) {
    throw new Error(`LW_ACCEPTANCE_RESOURCE_REQUEST_ACTIVE:${requestKey}`)
  }

  const cancelButton = row.getByRole('button', { name: '取消', exact: true })
  await expect(cancelButton).toBeEnabled({ timeout: RESOURCE_PAGE_TIMEOUT_MS })
  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST' && url.pathname === `/api/v1/resource-requests/${rendered.requestId}/cancel`
  })
  await cancelButton.click()
  const dialog = page.locator('dialog.confirm-dialog[role="alertdialog"]')
  await expect(dialog).toBeVisible()
  await dialog.locator('.filled-button').click()
  await expectJson(await responsePromise, 'LW_ACCEPTANCE_RESOURCE_REQUEST_CANCEL_FAILED')

  const settled = await waitForRenderedState(page, {
    label: `LW_ACCEPTANCE_RESOURCE_REQUEST_CANCEL:${requestKey}`,
    reload: () => openResourcePage(page, { projectName, projectId }),
    read: async () => {
      const current = researcherRequestRows(page).filter({ hasText: requestKey })
      return (await current.count()) === 0 ? null : await readResearcherRequestRow(current)
    },
    expected: (value) => value.state === RESEARCHER_REQUEST_STATE.cancelled,
  })
  return { ...settled, requestKey, cancelled: true }
}
