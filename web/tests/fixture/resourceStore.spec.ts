import { beforeEach, describe, expect, it } from 'vitest'
import type { ApproveResourceRequestSchema, CreateResourceRequestSchema } from '@/generated/contracts'
import {
  approveResourceRequest,
  cancelResourceRequest,
  createResourceRequest,
  getResourceLease,
  getResourceRequest,
  listResourceLeases,
  listResourceRequests,
  rejectResourceRequest,
  renewResourceLease,
  resetResourceStore,
  retryResourceRequest,
  revokeResourceLease,
  seedResources,
  bumpRequestRevision,
} from '@/fixture/stores/resourceStore'
import type { FixtureActor } from '@/fixture/stores/actorStore'

const student: FixtureActor = { actorId: 'fixture-actor-student', role: 'student', courseIds: ['course-101'] }

function seedReviewingRequest() {
  const request = listResourceRequests().find((item) => item.requestKey === 'cpu-lab-request')
  expect(request).toBeDefined()
  return request!
}

function approveBody(request: { revision: number }): ApproveResourceRequestSchema {
  return {
    expectedRevision: request.revision,
    providerBinding: 'fixture-capacity',
    resources: { cpuMillicores: 2000, memoryBytes: 4 * 1024 ** 3, storageBytes: 20 * 1024 ** 3 },
    durationSeconds: 7200,
    reason: 'approved for the sprint demo',
  }
}

describe('resourceStore', () => {
  beforeEach(() => {
    resetResourceStore()
    seedResources()
  })

  it('seeds deterministic requests and leases across states', () => {
    const requests = listResourceRequests()
    expect(requests.map((request) => request.state).sort()).toEqual(['active', 'allocating', 'reviewing', 'reviewing'])
    const leases = listResourceLeases()
    expect(leases.map((lease) => lease.state).sort()).toEqual(['active', 'allocating'])
  })

  it('approves a reviewing request and creates an allocating lease', () => {
    const request = seedReviewingRequest()
    const initialRevision = request.revision
    const leaseCountBefore = listResourceLeases().length

    const result = approveResourceRequest(request.id, approveBody(request), 'idem-approve-1')
    expect(result.kind).toBe('ok')
    if (result.kind !== 'ok') return
    expect(result.replayed).toBe(false)
    expect(result.value.requestId).toBe(request.id)
    expect(result.value.leaseId).toBeTruthy()

    const updated = getResourceRequest(request.id)
    expect(updated?.state).toBe('allocating')
    expect(updated?.revision).toBe(initialRevision + 1)

    const leases = listResourceLeases()
    expect(leases).toHaveLength(leaseCountBefore + 1)
    const lease = leases.find((item) => item.id === result.value.leaseId)
    expect(lease?.state).toBe('allocating')
    expect(lease?.requestId).toBe(request.id)
  })

  it('rejects a second approve from a non-reviewing state', () => {
    const request = seedReviewingRequest()
    approveResourceRequest(request.id, approveBody(request), 'idem-approve-2')
    const second = approveResourceRequest(request.id, approveBody({ revision: request.revision + 1 }), 'idem-approve-3')
    expect(second.kind).toBe('invalid-state')
  })

  it('replays the same idempotency key without advancing the state machine', () => {
    const request = seedReviewingRequest()
    const initialRevision = request.revision
    const body = approveBody(request)
    const first = approveResourceRequest(request.id, body, 'idem-replay')
    const second = approveResourceRequest(request.id, body, 'idem-replay')
    expect(first.kind).toBe('ok')
    expect(second.kind).toBe('ok')
    if (first.kind !== 'ok' || second.kind !== 'ok') return
    expect(second.replayed).toBe(true)
    expect(second.value).toEqual(first.value)
    expect(getResourceRequest(request.id)?.revision).toBe(initialRevision + 1)
    expect(listResourceLeases().filter((lease) => lease.requestId === request.id)).toHaveLength(1)
  })

  it('conflicts when an idempotency key is reused with a different payload', () => {
    const request = seedReviewingRequest()
    approveResourceRequest(request.id, approveBody(request), 'idem-conflict')
    const replayed = approveResourceRequest(
      request.id,
      { ...approveBody(request), reason: 'different reason' },
      'idem-conflict',
    )
    expect(replayed.kind).toBe('conflict')
  })

  it('rejects a reviewing request and blocks further mutations', () => {
    const request = seedReviewingRequest()
    const initialRevision = request.revision
    const result = rejectResourceRequest(request.id, { expectedRevision: initialRevision, reason: 'quota exceeded' }, 'idem-reject-1')
    expect(result.kind).toBe('ok')
    expect(getResourceRequest(request.id)?.state).toBe('rejected')

    const retry = retryResourceRequest(request.id, { expectedRevision: initialRevision + 1, reason: 'retry' }, 'idem-retry-1')
    expect(retry.kind).toBe('invalid-state')
  })

  it('retries only allocating requests', () => {
    const allocating = listResourceRequests().find((item) => item.state === 'allocating')!
    const initialRevision = allocating.revision
    const result = retryResourceRequest(allocating.id, { expectedRevision: initialRevision, reason: 're-drive allocation' }, 'idem-retry-2')
    expect(result.kind).toBe('ok')
    expect(getResourceRequest(allocating.id)?.state).toBe('allocating')
    expect(getResourceRequest(allocating.id)?.revision).toBe(initialRevision + 1)

    const reviewing = seedReviewingRequest()
    const invalid = retryResourceRequest(reviewing.id, { expectedRevision: reviewing.revision, reason: 'retry' }, 'idem-retry-3')
    expect(invalid.kind).toBe('invalid-state')
  })

  it('cancels an allocating request and revokes its pending lease', () => {
    const allocating = listResourceRequests().find((item) => item.state === 'allocating')!
    const result = cancelResourceRequest(allocating.id, { expectedRevision: allocating.revision, reason: 'requester withdrew' }, 'idem-cancel-1')
    expect(result.kind).toBe('ok')
    expect(getResourceRequest(allocating.id)?.state).toBe('cancelled')
    const lease = listResourceLeases().find((item) => item.requestId === allocating.id)
    expect(lease?.state).toBe('revoked')
    expect(lease?.revokeReasonCode).toBe('REQUEST_CANCELLED')
  })

  it('renews an active lease and rejects renewal of an allocating lease', () => {
    const active = listResourceLeases().find((lease) => lease.state === 'active')!
    const initialRevision = active.revision
    const result = renewResourceLease(active.id, { expectedRevision: initialRevision, durationSeconds: 3600, reason: 'extend window' }, 'idem-renew-1')
    expect(result.kind).toBe('ok')
    if (result.kind !== 'ok') return
    expect(result.value.revision).toBe(initialRevision + 1)
    expect(result.value.expiresAt).toBe('2026-07-11T11:00:00.000Z')

    const allocating = listResourceLeases().find((lease) => lease.state === 'allocating')!
    const invalid = renewResourceLease(allocating.id, { expectedRevision: allocating.revision, durationSeconds: 3600, reason: 'extend' }, 'idem-renew-2')
    expect(invalid.kind).toBe('invalid-state')
  })

  it('revokes an active lease and expires the owning request', () => {
    const active = listResourceLeases().find((lease) => lease.state === 'active')!
    const initialRevision = active.revision
    const result = revokeResourceLease(active.id, { expectedRevision: initialRevision, reason: 'policy violation' }, 'idem-revoke-1')
    expect(result.kind).toBe('ok')
    if (result.kind !== 'ok') return
    expect(result.value.state).toBe('revoked')
    expect(result.value.revokeReasonCode).toBe('ADMIN_REVOKED')
    expect(getResourceRequest(active.requestId)?.state).toBe('expired')

    const again = revokeResourceLease(active.id, { expectedRevision: initialRevision + 1, reason: 'again' }, 'idem-revoke-2')
    expect(again.kind).toBe('invalid-state')
  })

  it('creates a request in reviewing state with idempotent replay', () => {
    const body: CreateResourceRequestSchema = {
      courseId: 'course-101',
      projectId: 'proj-course-101',
      environmentId: 'env-fixture-1',
      releaseId: 'release-fixture-1',
      releaseSha256: 'd'.repeat(64),
      releaseVersion: 4,
      requestKey: 'new-lab-request',
      durationSeconds: 3600,
      resources: { cpuMillicores: 1000, memoryBytes: 2 * 1024 ** 3, storageBytes: 10 * 1024 ** 3 },
    }
    const created = createResourceRequest(body, student, 'idem-create-1')
    expect(created.kind).toBe('ok')
    if (created.kind !== 'ok') return
    expect(created.value.state).toBe('reviewing')
    expect(created.value.revision).toBe(1)
    expect(created.value.requesterId).toBe(student.actorId)

    const replay = createResourceRequest(body, student, 'idem-create-1')
    expect(replay.kind).toBe('ok')
    if (replay.kind !== 'ok') return
    expect(replay.replayed).toBe(true)
    expect(replay.value.id).toBe(created.value.id)
  })

  it('bumpRequestRevision simulates a concurrent writer', () => {
    const request = seedReviewingRequest()
    const initialRevision = request.revision
    bumpRequestRevision(request.id)
    expect(getResourceRequest(request.id)?.revision).toBe(initialRevision + 1)
  })

  it('returns not-found for unknown identifiers', () => {
    const result = approveResourceRequest('rreq-missing', approveBody({ revision: 1 }), 'idem-missing')
    expect(result.kind).toBe('not-found')
    expect(getResourceLease('lease-missing')).toBeUndefined()
  })
})
