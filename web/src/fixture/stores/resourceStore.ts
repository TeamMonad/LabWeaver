import type {
  ApproveResourceRequestSchema,
  CreateResourceRequestSchema,
  ResourceLeaseSchema,
  ResourceOperationAcceptedSchema,
  ResourceRequestMutationSchema,
  ResourceRequestSchema,
  ResourceRequestState,
  RenewResourceLeaseSchema,
  WorkloadResources,
} from '@/generated/contracts'
import { addSecondsIso, nowIso } from '../utils/clock'
import { nextUuid7 } from '../utils/identity'
import type { FixtureActor } from './actorStore'

const GIB = 1024 ** 3
const FIXTURE_RELEASE_SHA256 = 'c'.repeat(64)

const requests = new Map<string, ResourceRequestSchema>()
const leases = new Map<string, ResourceLeaseSchema>()

/**
 * Idempotent mutation replay records. A replayed Idempotency-Key with an
 * identical fingerprint returns the first accepted response without advancing
 * the state machine; a different fingerprint under the same key is a conflict.
 */
interface IdempotentRecord {
  fingerprint: string
  response: ResourceOperationAcceptedSchema | ResourceLeaseSchema
}

const idempotencyMap = new Map<string, IdempotentRecord>()

export type ResourceMutationResult<T> =
  | { kind: 'ok'; value: T; replayed: boolean }
  | { kind: 'conflict' }
  | { kind: 'not-found' }
  | { kind: 'invalid-state' }

function buildResources(overrides: Partial<WorkloadResources> = {}): WorkloadResources {
  return {
    cpuMillicores: 2000,
    memoryBytes: 4 * GIB,
    storageBytes: 20 * GIB,
    ...overrides,
  }
}

interface RequestSeedOptions {
  requestKey: string
  courseId: string
  requesterId: string
  state: ResourceRequestState
  revision: number
  resources: WorkloadResources
  durationSeconds: number
  diagnosticCode?: string | null
}

function seedRequest(options: RequestSeedOptions): ResourceRequestSchema {
  const request: ResourceRequestSchema = {
    id: nextUuid7('rreq'),
    generation: 1,
    requestKey: options.requestKey,
    requesterId: options.requesterId,
    courseId: options.courseId,
    projectId: `proj-${options.courseId}`,
    target: {
      environmentId: nextUuid7('env'),
      releaseId: nextUuid7('release'),
      releaseVersion: 3,
      releaseSha256: FIXTURE_RELEASE_SHA256,
    },
    requestedResources: options.resources,
    requestedDurationSeconds: options.durationSeconds,
    state: options.state,
    revision: options.revision,
    createdAt: nowIso(),
    updatedAt: nowIso(),
    diagnosticCode: options.diagnosticCode ?? null,
  }
  requests.set(request.id, request)
  return request
}

function seedLease(
  request: ResourceRequestSchema,
  state: ResourceLeaseSchema['state'],
  revision: number,
  windowSeconds?: number,
): ResourceLeaseSchema {
  const activeWindow = windowSeconds === undefined ? null : {
    activeFrom: nowIso(),
    expiresAt: addSecondsIso(windowSeconds),
  }
  const lease: ResourceLeaseSchema = {
    id: nextUuid7('lease'),
    requestId: request.id,
    claimId: nextUuid7('claim'),
    state,
    revision,
    activeFrom: activeWindow?.activeFrom ?? null,
    expiresAt: activeWindow?.expiresAt ?? null,
    revokeReasonCode: null,
    createdAt: nowIso(),
    updatedAt: nowIso(),
  }
  leases.set(lease.id, lease)
  return lease
}

/**
 * Seeds deterministic resource requests and leases covering the states the
 * admin approval surface drives: two reviewing requests (approve/reject
 * flows), one allocating request with its allocating Lease, and one active
 * request with an active Lease (renew/revoke flows).
 */
export function seedResources(): void {
  seedRequest({
    requestKey: 'cpu-lab-request',
    courseId: 'course-101',
    requesterId: 'fixture-actor-student',
    state: 'reviewing',
    revision: 1,
    resources: buildResources(),
    durationSeconds: 7200,
  })

  seedRequest({
    requestKey: 'gpu-training-request',
    courseId: 'course-102',
    requesterId: 'fixture-actor-teacher',
    state: 'reviewing',
    revision: 1,
    resources: buildResources({
      cpuMillicores: 4000,
      memoryBytes: 16 * GIB,
      storageBytes: 100 * GIB,
      gpu: { class: 'nvidia-a100', count: 1 },
    }),
    durationSeconds: 14400,
  })

  const allocating = seedRequest({
    requestKey: 'allocating-cpu-request',
    courseId: 'course-101',
    requesterId: 'fixture-actor-student',
    state: 'allocating',
    revision: 2,
    resources: buildResources(),
    durationSeconds: 7200,
  })
  seedLease(allocating, 'allocating', 1)

  const active = seedRequest({
    requestKey: 'active-gpu-request',
    courseId: 'course-102',
    requesterId: 'fixture-actor-teacher',
    state: 'active',
    revision: 3,
    resources: buildResources({
      cpuMillicores: 4000,
      memoryBytes: 16 * GIB,
      storageBytes: 100 * GIB,
      gpu: { class: 'nvidia-a100', count: 1 },
    }),
    durationSeconds: 14400,
  })
  seedLease(active, 'active', 2, 14400)
}

export function listResourceRequests(): ResourceRequestSchema[] {
  return Array.from(requests.values())
}

export function getResourceRequest(requestId: string): ResourceRequestSchema | undefined {
  return requests.get(requestId)
}

export function listResourceLeases(): ResourceLeaseSchema[] {
  return Array.from(leases.values())
}

export function getResourceLease(leaseId: string): ResourceLeaseSchema | undefined {
  return leases.get(leaseId)
}

/** Simulates a concurrent writer so the next revision-fenced mutation fails. */
export function bumpRequestRevision(requestId: string): void {
  const request = requests.get(requestId)
  if (!request) return
  request.revision += 1
  request.updatedAt = nowIso()
}

/** Simulates a concurrent writer so the next revision-fenced mutation fails. */
export function bumpLeaseRevision(leaseId: string): void {
  const lease = leases.get(leaseId)
  if (!lease) return
  lease.revision += 1
  lease.updatedAt = nowIso()
}

function replayOrRecord<T extends ResourceOperationAcceptedSchema | ResourceLeaseSchema>(
  idempotencyKey: string,
  fingerprint: string,
  mutate: () => T | 'not-found' | 'invalid-state',
): ResourceMutationResult<T> {
  const cached = idempotencyMap.get(idempotencyKey)
  if (cached) {
    if (cached.fingerprint !== fingerprint) return { kind: 'conflict' }
    return { kind: 'ok', value: cached.response as T, replayed: true }
  }
  const value = mutate()
  if (value === 'not-found' || value === 'invalid-state') return { kind: value }
  idempotencyMap.set(idempotencyKey, { fingerprint, response: value })
  return { kind: 'ok', value, replayed: false }
}

function accepted(request: ResourceRequestSchema, lease?: ResourceLeaseSchema): ResourceOperationAcceptedSchema {
  return {
    requestId: request.id,
    leaseId: lease?.id ?? null,
    revision: request.revision,
    statusUrl: `/api/v1/resource-requests/${request.id}`,
  }
}

export function createResourceRequest(
  body: CreateResourceRequestSchema,
  actor: FixtureActor,
  idempotencyKey: string,
): ResourceMutationResult<ResourceRequestSchema> {
  const fingerprint = `create:${JSON.stringify(body)}`
  const result = replayOrRecord(idempotencyKey, fingerprint, () => {
    const request: ResourceRequestSchema = {
      id: nextUuid7('rreq'),
      generation: 1,
      requestKey: body.requestKey,
      requesterId: actor.actorId,
      courseId: body.courseId,
      projectId: body.projectId,
      target: {
        environmentId: body.environmentId,
        releaseId: body.releaseId,
        releaseVersion: body.releaseVersion,
        releaseSha256: body.releaseSha256,
      },
      requestedResources: body.resources,
      requestedDurationSeconds: body.durationSeconds,
      state: 'reviewing',
      revision: 1,
      createdAt: nowIso(),
      updatedAt: nowIso(),
      diagnosticCode: null,
    }
    requests.set(request.id, request)
    return request
  })
  return result
}

function transitionRequest(
  requestId: string,
  from: ResourceRequestState[],
  mutate: (request: ResourceRequestSchema) => ResourceLeaseSchema | undefined,
): ResourceMutationResult<ResourceOperationAcceptedSchema> {
  const request = requests.get(requestId)
  if (!request) return { kind: 'not-found' }
  if (!from.includes(request.state)) return { kind: 'invalid-state' }
  const lease = mutate(request)
  request.revision += 1
  request.updatedAt = nowIso()
  return { kind: 'ok', value: accepted(request, lease), replayed: false }
}

export function approveResourceRequest(
  requestId: string,
  body: ApproveResourceRequestSchema,
  idempotencyKey: string,
): ResourceMutationResult<ResourceOperationAcceptedSchema> {
  const fingerprint = `approve:${requestId}:${JSON.stringify(body)}`
  return replayOrRecord(idempotencyKey, fingerprint, () => {
    const outcome = transitionRequest(requestId, ['reviewing'], (request) => {
      request.state = 'allocating'
      request.diagnosticCode = null
      const lease: ResourceLeaseSchema = {
        id: nextUuid7('lease'),
        requestId: request.id,
        claimId: nextUuid7('claim'),
        state: 'allocating',
        revision: 1,
        activeFrom: null,
        expiresAt: null,
        revokeReasonCode: null,
        createdAt: nowIso(),
        updatedAt: nowIso(),
      }
      leases.set(lease.id, lease)
      return lease
    })
    if (outcome.kind !== 'ok') return outcome.kind
    return outcome.value
  })
}

export function rejectResourceRequest(
  requestId: string,
  body: ResourceRequestMutationSchema,
  idempotencyKey: string,
): ResourceMutationResult<ResourceOperationAcceptedSchema> {
  const fingerprint = `reject:${requestId}:${JSON.stringify(body)}`
  return replayOrRecord(idempotencyKey, fingerprint, () => {
    const outcome = transitionRequest(requestId, ['reviewing'], (request) => {
      request.state = 'rejected'
      return undefined
    })
    if (outcome.kind !== 'ok') return outcome.kind
    return outcome.value
  })
}

export function cancelResourceRequest(
  requestId: string,
  body: ResourceRequestMutationSchema,
  idempotencyKey: string,
): ResourceMutationResult<ResourceOperationAcceptedSchema> {
  const fingerprint = `cancel:${requestId}:${JSON.stringify(body)}`
  return replayOrRecord(idempotencyKey, fingerprint, () => {
    const outcome = transitionRequest(requestId, ['reviewing', 'allocating'], (request) => {
      request.state = 'cancelled'
      const pendingLease = Array.from(leases.values()).find(
        (lease) => lease.requestId === request.id && lease.state === 'allocating',
      )
      if (pendingLease) {
        pendingLease.state = 'revoked'
        pendingLease.revokeReasonCode = 'REQUEST_CANCELLED'
        pendingLease.revision += 1
        pendingLease.updatedAt = nowIso()
      }
      return undefined
    })
    if (outcome.kind !== 'ok') return outcome.kind
    return outcome.value
  })
}

export function retryResourceRequest(
  requestId: string,
  body: ResourceRequestMutationSchema,
  idempotencyKey: string,
): ResourceMutationResult<ResourceOperationAcceptedSchema> {
  const fingerprint = `retry:${requestId}:${JSON.stringify(body)}`
  return replayOrRecord(idempotencyKey, fingerprint, () => {
    const outcome = transitionRequest(requestId, ['allocating'], (request) => {
      // Retry re-drives a stuck allocation; the request stays in allocating.
      request.diagnosticCode = null
      return undefined
    })
    if (outcome.kind !== 'ok') return outcome.kind
    return outcome.value
  })
}

export function renewResourceLease(
  leaseId: string,
  body: RenewResourceLeaseSchema,
  idempotencyKey: string,
): ResourceMutationResult<ResourceLeaseSchema> {
  const fingerprint = `renew:${leaseId}:${JSON.stringify(body)}`
  return replayOrRecord(idempotencyKey, fingerprint, () => {
    const lease = leases.get(leaseId)
    if (!lease) return 'not-found'
    if (lease.state !== 'active' && lease.state !== 'expiring') return 'invalid-state'
    lease.activeFrom = lease.activeFrom ?? nowIso()
    lease.expiresAt = addSecondsIso(body.durationSeconds)
    lease.revision += 1
    lease.updatedAt = nowIso()
    return lease
  })
}

export function revokeResourceLease(
  leaseId: string,
  body: ResourceRequestMutationSchema,
  idempotencyKey: string,
): ResourceMutationResult<ResourceLeaseSchema> {
  const fingerprint = `revoke:${leaseId}:${JSON.stringify(body)}`
  return replayOrRecord(idempotencyKey, fingerprint, () => {
    const lease = leases.get(leaseId)
    if (!lease) return 'not-found'
    if (lease.state === 'expired' || lease.state === 'revoked') return 'invalid-state'
    lease.state = 'revoked'
    lease.revokeReasonCode = 'ADMIN_REVOKED'
    lease.revision += 1
    lease.updatedAt = nowIso()

    const request = requests.get(lease.requestId)
    if (request && (request.state === 'allocating' || request.state === 'active' || request.state === 'expiring')) {
      request.state = 'expired'
      request.revision += 1
      request.updatedAt = nowIso()
    }
    return lease
  })
}

export function resetResourceStore(): void {
  requests.clear()
  leases.clear()
  idempotencyMap.clear()
}
