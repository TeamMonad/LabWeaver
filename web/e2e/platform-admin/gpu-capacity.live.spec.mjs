import { expect, test } from '@playwright/test'
import { AUTH_STATE, expectJson } from '../support/live.mjs'
import {
  approveResourceRequestByUi,
  requestProjectResourceByUi,
} from '../support/real-resource.mjs'
import { cleanupWorkResources } from '../support/real-work.mjs'
import { createRealWorkSshIdentity, readRealWorkVmLicenseStatus, runRealWorkVmCudaProbe } from '../support/real-work-ssh.mjs'
import { addSshPublicKeyByUi, deleteSshPublicKeyByUi, issueEnvironmentSshAccessGrantByUi } from '../support/ssh-access.mjs'
import { runTerminalCudaProbe } from '../support/real-gpu.mjs'
import { readActorId } from '../support/real-experiment.mjs'

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
  let release = null
  for (const candidateRelease of page.items) {
    if (candidateRelease.runtimeKind !== expectedRuntimeKind
      || candidateRelease.artifact?.kind !== expectedRuntimeKind
      || candidateRelease.withdrawal) continue
    const view = await expectJson(
      await request.get(`/api/v1/projects/${INPUT.projectId}/environment-candidates/${candidateRelease.candidateId}`),
      'LW_GPU_CAPACITY_RELEASE_CANDIDATE_READ_FAILED',
    )
    const candidate = view.candidate
    if (candidate?.id !== candidateRelease.candidateId
      || candidate.projectId !== INPUT.projectId
      || candidate.revision !== candidateRelease.candidateRevision
      || candidateRelease.projectId !== INPUT.projectId
      || candidateRelease.approval?.decision !== 'approved'
      || candidateRelease.approval.candidateId !== candidate.id
      || candidateRelease.approval.candidateRevision !== candidate.revision) {
      throw new Error('LW_GPU_CAPACITY_RELEASE_IDENTITY_INVALID')
    }
    expect(view.imageArtifact, 'LW_GPU_CAPACITY_RELEASE_ARTIFACT_MISMATCH').toEqual(candidateRelease.artifact)
    const runtime = candidate.spec?.runtime
    if (runtime?.kind !== expectedRuntimeKind || runtime.provider_binding !== entry.providerBinding
      || candidate.spec.resources?.gpu?.class !== entry.class
      || candidate.spec.resources.gpu.count !== 1) continue
    if (expectedRuntimeKind === 'container' && !runtime.terminal) continue
    release = candidateRelease
    break
  }
  if (!release || typeof release.id !== 'string' || !Number.isInteger(release.version) || release.version < 1) {
    throw new Error(`LW_GPU_CAPACITY_GPU_ACCESS_RELEASE_MISSING:${expectedRuntimeKind}:${entry.class}`)
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
      leaseId: ownedLease.leaseId,
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

async function verifyOwnedGpuEnvironment(studentPage, entry, owned, release, ownerId, identity) {
  const environment = await readEnvironment(studentPage.request, owned.environmentId)
  expect(environment).toMatchObject({
    id: owned.environmentId,
    projectId: INPUT.projectId,
    observedState: 'ready',
    runtimeKind: release.runtimeKind,
    ownerId,
    releaseId: release.releaseId,
    releaseVersion: release.releaseVersion,
    gpuAllocation: { class: entry.class, mode: entry.mode, count: 1, providerBinding: entry.providerBinding },
  })
  if (release.runtimeKind === 'virtual_machine') {
    const connection = await issueEnvironmentSshAccessGrantByUi(studentPage, INPUT.projectId, environment)
    await expect.poll(async () => {
      let license
      try {
        license = await readRealWorkVmLicenseStatus(connection.endpointGrant, identity)
      } catch (error) {
        if (error instanceof Error && error.message === 'WORK_VM_VGPU_LICENSE_NOT_GRANTED:unlicensed') return false
        throw error
      }
      expect(license).toMatchObject({ licenseStatus: 'Licensed', licensedGpuCount: 1 })
      return true
    }, { timeout: 180_000, intervals: [2000, 5000] }).toBe(true)
    expect(await runRealWorkVmCudaProbe(connection.endpointGrant, identity)).toEqual({ count: 256, sum: 32640, max: 255 })
  } else {
    const terminalPage = await studentPage.context().newPage()
    try {
      expect(await runTerminalCudaProbe(terminalPage, INPUT.projectId, environment.id)).toEqual({ count: 256, sum: 32640, max: 255 })
    } finally {
      await terminalPage.close()
    }
  }
}

async function cleanupOwnedResources(studentPage, baseURL, requests, knownLeases) {
  const failures = []
  for (const owned of requests) {
    const lease = knownLeases.find((item) => item.requestId === owned.requestId)
    try {
      await cleanupWorkResources(studentPage.request, baseURL, INPUT.projectId, owned.environmentId, lease?.leaseId ?? null, owned.requestId)
    } catch (error) {
      failures.push(error)
    }
  }
  if (failures.length) throw new AggregateError(failures, `LW_GPU_CAPACITY_CLEANUP_FAILED:${failures.length}`)
}

async function readProjectCharges(request) {
  const charges = await expectJson(await request.get(`/api/v1/projects/${INPUT.projectId}/charges`), 'LW_GPU_CAPACITY_CHARGES_READ_FAILED')
  if (!Array.isArray(charges)) throw new Error('LW_GPU_CAPACITY_CHARGES_INVALID')
  return charges
}

async function assertPositiveGpuCharge(request, priorChargeIds) {
  // Public charges expose usageRecordId but no Lease identity. This assertion
  // verifies a new positive project charge, not its attribution to a Lease.
  await expect.poll(async () => {
    const charges = await readProjectCharges(request)
    return charges.some((charge) => !priorChargeIds.has(charge.id) && charge.projectId === INPUT.projectId
      && charge.settlement === 'settled' && Number(charge.total?.amount) > 0
      && charge.lines?.some((line) => line.unit === 'gpu_unit_second'
        && line.quantity > 0 && Number(line.amount?.amount) > 0))
  }, { timeout: 300_000, intervals: [1000, 2000, 5000] }).toBe(true)
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
  let sshIdentity = null
  let sshKey = null
  let priorChargeIds = new Set()
  try {
    const ownerId = await readActorId(studentPage.request)
    const { entry, entries } = await readActiveGpuCatalogEntry(studentPage.request, INPUT.class)
    const release = await readCompatibleWorkRelease(studentPage.request, entry)
    if (release.runtimeKind === 'virtual_machine') {
      sshIdentity = await createRealWorkSshIdentity()
      await addSshPublicKeyByUi(studentPage, sshIdentity, (accepted) => { sshKey = accepted })
    }
    const globalState = await readGlobalResourceState(page.request)
    assertAllocationBindingIdle(globalState, entries, entry)
    priorChargeIds = new Set((await readProjectCharges(page.request)).map((charge) => charge.id))

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
    for (const owned of baselineRequests) {
      await verifyOwnedGpuEnvironment(studentPage, entry, owned, release, ownerId, sshIdentity)
    }

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
    await cleanupWorkResources(studentPage.request, baseURL, INPUT.projectId, releasedRequest.environmentId, releasedLease.leaseId, releasedRequest.requestId)
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
    await verifyOwnedGpuEnvironment(studentPage, entry, contender, release, ownerId, sshIdentity)
  } catch (error) {
    primaryError = error
  }
  try {
    await cleanupOwnedResources(studentPage, baseURL, ownedRequests, ownedLeases)
  } catch (error) {
    cleanupError = error
  }
  if (!primaryError && !cleanupError) {
    try {
      await assertPositiveGpuCharge(page.request, priorChargeIds)
    } catch (error) {
      primaryError = error
    }
  }
  for (const cleanup of [
    async () => { if (sshKey) await deleteSshPublicKeyByUi(studentPage, sshKey) },
    async () => { await sshIdentity?.cleanup() },
  ]) {
    try {
      await cleanup()
    } catch (error) {
      cleanupError = cleanupError ? new AggregateError([cleanupError, error], 'LW_GPU_CAPACITY_SSH_CLEANUP_FAILED') : error
    }
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
