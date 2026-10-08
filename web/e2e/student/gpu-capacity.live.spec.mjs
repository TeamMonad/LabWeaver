import { expect, test } from '@playwright/test'
import {
  AUTH_STATE,
  expectJson,
  navigateFromHomeByUi,
  pollJson,
  selectProjectByUi,
} from '../support/live.mjs'
import {
  approveResourceRequestByUi,
  requestProjectResourceByUi,
} from '../support/real-resource.mjs'
import {
  cleanupWorkResources,
  inspectRealWorkFinanceByUi,
  readResourceRates,
  waitForDeletedEnvironment,
  waitForSettledWorkUsageCharges,
} from '../support/real-work.mjs'
import {
  cudaProbeTerminalCommand,
  parseCudaProbeResult,
  typeTerminalCommand,
} from '../support/real-gpu.mjs'
import { readActorId } from '../support/real-experiment.mjs'

const CLASS_ENV = 'LABWEAVER_E2E_GPU_CAPACITY_CLASS'
const PROJECT_ENV = 'LABWEAVER_E2E_GPU_CAPACITY_PROJECT_ID'
const RELEASE_ENV = 'LABWEAVER_E2E_GPU_CAPACITY_RELEASE_ID'
const RELEASE_VERSION_ENV = 'LABWEAVER_E2E_GPU_CAPACITY_RELEASE_VERSION'
const GIB = 1024 ** 3
const EXPECTED_CAPACITY_FAILURE = 'LW_RESOURCE_GPU_CAPACITY_EXHAUSTED'
const ACTIVE_REQUEST_STATES = new Set(['allocating', 'active', 'expiring'])
const ACTIVE_LEASE_STATES = new Set(['allocating', 'active', 'expiring'])
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i
const GPU_CLASS = /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/

const INPUT = Object.freeze({
  class: process.env[CLASS_ENV]?.trim() ?? '',
  projectId: process.env[PROJECT_ENV]?.trim() ?? '',
  releaseId: process.env[RELEASE_ENV]?.trim() ?? '',
  releaseVersion: process.env[RELEASE_VERSION_ENV]?.trim() ?? '',
})
const PROVIDED_INPUTS = Object.values(INPUT).filter(Boolean).length

if (PROVIDED_INPUTS !== 0 && PROVIDED_INPUTS !== Object.keys(INPUT).length) {
  throw new Error('LW_GPU_CAPACITY_INPUT_INCOMPLETE')
}
if (INPUT.projectId && !UUID.test(INPUT.projectId)) {
  throw new Error('LW_GPU_CAPACITY_PROJECT_ID_INVALID')
}
if (INPUT.releaseId && !UUID.test(INPUT.releaseId)) {
  throw new Error('LW_GPU_CAPACITY_RELEASE_ID_INVALID')
}
if (INPUT.releaseVersion && !/^[1-9][0-9]*$/.test(INPUT.releaseVersion)) {
  throw new Error('LW_GPU_CAPACITY_RELEASE_VERSION_INVALID')
}
if (INPUT.class && !GPU_CLASS.test(INPUT.class)) {
  throw new Error('LW_GPU_CAPACITY_CLASS_INVALID')
}

test.describe.configure({ retries: 0 })
test.skip(PROVIDED_INPUTS === 0, `set ${PROJECT_ENV}, ${RELEASE_ENV}, ${RELEASE_VERSION_ENV}, and ${CLASS_ENV} to run the owned GPU capacity scenario`)

function releaseVersion() {
  const value = Number(INPUT.releaseVersion)
  if (!Number.isSafeInteger(value) || value < 1) throw new Error('LW_GPU_CAPACITY_RELEASE_VERSION_INVALID')
  return value
}

function bytesToGiB(value, label) {
  if (!Number.isSafeInteger(value) || value < GIB || value % GIB !== 0) {
    throw new Error(`LW_GPU_CAPACITY_RELEASE_${label.toUpperCase()}_NOT_WHOLE_GIB`)
  }
  return value / GIB
}

async function readActiveGpuCatalogEntry(request) {
  const response = await request.get('/api/v1/resource/gpu-catalog')
  const entries = await expectJson(response, 'LW_GPU_CAPACITY_CATALOG_READ_FAILED')
  if (!Array.isArray(entries)) throw new Error('LW_GPU_CAPACITY_CATALOG_INVALID')
  const matching = entries.filter((entry) => entry.class === INPUT.class && entry.active === true)
  if (matching.length !== 1) throw new Error(`LW_GPU_CAPACITY_CATALOG_ENTRY_AMBIGUOUS:${INPUT.class}:${matching.length}`)
  const entry = matching[0]
  if (
    entry.mode !== 'exclusive'
    || entry.capacityUnits !== 1
    || typeof entry.providerBinding !== 'string'
    || entry.providerBinding.trim() === ''
    || typeof entry.allocationBinding !== 'string'
    || entry.allocationBinding.trim() === ''
  ) {
    throw new Error(`LW_GPU_CAPACITY_EXCLUSIVE_ONE_UNIT_REQUIRED:${INPUT.class}`)
  }
  return { entry, entries }
}

async function readPublishedWorkRelease(request, entry) {
  const release = await expectJson(
    await request.get(`/api/v1/projects/${encodeURIComponent(INPUT.projectId)}/environment-template-releases/${encodeURIComponent(INPUT.releaseId)}`),
    'LW_GPU_CAPACITY_RELEASE_READ_FAILED',
  )
  if (
    release.id !== INPUT.releaseId
    || release.projectId !== INPUT.projectId
    || release.version !== releaseVersion()
    || release.withdrawal
    || release.runtimeKind !== 'container'
    || release.artifact?.kind !== 'container'
    || typeof release.candidateId !== 'string'
    || !Number.isSafeInteger(release.candidateRevision)
  ) {
    throw new Error('LW_GPU_CAPACITY_RELEASE_IDENTITY_INVALID')
  }

  const view = await expectJson(
    await request.get(`/api/v1/projects/${encodeURIComponent(INPUT.projectId)}/environment-candidates/${encodeURIComponent(release.candidateId)}`),
    'LW_GPU_CAPACITY_RELEASE_CANDIDATE_READ_FAILED',
  )
  const candidate = view.candidate
  if (
    candidate?.id !== release.candidateId
    || candidate.projectId !== INPUT.projectId
    || candidate.revision !== release.candidateRevision
    || candidate.spec?.class !== 'work'
    || candidate.spec?.runtime?.kind !== 'container'
    || candidate.spec.runtime.provider_binding !== entry.providerBinding
    || !candidate.spec.runtime.terminal
    || candidate.spec.resources?.gpu?.class !== entry.class
    || candidate.spec.resources.gpu.count !== 1
    || release.approval?.decision !== 'approved'
    || release.approval.candidateId !== candidate.id
    || release.approval.candidateRevision !== candidate.revision
  ) {
    throw new Error('LW_GPU_CAPACITY_RELEASE_WORK_GPU_BINDING_INVALID')
  }
  expect(view.imageArtifact, 'LW_GPU_CAPACITY_RELEASE_ARTIFACT_MISMATCH').toEqual(release.artifact)

  const resources = candidate.spec.resources
  if (!Number.isSafeInteger(resources?.cpuMillicores) || resources.cpuMillicores <= 0) {
    throw new Error('LW_GPU_CAPACITY_RELEASE_CPU_REQUIREMENT_INVALID')
  }
  return Object.freeze({
    releaseId: release.id,
    releaseVersion: release.version,
    runtimeKind: release.runtimeKind,
    providerBinding: entry.providerBinding,
    workload: Object.freeze({
      cpuMillicores: resources.cpuMillicores,
      memoryGiB: bytesToGiB(resources.memoryBytes, 'MEMORY'),
      storageGiB: bytesToGiB(resources.storageBytes, 'STORAGE'),
      durationHours: 1,
    }),
  })
}

async function readGlobalResourceState(request) {
  const [requestsResponse, leasesResponse] = await Promise.all([
    request.get('/api/v1/resource-requests'),
    request.get('/api/v1/resource-leases'),
  ])
  const [requests, leases] = await Promise.all([
    expectJson(requestsResponse, 'LW_GPU_CAPACITY_GLOBAL_REQUEST_READ_FAILED'),
    expectJson(leasesResponse, 'LW_GPU_CAPACITY_GLOBAL_LEASE_READ_FAILED'),
  ])
  if (!Array.isArray(requests) || !Array.isArray(leases)) throw new Error('LW_GPU_CAPACITY_GLOBAL_STATE_INVALID')
  return { requests, leases }
}

function assertAllocationBindingIdle({ requests, leases }, catalogEntries, selectedEntry) {
  const entryByClass = new Map()
  for (const entry of catalogEntries.filter((candidate) => candidate.active === true)) {
    if (entryByClass.has(entry.class)) throw new Error(`LW_GPU_CAPACITY_ACTIVE_CATALOG_CLASS_AMBIGUOUS:${entry.class}`)
    entryByClass.set(entry.class, entry)
  }
  const requestById = new Map(requests.map((request) => [request.id, request]))
  const heldOrPending = requests.filter((request) => (
    request.state === 'reviewing' || ACTIVE_REQUEST_STATES.has(request.state)
  )).filter((request) => {
    const gpuClass = request.requestedResources?.gpu?.class
    if (!gpuClass) return false
    const entry = entryByClass.get(gpuClass)
    if (!entry) throw new Error(`LW_GPU_CAPACITY_CATALOG_CLASS_UNKNOWN:${gpuClass}`)
    return entry.allocationBinding === selectedEntry.allocationBinding
  })
  const activeLeases = leases.filter((lease) => ACTIVE_LEASE_STATES.has(lease.state)).filter((lease) => {
    const request = requestById.get(lease.requestId)
    if (!request) throw new Error(`LW_GPU_CAPACITY_LEASE_REQUEST_MISSING:${lease.id}:${lease.requestId}`)
    const gpuClass = request.requestedResources?.gpu?.class
    if (!gpuClass) return false
    const entry = entryByClass.get(gpuClass)
    if (!entry) throw new Error(`LW_GPU_CAPACITY_CATALOG_CLASS_UNKNOWN:${gpuClass}`)
    return entry.allocationBinding === selectedEntry.allocationBinding
  })
  if (heldOrPending.length || activeLeases.length) {
    const requestsDetail = heldOrPending.map((request) => `${request.id}:${request.state}:${request.projectId}`).join(',') || 'none'
    const leasesDetail = activeLeases.map((lease) => `${lease.id}:${lease.state}:${lease.requestId}`).join(',') || 'none'
    throw new Error(`LW_GPU_CAPACITY_ALLOCATION_BINDING_BUSY:${selectedEntry.allocationBinding}:requests=${requestsDetail}:leases=${leasesDetail}`)
  }
}

async function readProjectResources(request) {
  const [requestsResponse, leasesResponse] = await Promise.all([
    request.get(`/api/v1/projects/${encodeURIComponent(INPUT.projectId)}/resource-requests`),
    request.get(`/api/v1/projects/${encodeURIComponent(INPUT.projectId)}/resource-leases`),
  ])
  const [requests, leases] = await Promise.all([
    expectJson(requestsResponse, 'LW_GPU_CAPACITY_PROJECT_REQUEST_READ_FAILED'),
    expectJson(leasesResponse, 'LW_GPU_CAPACITY_PROJECT_LEASE_READ_FAILED'),
  ])
  if (!Array.isArray(requests) || !Array.isArray(leases)) throw new Error('LW_GPU_CAPACITY_PROJECT_STATE_INVALID')
  return { requests, leases }
}

async function readProjectRequest(request, requestId) {
  return await expectJson(
    await request.get(`/api/v1/resource-requests/${encodeURIComponent(requestId)}`),
    `LW_GPU_CAPACITY_REQUEST_READ_FAILED:${requestId}`,
  )
}

async function tryReadEnvironment(request, environmentId) {
  const response = await request.get(`/api/v1/environments/${encodeURIComponent(environmentId)}`)
  if (response.status() === 404) return null
  if (!response.ok()) throw new Error(`LW_GPU_CAPACITY_ENVIRONMENT_READ_FAILED:${environmentId}:${response.status()}`)
  const environment = await expectJson(response, 'LW_GPU_CAPACITY_ENVIRONMENT_READ_FAILED')
  if (environment?.id !== environmentId || environment.projectId !== INPUT.projectId) {
    throw new Error(`LW_GPU_CAPACITY_ENVIRONMENT_IDENTITY_MISMATCH:${environmentId}`)
  }
  return environment
}

async function waitForEnvironmentReady(request, environmentId, release) {
  const value = await pollJson(
    request,
    `/api/v1/environments/${encodeURIComponent(environmentId)}`,
    (environment) => environment.observedState === 'ready' || ['failed', 'deleted'].includes(environment.observedState),
    `LW_GPU_CAPACITY_ENVIRONMENT_READY_STATUS_FAILED:${environmentId}`,
    240_000,
  )
  if (value.observedState !== 'ready') throw new Error(`LW_GPU_CAPACITY_ENVIRONMENT_NOT_READY:${environmentId}:${value.observedState}`)
  expect(value).toMatchObject({
    id: environmentId,
    projectId: INPUT.projectId,
    class: 'work',
    runtimeKind: release.runtimeKind,
    providerBinding: release.providerBinding,
    releaseId: release.releaseId,
    releaseVersion: release.releaseVersion,
  })
  return value
}

async function waitForReleasedResource(request, owned) {
  await expect.poll(async () => {
    const resources = await readProjectResources(request)
    const currentRequest = resources.requests.find((item) => item.id === owned.requestId)
    const currentLease = resources.leases.find((item) => item.id === owned.leaseId)
    return currentRequest?.state === 'expired'
      && currentLease?.requestId === owned.requestId
      && ['revoked', 'expired'].includes(currentLease?.state)
  }, { timeout: 240_000, intervals: [1000, 2000, 5000] }).toBe(true)
}

async function assertHeldResource(request, owned, lease, environment, release, ownerId) {
  const resources = await readProjectResources(request)
  const currentRequest = resources.requests.find((item) => item.id === owned.requestId)
  const currentLease = resources.leases.find((item) => item.id === lease.leaseId)
  expect(currentRequest, `LW_GPU_CAPACITY_REQUEST_NOT_ACTIVE:${owned.requestId}`).toMatchObject({
    id: owned.requestId,
    requesterId: ownerId,
    projectId: INPUT.projectId,
    target: {
      kind: 'environment',
      environmentId: owned.environmentId,
      releaseId: release.releaseId,
      releaseVersion: release.releaseVersion,
    },
    state: 'active',
    requestedResources: { gpu: { class: INPUT.class, count: 1 } },
  })
  expect(currentLease, `LW_GPU_CAPACITY_LEASE_NOT_ACTIVE:${owned.requestId}`).toMatchObject({
    id: lease.leaseId,
    requestId: owned.requestId,
    state: 'active',
    claimId: expect.stringMatching(UUID),
  })
  expect(environment).toMatchObject({
    id: owned.environmentId,
    ownerId,
    projectId: INPUT.projectId,
    class: 'work',
    observedState: 'ready',
    runtimeKind: release.runtimeKind,
    providerBinding: release.providerBinding,
    releaseId: release.releaseId,
    releaseVersion: release.releaseVersion,
    leaseId: lease.leaseId,
    capacityBinding: currentLease.claimId,
  })
}

async function assertBlockedRequest(request, contender, primaryLease) {
  const resources = await readProjectResources(request)
  const blocked = resources.requests.find((item) => item.id === contender.requestId)
  expect(blocked, `LW_GPU_CAPACITY_BLOCKED_REQUEST_MISSING:${contender.requestId}`).toMatchObject({
    id: contender.requestId,
    projectId: INPUT.projectId,
    state: 'reviewing',
    requestedResources: { gpu: { class: INPUT.class, count: 1 } },
  })
  const requestById = new Map(resources.requests.map((item) => [item.id, item]))
  const activeLeases = resources.leases
    .filter((item) => ACTIVE_LEASE_STATES.has(item.state))
    .filter((item) => requestById.get(item.requestId)?.requestedResources?.gpu?.class === INPUT.class)
  expect(activeLeases, 'LW_GPU_CAPACITY_DUPLICATE_ACTIVE_LEASE').toHaveLength(1)
  expect(activeLeases[0]).toMatchObject({ id: primaryLease.leaseId, requestId: primaryLease.requestId })
  expect(activeLeases.some((item) => item.requestId === contender.requestId)).toBe(false)
  const contenderEnvironment = await tryReadEnvironment(request, contender.environmentId)
  if (contenderEnvironment) {
    expect(contenderEnvironment.observedState).not.toBe('ready')
    expect(contenderEnvironment.leaseId ?? null).not.toBe(primaryLease.leaseId)
  }
}

async function requestGpu(page, entry, release, onAccepted) {
  return await requestProjectResourceByUi(page, {
    projectId: INPUT.projectId,
    kind: 'gpu',
    gpuClass: entry.class,
    gpuMode: entry.mode,
    gpuCount: 1,
    releaseId: release.releaseId,
    releaseVersion: release.releaseVersion,
    ...release.workload,
    onAccepted,
  })
}

async function approveGpuRequest(page, request, entry, expectedFailureCode = null, onAccepted = null) {
  return await approveResourceRequestByUi(page, {
    requestKey: request.requestKey,
    projectId: INPUT.projectId,
    requestId: request.requestId,
    environmentId: request.environmentId,
    durationSeconds: request.durationSeconds,
    providerBinding: entry.providerBinding,
    expectedFailureCode,
    onAccepted,
  })
}

async function readActiveGrantIds(request, environmentId) {
  const response = await request.get(`/api/v1/environments/${encodeURIComponent(environmentId)}/access-grants?state=active&includeTerminal=false&limit=10`)
  if (response.status() === 404) return []
  const body = await expectJson(response, `LW_GPU_CAPACITY_ACCESS_GRANTS_READ_FAILED:${environmentId}`)
  if (!Array.isArray(body.items)) throw new Error(`LW_GPU_CAPACITY_ACCESS_GRANTS_INVALID:${environmentId}`)
  return body.items.map((item) => item.id).filter((id) => typeof id === 'string' && id !== '')
}

async function openEnvironmentFromWorkspacesByUi(page, environmentId) {
  await navigateFromHomeByUi(page, '项目与工作空间')
  await selectProjectByUi(page, INPUT.projectId)

  const row = page.locator('.work-list .work-row').filter({
    has: page.locator('details.advanced-details code').filter({ hasText: environmentId }),
  })
  await expect.poll(async () => await row.count(), { timeout: 120_000, intervals: [1000, 2000, 5000] }).toBe(1)
  const openLink = row.getByRole('link', { name: '打开', exact: true })
  await expect(openLink).toBeVisible({ timeout: 30_000 })
  await openLink.click()
  await expect(page).toHaveURL(new RegExp(`/researcher/environments[?].*environmentId=${encodeURIComponent(environmentId)}`))
  await expect(page.getByRole('heading', { name: '项目环境控制台', exact: true })).toBeVisible({ timeout: 120_000 })
}

async function runTerminalCudaProbeFromWorkspace(page, environmentId) {
  const frames = []
  const sockets = new Map()
  const capture = (socket) => {
    const receive = ({ payload }) => {
      frames.push(typeof payload === 'string' ? payload : Buffer.from(payload).toString('utf8'))
    }
    sockets.set(socket, receive)
    socket.on('framereceived', receive)
  }
  page.on('websocket', capture)
  try {
    await openEnvironmentFromWorkspacesByUi(page, environmentId)
    const environmentIdDetails = page.locator('details.environment-id-details')
    await expect(environmentIdDetails).toBeVisible({ timeout: 120_000 })
    await environmentIdDetails.locator('summary').click()
    await expect(environmentIdDetails.locator('code')).toHaveText(environmentId, { timeout: 30_000 })
    const grantButton = page.getByRole('button', { name: '签发访问授权', exact: true })
    if (await grantButton.count() > 0) {
      await expect(grantButton).toBeEnabled({ timeout: 120_000 })
      await grantButton.click()
    }
    await pollJson(
      page.request,
      `/api/v1/environments/${encodeURIComponent(environmentId)}/access-grants?includeTerminal=false&limit=10`,
      (value) => Array.isArray(value.items) && value.items.some((item) => item.state === 'active'),
      'LW_GPU_CAPACITY_ACCESS_GRANT_ACTIVE_TIMEOUT',
      120_000,
    )
    await page.getByRole('button', { name: 'Web 控制台', exact: true }).click()
    const reconnect = page.getByRole('button', { name: /重新连接终端|重新签发授权并连接终端|立即签发授权并连接终端/ })
    let consoleState = null
    await expect.poll(async () => {
      if (await reconnect.count() > 0
        && await reconnect.first().isVisible()
        && await reconnect.first().isEnabled()) {
        consoleState = 'reconnect'
        return consoleState
      }
      const consolePanel = page.locator('.console-panel')
      if (await consolePanel.count() > 0 && await consolePanel.first().isVisible()) {
        consoleState = 'panel'
        return consoleState
      }
      return null
    }, { timeout: 120_000, intervals: [250, 500, 1000] }).not.toBeNull()
    if (consoleState === 'reconnect') {
      await reconnect.click()
    }
    const consolePanel = page.locator('.console-panel')
    await expect(consolePanel).toBeVisible({ timeout: 120_000 })
    const openTerminal = consolePanel.getByRole('button', { name: '打开终端', exact: true })
    await expect(openTerminal).toBeVisible({ timeout: 120_000 })
    await expect(openTerminal).toBeEnabled({ timeout: 120_000 })
    await openTerminal.click()
    const host = page.locator('.xterm-host')
    await expect(host).toBeVisible({ timeout: 120_000 })
    const input = page.locator('.xterm-helper-textarea')
    await expect(input).toBeAttached({ timeout: 30_000 })
    const output = await typeTerminalCommand(page, input, frames, cudaProbeTerminalCommand(), 'LABWEAVER_CAPACITY_CUDA_DONE')
    return parseCudaProbeResult(output)
  } finally {
    page.off('websocket', capture)
    for (const [socket, receive] of sockets) socket.off('framereceived', receive)
    frames.length = 0
  }
}

async function revokeEnvironmentGrantByUi(page, environmentId, grantId) {
  const response = await page.request.get(`/api/v1/access-grants/${encodeURIComponent(grantId)}`)
  if (response.status() === 404) return { id: grantId, state: 'missing' }
  const current = await expectJson(response, `LW_GPU_CAPACITY_ACCESS_GRANT_READ_FAILED:${grantId}`)
  if (current.state !== 'active') return current
  await openEnvironmentFromWorkspacesByUi(page, environmentId)
  const card = page.locator('.grant-card')
  await expect(card).toBeVisible({ timeout: 120_000 })
  await expect(card).toContainText(grantId, { timeout: 30_000 })
  const revokeButton = page.getByRole('button', { name: '撤销授权', exact: true })
  await expect(revokeButton).toBeEnabled({ timeout: 30_000 })
  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return response.request().method() === 'POST' && url.pathname === `/api/v1/access-grants/${grantId}/revoke`
  })
  await revokeButton.click()
  const accepted = await expectJson(await responsePromise, `LW_GPU_CAPACITY_ACCESS_GRANT_REVOKE_FAILED:${grantId}`)
  expect(accepted).toMatchObject({ id: grantId })
  return await pollJson(
    page.request,
    `/api/v1/access-grants/${encodeURIComponent(grantId)}`,
    (value) => ['revoked', 'denied', 'expired'].includes(value.state),
    `LW_GPU_CAPACITY_ACCESS_GRANT_REVOKE_STATUS_FAILED:${grantId}`,
    120_000,
  )
}

async function cleanupOwnedResource(page, baseURL, owned, lease) {
  await cleanupWorkResources(
    page.request,
    baseURL,
    INPUT.projectId,
    owned.environmentId,
    lease?.leaseId ?? null,
    owned.requestId,
    page,
  )
  if (lease) {
    await waitForReleasedResource(page.request, { requestId: owned.requestId, leaseId: lease.leaseId })
  }
  await waitForDeletedEnvironment(page.request, owned.environmentId)
}

async function readProjectCharges(request) {
  const charges = await expectJson(
    await request.get(`/api/v1/projects/${encodeURIComponent(INPUT.projectId)}/charges`),
    'LW_GPU_CAPACITY_CHARGES_READ_FAILED',
  )
  if (!Array.isArray(charges)) throw new Error('LW_GPU_CAPACITY_CHARGES_INVALID')
  return charges
}

test('student enforces one exclusive GPU capacity and admits the waiting Work request after release', async ({ page, browser, baseURL }) => {
  test.setTimeout(1_800_000)
  if (!baseURL) throw new Error('PW_BASE_URL_REQUIRED')

  const adminContext = await browser.newContext({ baseURL, storageState: AUTH_STATE.admin })
  const adminPage = await adminContext.newPage()
  let requestA = null
  let leaseA = null
  let requestB = null
  let leaseB = null
  const grantIdsB = new Set()
  let primaryError = null
  const cleanupErrors = []
  let cleanedA = false
  let cleanedB = false
  let baselineChargeIds = new Set()
  let gpuRate = null
  let selectedEntry = null

  try {
    await navigateFromHomeByUi(page, '项目与工作空间')
    await navigateFromHomeByUi(adminPage, '资源审批')

    const ownerId = await readActorId(page.request)
    const catalog = await readActiveGpuCatalogEntry(page.request)
    selectedEntry = catalog.entry
    const release = await readPublishedWorkRelease(page.request, selectedEntry)
    const globalState = await readGlobalResourceState(adminPage.request)
    assertAllocationBindingIdle(globalState, catalog.entries, selectedEntry)

    baselineChargeIds = new Set((await readProjectCharges(adminPage.request))
      .map((charge) => charge.id)
      .filter((id) => typeof id === 'string' && id !== ''))
    const now = Date.now()
    const activeGpuRates = (await readResourceRates(adminPage, 'LW_GPU_CAPACITY_RATES_READ_FAILED')).filter((rate) => {
      const effectiveFrom = Date.parse(rate?.effectiveFrom)
      const effectiveUntil = rate?.effectiveUntil == null ? null : Date.parse(rate.effectiveUntil)
      return rate?.unit === 'gpu_unit_second'
        && rate.gpuClass === selectedEntry.class
        && rate.gpuMode === selectedEntry.mode
        && Number.isSafeInteger(rate.unitQuantity)
        && rate.unitQuantity > 0
        && Number.isFinite(effectiveFrom)
        && effectiveFrom <= now
        && (effectiveUntil === null || (Number.isFinite(effectiveUntil) && effectiveUntil > now))
        && rate.unitPrice?.currency
        && Number(rate.unitPrice?.amount) > 0
    })
    if (activeGpuRates.length !== 1) {
      throw new Error(`LW_GPU_CAPACITY_ACTIVE_RATE_AMBIGUOUS:${selectedEntry.class}:${selectedEntry.mode}:${activeGpuRates.length}`)
    }
    [gpuRate] = activeGpuRates

    const acceptedA = await requestGpu(page, selectedEntry, release, (accepted) => { requestA = accepted })
    requestA = acceptedA
    const approvedA = await approveGpuRequest(adminPage, acceptedA, selectedEntry, null, (accepted) => {
      leaseA = { requestId: accepted.requestId, leaseId: accepted.leaseId }
    })
    if (approvedA.blocked || approvedA.leaseState !== '使用中') {
      throw new Error(`LW_GPU_CAPACITY_PRIMARY_APPROVAL_NOT_ACTIVE:${acceptedA.requestId}:${approvedA.diagnosticCode ?? approvedA.leaseState}`)
    }
    if (!leaseA) throw new Error(`LW_GPU_CAPACITY_PRIMARY_LEASE_MISSING:${acceptedA.requestId}`)
    const environmentA = await waitForEnvironmentReady(page.request, acceptedA.environmentId, release)
    await assertHeldResource(page.request, acceptedA, leaseA, environmentA, release, ownerId)
    await openEnvironmentFromWorkspacesByUi(page, acceptedA.environmentId)

    const acceptedB = await requestGpu(page, selectedEntry, release, (accepted) => { requestB = accepted })
    requestB = acceptedB
    const blockedB = await approveGpuRequest(adminPage, acceptedB, selectedEntry, EXPECTED_CAPACITY_FAILURE)
    expect(blockedB).toMatchObject({ blocked: true, diagnosticCode: EXPECTED_CAPACITY_FAILURE, httpStatus: 409 })
    await assertBlockedRequest(page.request, acceptedB, leaseA)

    await cleanupOwnedResource(page, baseURL, acceptedA, leaseA)
    cleanedA = true
    const releasedA = await tryReadEnvironment(page.request, acceptedA.environmentId)
    if (releasedA && releasedA.observedState !== 'deleted') {
      throw new Error(`LW_GPU_CAPACITY_PRIMARY_ENVIRONMENT_NOT_DELETED:${acceptedA.environmentId}:${releasedA.observedState}`)
    }

    const retryState = await readProjectRequest(page.request, acceptedB.requestId)
    if (retryState.state !== 'reviewing') {
      throw new Error(`LW_GPU_CAPACITY_RETRY_NOT_ALLOWED:${acceptedB.requestId}:${retryState.state}:use_the_public_resource_request_page_to_recover_before_retry`)
    }
    const approvedB = await approveGpuRequest(adminPage, acceptedB, selectedEntry, null, (accepted) => {
      leaseB = { requestId: accepted.requestId, leaseId: accepted.leaseId }
    })
    if (approvedB.blocked || approvedB.leaseState !== '使用中') {
      throw new Error(`LW_GPU_CAPACITY_RELEASE_DID_NOT_ADMIT:${acceptedB.requestId}:${approvedB.diagnosticCode ?? approvedB.leaseState}`)
    }
    if (!leaseB) throw new Error(`LW_GPU_CAPACITY_CONTENDER_LEASE_MISSING:${acceptedB.requestId}`)
    const environmentB = await waitForEnvironmentReady(page.request, acceptedB.environmentId, release)
    await assertHeldResource(page.request, acceptedB, leaseB, environmentB, release, ownerId)

    const terminalPage = await page.context().newPage()
    try {
      const beforeGrantIds = await readActiveGrantIds(terminalPage.request, acceptedB.environmentId)
      if (beforeGrantIds.length !== 0) throw new Error(`LW_GPU_CAPACITY_ACCESS_GRANT_PREEXISTING:${acceptedB.environmentId}`)
      expect(await runTerminalCudaProbeFromWorkspace(terminalPage, acceptedB.environmentId)).toEqual({ count: 256, sum: 32640, max: 255 })
      for (const grantId of await readActiveGrantIds(terminalPage.request, acceptedB.environmentId)) grantIdsB.add(grantId)
      if (grantIdsB.size !== 1) throw new Error(`LW_GPU_CAPACITY_ACCESS_GRANT_COUNT_INVALID:${grantIdsB.size}`)
    } finally {
      await terminalPage.close()
    }

    for (const grantId of grantIdsB) await revokeEnvironmentGrantByUi(page, acceptedB.environmentId, grantId)
    await cleanupOwnedResource(page, baseURL, acceptedB, leaseB)
    cleanedB = true

    const finance = await waitForSettledWorkUsageCharges(browser, baseURL, {
      projectId: INPUT.projectId,
      leases: [leaseA, leaseB],
      baselineChargeIds,
      gpu: { class: selectedEntry.class, mode: selectedEntry.mode, rate: gpuRate },
    })
    const expectedLeaseKeys = new Set([`${leaseA.requestId}:${leaseA.leaseId}`, `${leaseB.requestId}:${leaseB.leaseId}`])
    const matchedLeaseKeys = new Set(finance.matches.map(({ usage }) => `${usage.target.requestId}:${usage.target.leaseId}`))
    expect(matchedLeaseKeys).toEqual(expectedLeaseKeys)
    await inspectRealWorkFinanceByUi(browser, baseURL, INPUT.projectId, {
      gpu: { class: selectedEntry.class, mode: selectedEntry.mode, rate: gpuRate },
      usageRecordIds: finance.matches.map(({ usage }) => usage.id),
      expectedCharges: finance.matches.map(({ charge }) => charge),
    })
  } catch (error) {
    primaryError = error
  }

  const cleanupGrantIds = new Set(grantIdsB)
  if (requestB?.environmentId) {
    try {
      for (const grantId of await readActiveGrantIds(page.request, requestB.environmentId)) cleanupGrantIds.add(grantId)
    } catch (error) {
      cleanupErrors.push(error)
    }
  }
  for (const grantId of cleanupGrantIds) {
    try {
      await revokeEnvironmentGrantByUi(page, requestB.environmentId, grantId)
    } catch (error) {
      cleanupErrors.push(error)
    }
  }
  if (requestA && !cleanedA) {
    try {
      await cleanupOwnedResource(page, baseURL, requestA, leaseA)
      cleanedA = true
    } catch (error) {
      cleanupErrors.push(error)
    }
  }
  if (requestB && !cleanedB) {
    try {
      await cleanupOwnedResource(page, baseURL, requestB, leaseB)
      cleanedB = true
    } catch (error) {
      cleanupErrors.push(error)
    }
  }
  try {
    await adminContext.close()
  } catch (error) {
    cleanupErrors.push(error)
  }

  if (primaryError && cleanupErrors.length) {
    throw new AggregateError([primaryError, ...cleanupErrors], `${primaryError instanceof Error ? primaryError.message : String(primaryError)}; cleanup failed`)
  }
  if (primaryError) throw primaryError
  if (cleanupErrors.length) throw new AggregateError(cleanupErrors, 'LW_GPU_CAPACITY_CLEANUP_FAILED')
})
