import type {
  ApproveResourceRequestSchema,
  CreateResourceRequestSchema,
  RenewResourceLeaseSchema,
  ResourceRequestMutationSchema,
  WorkloadResources,
} from '@/generated/contracts'
import { conflict, problem } from '../diagnostics'
import { consumeResourceRevisionConflict } from '../scenarioFlags'
import * as resourceStore from '../stores/resourceStore'
import type { FixtureActor } from '../stores/actorStore'
import type { FixtureHandler, FixtureResponse } from '../types'
import {
  extractPathParam,
  parseIfMatchRevision,
  requireActor,
  requireIdempotencyKey,
  requireIfMatch,
  requireRole,
} from './index'

function etag(revision: number): string {
  return `"rev-${revision}"`
}

function isValidReason(value: unknown): value is string {
  if (typeof value !== 'string') return false
  const length = value.trim().length
  return length >= 1 && length <= 500
}

function isValidResources(value: unknown): value is WorkloadResources {
  if (!value || typeof value !== 'object') return false
  const resources = value as WorkloadResources
  if (!Number.isInteger(resources.cpuMillicores) || resources.cpuMillicores <= 0) return false
  if (!Number.isInteger(resources.memoryBytes) || resources.memoryBytes <= 0) return false
  if (!Number.isInteger(resources.storageBytes) || resources.storageBytes <= 0) return false
  if (resources.gpu !== undefined && resources.gpu !== null) {
    const gpu = resources.gpu
    if (typeof gpu.class !== 'string' || gpu.class.length === 0 || gpu.class.length > 63) return false
    if (!Number.isInteger(gpu.count) || gpu.count <= 0) return false
  }
  return true
}

function parseApproveBody(raw: unknown): ApproveResourceRequestSchema | null {
  const body = raw as ApproveResourceRequestSchema | undefined
  if (!body || typeof body !== 'object') return null
  if (!Number.isInteger(body.expectedRevision) || body.expectedRevision <= 0) return null
  if (typeof body.providerBinding !== 'string' || body.providerBinding.length === 0 || body.providerBinding.length > 120) return null
  if (!isValidReason(body.reason)) return null
  if (!Number.isInteger(body.durationSeconds) || body.durationSeconds <= 0) return null
  if (!isValidResources(body.resources)) return null
  return body
}

function parseMutationBody(raw: unknown): ResourceRequestMutationSchema | null {
  const body = raw as ResourceRequestMutationSchema | undefined
  if (!body || typeof body !== 'object') return null
  if (!Number.isInteger(body.expectedRevision) || body.expectedRevision <= 0) return null
  if (!isValidReason(body.reason)) return null
  return body
}

function parseRenewBody(raw: unknown): RenewResourceLeaseSchema | null {
  const body = raw as RenewResourceLeaseSchema | undefined
  if (!body || typeof body !== 'object') return null
  if (!Number.isInteger(body.expectedRevision) || body.expectedRevision <= 0) return null
  if (!Number.isInteger(body.durationSeconds) || body.durationSeconds <= 0) return null
  if (!isValidReason(body.reason)) return null
  return body
}

function parseCreateBody(raw: unknown): CreateResourceRequestSchema | null {
  const body = raw as CreateResourceRequestSchema | undefined
  if (!body || typeof body !== 'object') return null
  if (typeof body.courseId !== 'string' || !body.courseId) return null
  if (typeof body.projectId !== 'string' || !body.projectId) return null
  if (typeof body.environmentId !== 'string' || !body.environmentId) return null
  if (typeof body.releaseId !== 'string' || !body.releaseId) return null
  if (typeof body.releaseSha256 !== 'string' || body.releaseSha256.length !== 64) return null
  if (!Number.isInteger(body.releaseVersion) || body.releaseVersion <= 0) return null
  if (typeof body.requestKey !== 'string' || !body.requestKey) return null
  if (!Number.isInteger(body.durationSeconds) || body.durationSeconds <= 0) return null
  if (!isValidResources(body.resources)) return null
  return body
}

interface RevisionFence {
  /** Live read of the entity revision, evaluated after the simulated bump. */
  currentRevision: () => number
  expectedRevision: number
}

/**
 * Enforces the revision fence shared by every Resource mutation: a parsed
 * strong If-Match validator plus the body `expectedRevision`, both compared
 * against the current entity revision. The one-shot scenario flag simulates a
 * concurrent writer so the stale-client conflict path is demonstrable.
 */
function checkRevisionFence(
  req: Parameters<FixtureHandler>[0],
  fence: RevisionFence,
  bump: () => void,
): FixtureResponse | null {
  const ifMatchResult = requireIfMatch(req)
  if (typeof ifMatchResult !== 'string') return ifMatchResult
  const ifMatchRevision = parseIfMatchRevision(ifMatchResult)
  if (ifMatchRevision === null) {
    return problem(412, 'PRECONDITION_FAILED', 'If-Match header 不是有效的强 ETag revision', false)
  }

  if (consumeResourceRevisionConflict()) bump()

  const entityRevision = fence.currentRevision()
  if (ifMatchRevision !== entityRevision) {
    return problem(412, 'PRECONDITION_FAILED', 'If-Match revision 不匹配', false)
  }
  if (fence.expectedRevision !== entityRevision) {
    return problem(412, 'PRECONDITION_FAILED', 'expectedRevision 与当前 revision 不匹配', false)
  }
  return null
}

function isAdmin(actor: FixtureActor): boolean {
  return actor.role === 'platform-admin'
}

export const listResourceRequests: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const roleCheck = requireRole(actorResult, 'resource_request:read')
  if (roleCheck !== true) return roleCheck

  const items = resourceStore
    .listResourceRequests()
    .filter((request) => isAdmin(actorResult) || request.requesterId === actorResult.actorId)
  return { status: 200, data: items }
}

export const createResourceRequest: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const roleCheck = requireRole(actorResult, 'resource_request:write')
  if (roleCheck !== true) return roleCheck

  const idempotencyResult = requireIdempotencyKey(req)
  if (typeof idempotencyResult !== 'string') return idempotencyResult

  const body = parseCreateBody(req.body)
  if (!body) return problem(422, 'UNPROCESSABLE_ENTITY', '无效的资源申请请求体', false)

  const result = resourceStore.createResourceRequest(body, actorResult, idempotencyResult)
  if (result.kind === 'conflict') return conflict('Idempotency-Key 已被用于不同的请求')
  if (result.kind !== 'ok') return problem(409, 'FIXTURE_INVALID_STATE', '资源申请无法创建', false)
  return {
    status: 201,
    data: result.value,
    headers: { ETag: etag(result.value.revision) },
  }
}

export const getResourceRequest: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const requestId = extractPathParam(req.url, /^\/api\/v1\/resource-requests\/([^/]+)$/, 1)
  if (!requestId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的资源申请 ID', false)

  const request = resourceStore.getResourceRequest(requestId)
  if (!request) return problem(404, 'RESOURCE_REQUEST_NOT_FOUND', `未找到资源申请 ${requestId}`, false)

  const roleCheck = requireRole(actorResult, 'resource_request:read', { actorId: request.requesterId })
  if (roleCheck !== true) return roleCheck
  if (!isAdmin(actorResult) && request.requesterId !== actorResult.actorId) {
    return problem(403, 'FORBIDDEN', '只能读取本人提交的资源申请', false)
  }

  return { status: 200, data: request, headers: { ETag: etag(request.revision) } }
}

type RequestMutationOp = 'approve' | 'resize-and-approve' | 'reject' | 'cancel' | 'retry'

function requestMutation(op: RequestMutationOp): FixtureHandler {
  return (req) => {
    const actorResult = requireActor(req)
    if (!('role' in actorResult)) return actorResult

    const requestId = extractPathParam(req.url, new RegExp(`^\\/api\\/v1\\/resource-requests\\/([^/]+)\\/${op}$`), 1)
    if (!requestId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的资源申请 ID', false)

    const request = resourceStore.getResourceRequest(requestId)
    if (!request) return problem(404, 'RESOURCE_REQUEST_NOT_FOUND', `未找到资源申请 ${requestId}`, false)

    const action = op === 'approve' || op === 'resize-and-approve'
      ? 'resource_request:approve'
      : op === 'cancel'
        ? 'resource_request:cancel'
        : 'resource_request:retry'
    const roleCheck = requireRole(actorResult, action)
    if (roleCheck !== true) return roleCheck

    const idempotencyResult = requireIdempotencyKey(req)
    if (typeof idempotencyResult !== 'string') return idempotencyResult

    if (op === 'approve' || op === 'resize-and-approve') {
      const body = parseApproveBody(req.body)
      if (!body) return problem(422, 'UNPROCESSABLE_ENTITY', '无效的审批载荷：providerBinding、resources、durationSeconds 与 reason(1-500 字）均为必填', false)
      const fenceFailure = checkRevisionFence(
        req,
        { currentRevision: () => resourceStore.getResourceRequest(requestId)?.revision ?? request.revision, expectedRevision: body.expectedRevision },
        () => resourceStore.bumpRequestRevision(requestId),
      )
      if (fenceFailure) return fenceFailure

      const result = resourceStore.approveResourceRequest(requestId, body, idempotencyResult)
      if (result.kind === 'conflict') return conflict('Idempotency-Key 已被用于不同的请求')
      if (result.kind === 'invalid-state') return problem(409, 'FIXTURE_INVALID_STATE', `当前状态 ${request.state} 不允许批准`, false)
      if (result.kind !== 'ok') return problem(404, 'RESOURCE_REQUEST_NOT_FOUND', `未找到资源申请 ${requestId}`, false)
      return { status: 202, data: result.value, headers: { ETag: etag(result.value.revision) } }
    }

    const body = parseMutationBody(req.body)
    if (!body) return problem(422, 'UNPROCESSABLE_ENTITY', '无效的变更载荷：expectedRevision 与 reason(1-500 字）为必填', false)
    const fenceFailure = checkRevisionFence(
      req,
      { currentRevision: () => resourceStore.getResourceRequest(requestId)?.revision ?? request.revision, expectedRevision: body.expectedRevision },
      () => resourceStore.bumpRequestRevision(requestId),
    )
    if (fenceFailure) return fenceFailure

    const result = op === 'reject'
      ? resourceStore.rejectResourceRequest(requestId, body, idempotencyResult)
      : op === 'cancel'
        ? resourceStore.cancelResourceRequest(requestId, body, idempotencyResult)
        : resourceStore.retryResourceRequest(requestId, body, idempotencyResult)
    if (result.kind === 'conflict') return conflict('Idempotency-Key 已被用于不同的请求')
    if (result.kind === 'invalid-state') return problem(409, 'FIXTURE_INVALID_STATE', `当前状态 ${request.state} 不允许该操作`, false)
    if (result.kind !== 'ok') return problem(404, 'RESOURCE_REQUEST_NOT_FOUND', `未找到资源申请 ${requestId}`, false)
    return { status: 202, data: result.value, headers: { ETag: etag(result.value.revision) } }
  }
}

export const approveResourceRequest = requestMutation('approve')
export const resizeAndApproveResourceRequest = requestMutation('resize-and-approve')
export const rejectResourceRequest = requestMutation('reject')
export const cancelResourceRequest = requestMutation('cancel')
export const retryResourceRequest = requestMutation('retry')

export const listResourceLeases: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const roleCheck = requireRole(actorResult, 'resource_lease:read')
  if (roleCheck !== true) return roleCheck

  return { status: 200, data: resourceStore.listResourceLeases() }
}

export const getResourceLease: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const leaseId = extractPathParam(req.url, /^\/api\/v1\/resource-leases\/([^/]+)$/, 1)
  if (!leaseId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的 Lease ID', false)

  const lease = resourceStore.getResourceLease(leaseId)
  if (!lease) return problem(404, 'RESOURCE_LEASE_NOT_FOUND', `未找到 Lease ${leaseId}`, false)

  const roleCheck = requireRole(actorResult, 'resource_lease:read')
  if (roleCheck !== true) return roleCheck

  return { status: 200, data: lease, headers: { ETag: etag(lease.revision) } }
}

export const renewResourceLease: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const leaseId = extractPathParam(req.url, /^\/api\/v1\/resource-leases\/([^/]+)\/renew$/, 1)
  if (!leaseId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的 Lease ID', false)

  const lease = resourceStore.getResourceLease(leaseId)
  if (!lease) return problem(404, 'RESOURCE_LEASE_NOT_FOUND', `未找到 Lease ${leaseId}`, false)

  const roleCheck = requireRole(actorResult, 'resource_lease:renew')
  if (roleCheck !== true) return roleCheck

  const idempotencyResult = requireIdempotencyKey(req)
  if (typeof idempotencyResult !== 'string') return idempotencyResult

  const body = parseRenewBody(req.body)
  if (!body) return problem(422, 'UNPROCESSABLE_ENTITY', '无效的续期载荷：expectedRevision、durationSeconds 与 reason(1-500 字）为必填', false)
  const fenceFailure = checkRevisionFence(
    req,
    { currentRevision: () => resourceStore.getResourceLease(leaseId)?.revision ?? lease.revision, expectedRevision: body.expectedRevision },
    () => resourceStore.bumpLeaseRevision(leaseId),
  )
  if (fenceFailure) return fenceFailure

  const result = resourceStore.renewResourceLease(leaseId, body, idempotencyResult)
  if (result.kind === 'conflict') return conflict('Idempotency-Key 已被用于不同的请求')
  if (result.kind === 'invalid-state') return problem(409, 'FIXTURE_INVALID_STATE', `当前状态 ${lease.state} 不允许续期`, false)
  if (result.kind !== 'ok') return problem(404, 'RESOURCE_LEASE_NOT_FOUND', `未找到 Lease ${leaseId}`, false)
  return { status: 200, data: result.value, headers: { ETag: etag(result.value.revision) } }
}

export const revokeResourceLease: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const leaseId = extractPathParam(req.url, /^\/api\/v1\/resource-leases\/([^/]+)\/revoke$/, 1)
  if (!leaseId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的 Lease ID', false)

  const lease = resourceStore.getResourceLease(leaseId)
  if (!lease) return problem(404, 'RESOURCE_LEASE_NOT_FOUND', `未找到 Lease ${leaseId}`, false)

  const roleCheck = requireRole(actorResult, 'resource_lease:revoke')
  if (roleCheck !== true) return roleCheck

  const idempotencyResult = requireIdempotencyKey(req)
  if (typeof idempotencyResult !== 'string') return idempotencyResult

  const body = parseMutationBody(req.body)
  if (!body) return problem(422, 'UNPROCESSABLE_ENTITY', '无效的撤销载荷：expectedRevision 与 reason(1-500 字）为必填', false)
  const fenceFailure = checkRevisionFence(
    req,
    { currentRevision: () => resourceStore.getResourceLease(leaseId)?.revision ?? lease.revision, expectedRevision: body.expectedRevision },
    () => resourceStore.bumpLeaseRevision(leaseId),
  )
  if (fenceFailure) return fenceFailure

  const result = resourceStore.revokeResourceLease(leaseId, body, idempotencyResult)
  if (result.kind === 'conflict') return conflict('Idempotency-Key 已被用于不同的请求')
  if (result.kind === 'invalid-state') return problem(409, 'FIXTURE_INVALID_STATE', `当前状态 ${lease.state} 不允许撤销`, false)
  if (result.kind !== 'ok') return problem(404, 'RESOURCE_LEASE_NOT_FOUND', `未找到 Lease ${leaseId}`, false)
  return { status: 200, data: result.value, headers: { ETag: etag(result.value.revision) } }
}
