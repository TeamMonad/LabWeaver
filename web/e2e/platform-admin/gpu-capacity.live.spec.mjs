import { expect, test } from '@playwright/test'
import { AUTH_STATE, expectJson } from '../support/live.mjs'
import {
  approveResourceRequestByUi,
  cancelProjectResourceRequestByUi,
  releaseProjectLeaseByUi,
  requestProjectResourceByUi,
} from '../support/real-resource.mjs'

const CLASS_ENV = 'LABWEAVER_E2E_GPU_CAPACITY_CLASS'
const PROJECT_ENV = 'LABWEAVER_E2E_GPU_CAPACITY_PROJECT_ID'
const INPUT = Object.freeze({
  class: process.env[CLASS_ENV]?.trim() || '',
  projectId: process.env[PROJECT_ENV]?.trim() || '',
})
const PROVIDED_INPUTS = Object.values(INPUT).filter(Boolean).length
const GPU_MODES = Object.freeze(['exclusive', 'container_time_slice', 'vm_vgpu'])
const ACTIVE_REQUEST_STATES = new Set(['allocating', 'active', 'expiring'])
const ACTIVE_LEASE_STATES = new Set(['allocating', 'active', 'expiring'])
const EXPECTED_CAPACITY_FAILURE = 'LW_RESOURCE_GPU_CAPACITY_EXHAUSTED'
const MAX_APPROVED_REQUESTS = 16
const GIB = 1024 ** 3

if (PROVIDED_INPUTS !== 0 && PROVIDED_INPUTS !== Object.keys(INPUT).length) {
  throw new Error('LW_GPU_CAPACITY_INPUT_INCOMPLETE')
}
if (INPUT.projectId && !/^[0-9a-f-]{36}$/i.test(INPUT.projectId)) {
  throw new Error('LW_GPU_CAPACITY_PROJECT_ID_INVALID')
}
if (INPUT.class && !/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(INPUT.class)) {
  throw new Error('LW_GPU_CAPACITY_CLASS_INVALID')
}

test.skip(PROVIDED_INPUTS === 0, `set ${CLASS_ENV} and ${PROJECT_ENV} to run the owned GPU capacity scenario`)

async function readActiveGpuCatalogEntry(request, className) {
  const response = await request.get('/api/v1/resource/gpu-catalog')
  const entries = await expectJson(response, 'LW_GPU_CAPACITY_CATALOG_READ_FAILED')
  if (!Array.isArray(entries)) throw new Error('LW_GPU_CAPACITY_CATALOG_INVALID')
  const matching = entries.filter((entry) => entry.class === className && entry.active === true)
  if (matching.length !== 1) throw new Error(`LW_GPU_CAPACITY_CATALOG_ENTRY_AMBIGUOUS:${className}:${matching.length}`)
  const entry = matching[0]
  if (
    !GPU_MODES.includes(entry.mode)
    || !Number.isInteger(entry.capacityUnits)
    || entry.capacityUnits < 1
    || typeof entry.providerBinding !== 'string'
    || entry.providerBinding.trim() === ''
    || typeof entry.allocationBinding !== 'string'
    || entry.allocationBinding.trim() === ''
  ) {
    throw new Error(`LW_GPU_CAPACITY_CATALOG_ENTRY_INVALID:${className}`)
  }
  return { entry, entries }
}

async function readCompatibleWorkRelease(request, entry) {
  const expectedRuntimeKind = entry.mode === 'vm_vgpu' ? 'virtual_machine' : 'container'
  const response = await request.get(`/api/v1/projects/${INPUT.projectId}/environment-template-releases?limit=100`)
  const page = await expectJson(response, 'LW_GPU_CAPACITY_RELEASES_READ_FAILED')
  if (!Array.isArray(page?.items)) throw new Error('LW_GPU_CAPACITY_RELEASES_INVALID')
  const release = page.items.find((candidate) => {
    const artifactKind = expectedRuntimeKind === 'virtual_machine' ? 'virtual_machine' : 'container'
    return candidate.runtimeKind === expectedRuntimeKind
      && candidate.artifact?.kind === artifactKind
      && !candidate.withdrawal
  })
  if (!release || typeof release.id !== 'string' || !Number.isInteger(release.version) || release.version < 1) {
    throw new Error(`LW_GPU_CAPACITY_COMPATIBLE_RELEASE_MISSING:${expectedRuntimeKind}`)
  }
  let storageGiB = 1
  if (expectedRuntimeKind === 'virtual_machine') {
    const capacityBytes = release.artifact?.base_disk?.capacityBytes
    if (!Number.isSafeInteger(capacityBytes) || capacityBytes < 1) {
      throw new Error('LW_GPU_CAPACITY_VM_BASE_DISK_CAPACITY_INVALID')
    }
    storageGiB = Math.max(16, Math.ceil(capacityBytes / GIB))
  }
  return {
    releaseId: release.id,
    releaseVersion: release.version,
    runtimeKind: expectedRuntimeKind,
    workload: Object.freeze({
      cpuMillicores: 1000,
      memoryGiB: expectedRuntimeKind === 'virtual_machine' ? 2 : 1,
      storageGiB,
      durationHours: 1,
    }),
  }
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
  if (!Array.isArray(requests) || !Array.isArray(leases)) {
    throw new Error('LW_GPU_CAPACITY_GLOBAL_STATE_INVALID')
  }
  return { requests, leases }
}

function assertAllocationBindingIdle({ requests, leases }, catalogEntries, selectedEntry) {
  const entryByClass = new Map()
  for (const entry of catalogEntries.filter((candidate) => candidate.active === true)) {
    if (entryByClass.has(entry.class)) {
      throw new Error(`LW_GPU_CAPACITY_ACTIVE_CATALOG_CLASS_AMBIGUOUS:${entry.class}`)
    }
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
    if (entry.allocationBinding !== selectedEntry.allocationBinding) return false
    return true
  })
  const orphanActiveLeases = []
  for (const lease of leases.filter((item) => ACTIVE_LEASE_STATES.has(item.state))) {
    const request = requestById.get(lease.requestId)
    if (!request) throw new Error(`LW_GPU_CAPACITY_LEASE_REQUEST_MISSING:${lease.id}:${lease.requestId}`)
    const gpuClass = request?.requestedResources?.gpu?.class
    if (!gpuClass) continue
    const entry = entryByClass.get(gpuClass)
    if (!entry) throw new Error(`LW_GPU_CAPACITY_CATALOG_CLASS_UNKNOWN:${gpuClass}`)
    if (entry.allocationBinding === selectedEntry.allocationBinding) orphanActiveLeases.push(lease)
  }
  // The public Lease projection intentionally omits its CapacityClaim/GPU allocation identity;
  // direct Environment reservations also have no public list route. Live runs require the
  // coordinated read-only Resource preflight before this UI flow starts.
  if (heldOrPending.length || orphanActiveLeases.length) {
    const requestsDetail = heldOrPending.map((request) => `${request.id}:${request.state}:${request.projectId}`).join(',') || 'none'
    const leasesDetail = orphanActiveLeases.map((lease) => `${lease.id}:${lease.state}:${lease.requestId}`).join(',') || 'none'
    throw new Error(`LW_GPU_CAPACITY_ALLOCATION_BINDING_BUSY:${selectedEntry.allocationBinding}:requests=${requestsDetail}:leases=${leasesDetail}`)
  }
}

async function readProjectResources(request, projectId) {
  const [requestsResponse, leasesResponse] = await Promise.all([
    request.get(`/api/v1/projects/${projectId}/resource-requests`),
    request.get(`/api/v1/projects/${projectId}/resource-leases`),
  ])
  const [requests, leases] = await Promise.all([
    expectJson(requestsResponse, 'LW_GPU_CAPACITY_PROJECT_REQUEST_READ_FAILED'),
    expectJson(leasesResponse, 'LW_GPU_CAPACITY_PROJECT_LEASE_READ_FAILED'),
  ])
  if (!Array.isArray(requests) || !Array.isArray(leases)) {
    throw new Error('LW_GPU_CAPACITY_PROJECT_STATE_INVALID')
  }
  return { requests, leases }
}

async function tryReadEnvironment(request, environmentId) {
  const response = await request.get(`/api/v1/environments/${environmentId}`)
  if (response.status() === 404) return null
  if (!response.ok()) {
    throw new Error(`LW_GPU_CAPACITY_ENVIRONMENT_READ_FAILED:${environmentId}:${response.status()}`)
  }
  const environment = await response.json()
  if (!environment || environment.id !== environmentId) {
    throw new Error(`LW_GPU_CAPACITY_ENVIRONMENT_IDENTITY_MISMATCH:${environmentId}`)
  }
  return environment
}

async function readEnvironment(request, environmentId) {
  const environment = await tryReadEnvironment(request, environmentId)
  if (!environment) throw new Error(`LW_GPU_CAPACITY_ENVIRONMENT_NOT_FOUND:${environmentId}`)
  return environment
}

async function waitForEnvironmentCreated(request, environmentId, runtimeKind) {
  let latestState = 'missing'
  let latestRuntime = 'missing'
  try {
    await expect.poll(async () => {
      const environment = await tryReadEnvironment(request, environmentId)
      latestState = environment?.observedState ?? 'missing'
      latestRuntime = environment?.runtimeKind ?? 'missing'
      return Boolean(environment)
    }, { timeout: 240_000, intervals: [1000, 2000, 5000] }).toBe(true)
  } catch (error) {
    throw new Error(`LW_GPU_CAPACITY_ENVIRONMENT_NOT_CREATED:${environmentId}:${latestState}:${latestRuntime}`, { cause: error })
  }
  const environment = await readEnvironment(request, environmentId)
  if (environment.runtimeKind !== runtimeKind) {
    throw new Error(`LW_GPU_CAPACITY_ENVIRONMENT_RUNTIME_MISMATCH:${environmentId}:${environment.runtimeKind}:${runtimeKind}`)
  }
  if (environment.observedState === 'failed' || environment.observedState === 'deleted') {
    throw new Error(`LW_GPU_CAPACITY_ENVIRONMENT_NOT_USABLE:${environmentId}:${environment.observedState}`)
  }
  return environment
}

async function waitForEnvironmentsReady(request, ownedRequests, runtimeKind) {
  const expected = ownedRequests.map(() => ({ state: 'ready', runtimeKind }))
  await expect.poll(async () => {
    const environments = await Promise.all(ownedRequests.map((owned) => (
      tryReadEnvironment(request, owned.environmentId)
    )))
    return environments.map((environment) => ({
      state: environment?.observedState ?? 'missing',
      runtimeKind: environment?.runtimeKind ?? 'missing',
    }))
  }, { timeout: 240_000, intervals: [1000, 2000, 5000] }).toEqual(expected)
}

async function assertHeldCapacity(request, ownedRequests, ownedLeases, runtimeKind, expectedCount) {
  expect(ownedRequests).toHaveLength(expectedCount)
  expect(new Set(ownedRequests.map((owned) => owned.environmentId)).size).toBe(expectedCount)
  const [resources, environments] = await Promise.all([
    readProjectResources(request, INPUT.projectId),
    Promise.all(ownedRequests.map((owned) => readEnvironment(request, owned.environmentId))),
  ])
  for (const [index, owned] of ownedRequests.entries()) {
    const currentRequest = resources.requests.find((item) => item.id === owned.requestId)
    const ownedLease = ownedLeases.find((item) => item.requestId === owned.requestId)
    const currentLease = resources.leases.find((item) => item.id === ownedLease?.leaseId)
    expect(currentRequest, `LW_GPU_CAPACITY_REQUEST_NOT_ACTIVE:${owned.requestId}`).toMatchObject({
      state: 'active',
      requestedResources: { gpu: { count: 1 } },
    })
    expect(currentLease, `LW_GPU_CAPACITY_LEASE_NOT_ACTIVE:${owned.requestId}`).toMatchObject({
      requestId: owned.requestId,
      state: 'active',
    })
    expect(environments[index], `LW_GPU_CAPACITY_ENVIRONMENT_NOT_READY:${owned.environmentId}`).toMatchObject({
      observedState: 'ready',
      runtimeKind,
    })
  }
}

async function waitForReleasedResource(request, ownedRequest, ownedLease) {
  await expect.poll(async () => {
    const resources = await readProjectResources(request, INPUT.projectId)
    const currentRequest = resources.requests.find((item) => item.id === ownedRequest.requestId)
    const currentLease = resources.leases.find((item) => item.id === ownedLease.leaseId)
    return currentRequest?.state === 'expired'
      && currentLease?.requestId === ownedRequest.requestId
      && ['revoked', 'expired'].includes(currentLease?.state)
  }, { timeout: 240_000, intervals: [1000, 2000, 5000] }).toBe(true)
}

async function waitForEnvironmentDeleted(request, environmentId) {
  let latestState = 'unread'
  try {
    await expect.poll(async () => {
      const environment = await tryReadEnvironment(request, environmentId)
      latestState = environment?.observedState ?? 'missing'
      if (!environment) return latestState
      return latestState
    }, { timeout: 240_000, intervals: [1000, 2000, 5000] }).toBe('deleted')
  } catch (error) {
    throw new Error(`LW_GPU_CAPACITY_ENVIRONMENT_NOT_DELETED:${environmentId}:${latestState}`, { cause: error })
  }
}

async function requestGpu(page, entry, count, release, onAccepted) {
  return await requestProjectResourceByUi(page, {
    projectId: INPUT.projectId,
    kind: 'gpu',
    gpuClass: entry.class,
    gpuMode: entry.mode,
    gpuCount: count,
    releaseId: release.releaseId,
    releaseVersion: release.releaseVersion,
    ...release.workload,
    onAccepted,
  })
}

async function approveGpuRequest(adminPage, request, entry, expectedFailureCode = null, onAccepted = null) {
  return await approveResourceRequestByUi(adminPage, {
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

async function cleanupOwnedResources(studentPage, projectId, requests, knownLeases) {
  const failures = []
  const releasedEnvironments = new Set()
  for (const lease of knownLeases) {
    const request = requests.find((item) => item.requestId === lease.requestId)
    if (!request) continue
    try {
      await releaseProjectLeaseByUi(studentPage, { projectId, leaseId: lease.leaseId })
      releasedEnvironments.add(request.environmentId)
    } catch (error) {
      failures.push(error)
    }
  }
  let resources = null
  try {
    resources = await readProjectResources(studentPage.request, projectId)
  } catch (error) {
    failures.push(error)
  }
  if (resources) {
    for (const request of requests) {
      const lease = resources.leases.find((item) => item.requestId === request.requestId)
      if (lease && !['revoked', 'expired'].includes(lease.state)) {
        try {
          await releaseProjectLeaseByUi(studentPage, { projectId, leaseId: lease.id })
          releasedEnvironments.add(request.environmentId)
        } catch (error) {
          failures.push(error)
        }
      }
      if (lease && ['revoked', 'expired'].includes(lease.state)) releasedEnvironments.add(request.environmentId)
    }
    const refreshed = await readProjectResources(studentPage.request, projectId).catch((error) => {
      failures.push(error)
      return null
    })
    if (refreshed) {
      for (const request of requests) {
        const current = refreshed.requests.find((item) => item.id === request.requestId)
        if (current && ['reviewing', 'allocating'].includes(current.state)) {
          try {
            await cancelProjectResourceRequestByUi(studentPage, { projectId, requestKey: request.requestKey })
          } catch (error) {
            failures.push(error)
          }
        }
      }
    }
  }
  for (const environmentId of releasedEnvironments) {
    try {
      await waitForEnvironmentDeleted(studentPage.request, environmentId)
    } catch (error) {
      failures.push(error)
    }
  }
  if (failures.length) {
    throw new AggregateError(failures, `LW_GPU_CAPACITY_CLEANUP_FAILED:${failures.length}`)
  }
}

test('platform administrator enforces GPU capacity and admits the waiting request after release', async ({ page, browser, baseURL }) => {
  test.setTimeout(1_800_000)
  if (!baseURL) throw new Error('PW_BASE_URL_REQUIRED')

  const studentContext = await browser.newContext({ baseURL, storageState: AUTH_STATE.student })
  const studentPage = await studentContext.newPage()
  const ownedRequests = []
  const ownedLeases = []
  let primaryError = null
  let cleanupError = null
  try {
    const { entry, entries } = await readActiveGpuCatalogEntry(studentPage.request, INPUT.class)
    const release = await readCompatibleWorkRelease(studentPage.request, entry)
    const globalState = await readGlobalResourceState(page.request)
    assertAllocationBindingIdle(globalState, entries, entry)

    if (entry.capacityUnits > MAX_APPROVED_REQUESTS) {
      throw new Error(`LW_GPU_CAPACITY_SCENARIO_REQUEST_LIMIT_EXCEEDED:${entry.class}:${entry.capacityUnits}:${MAX_APPROVED_REQUESTS}`)
    }
    const baselineAllocations = Array.from({ length: entry.capacityUnits }, () => 1)
    for (const count of baselineAllocations) {
      const request = await requestGpu(studentPage, entry, count, release, (accepted) => ownedRequests.push(accepted))
      const lease = await approveGpuRequest(page, request, entry, null, (accepted) => {
        ownedLeases.push({ requestId: accepted.requestId, leaseId: accepted.leaseId })
      })
      if (lease.blocked || lease.leaseState !== '使用中') {
        throw new Error(`LW_GPU_CAPACITY_BASELINE_NOT_ACTIVE:${request.requestId}:${lease.diagnosticCode ?? lease.leaseState}`)
      }
      await waitForEnvironmentCreated(studentPage.request, request.environmentId, release.runtimeKind)
    }

    const baselineRequests = [...ownedRequests]
    await waitForEnvironmentsReady(studentPage.request, baselineRequests, release.runtimeKind)
    await assertHeldCapacity(studentPage.request, baselineRequests, ownedLeases, release.runtimeKind, entry.capacityUnits)

    const contender = await requestGpu(studentPage, entry, 1, release, (accepted) => ownedRequests.push(accepted))
    const blocked = await approveGpuRequest(page, contender, entry, EXPECTED_CAPACITY_FAILURE)
    expect(blocked).toMatchObject({ blocked: true, diagnosticCode: EXPECTED_CAPACITY_FAILURE })
    const blockedState = await readProjectResources(studentPage.request, INPUT.projectId)
    const blockedRequest = blockedState.requests.find((item) => item.id === contender.requestId)
    expect(blockedRequest?.state).toBe('reviewing')
    expect(blockedState.leases.some((lease) => lease.requestId === contender.requestId)).toBe(false)

    await assertHeldCapacity(studentPage.request, baselineRequests, ownedLeases, release.runtimeKind, entry.capacityUnits)
    const releasedRequest = baselineRequests[0]
    const releasedLease = ownedLeases.find((lease) => lease.requestId === releasedRequest.requestId)
    if (!releasedLease) throw new Error(`LW_GPU_CAPACITY_RELEASE_LEASE_MISSING:${releasedRequest.requestId}`)
    await releaseProjectLeaseByUi(studentPage, { projectId: INPUT.projectId, leaseId: releasedLease.leaseId })
    await Promise.all([
      waitForEnvironmentDeleted(studentPage.request, releasedRequest.environmentId),
      waitForReleasedResource(studentPage.request, releasedRequest, releasedLease),
    ])
    ownedLeases.splice(ownedLeases.indexOf(releasedLease), 1)
    const remainingRequests = baselineRequests.slice(1)
    await assertHeldCapacity(studentPage.request, remainingRequests, ownedLeases, release.runtimeKind, entry.capacityUnits - 1)

    const acquired = await approveGpuRequest(page, contender, entry, null, (accepted) => {
      ownedLeases.push({ requestId: accepted.requestId, leaseId: accepted.leaseId })
    })
    if (acquired.blocked || acquired.leaseState !== '使用中') {
      throw new Error(`LW_GPU_CAPACITY_RELEASE_DID_NOT_ADMIT:${contender.requestId}:${acquired.diagnosticCode ?? acquired.leaseState}`)
    }
    await waitForEnvironmentCreated(studentPage.request, contender.environmentId, release.runtimeKind)
    const fullCapacityRequests = [...remainingRequests, contender]
    await waitForEnvironmentsReady(studentPage.request, fullCapacityRequests, release.runtimeKind)
    await assertHeldCapacity(studentPage.request, fullCapacityRequests, ownedLeases, release.runtimeKind, entry.capacityUnits)
  } catch (error) {
    primaryError = error
  }
  try {
    await cleanupOwnedResources(studentPage, INPUT.projectId, ownedRequests, ownedLeases)
  } catch (error) {
    cleanupError = error
  }
  try {
    await studentContext.close()
  } catch (error) {
    cleanupError = cleanupError
      ? new AggregateError([cleanupError, error], 'LW_GPU_CAPACITY_CONTEXT_CLEANUP_FAILED')
      : error
  }
  if (primaryError && cleanupError) {
    throw new AggregateError([primaryError, cleanupError], `${primaryError instanceof Error ? primaryError.message : String(primaryError)}; cleanup failed: ${cleanupError instanceof Error ? cleanupError.message : String(cleanupError)}`, { cause: primaryError })
  }
  if (primaryError) throw primaryError
  if (cleanupError) throw cleanupError
})
