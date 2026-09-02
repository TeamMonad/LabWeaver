import type { CreateAccessGrantRequestSchema, EnvironmentAccessGrantPageSchema } from '@/generated/contracts'
import { nowIso } from '../utils/clock'
import { conflict, problem } from '../diagnostics'
import type { FixtureHandler } from '../types'
import { createAccessGrant, listEnvironmentAccessGrants, toAccessGrantSnapshot } from '../stores/accessGrantStore'
import { getEnvironment } from '../stores/environmentStore'
import { extractPathParam, requireActor, requireIdempotencyKey, requireRole } from './index'

export const listEnvironmentAccessGrants: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const environmentId = extractPathParam(req.url, /^\/api\/v1\/environments\/([^/]+)\/access-grants$/, 1)
  if (!environmentId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的环境 ID', false)

  const instance = getEnvironment(environmentId)
  if (!instance) return problem(404, 'ENVIRONMENT_NOT_FOUND', `未找到环境 ${environmentId}`, false)

  const roleCheck = requireRole(actorResult, 'access_grant:read', { courseId: instance.courseId })
  if (roleCheck !== true) return roleCheck

  const grants = listEnvironmentAccessGrants(environmentId)
  const page: EnvironmentAccessGrantPageSchema = {
    items: grants.map(toAccessGrantSnapshot),
    snapshotAt: nowIso(),
    snapshotSequence: grants.length > 0 ? grants[grants.length - 1].revision.toString(16).padStart(16, '0') : '0',
  }
  return { status: 200, data: page }
}

export const createEnvironmentAccessGrant: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const environmentId = extractPathParam(req.url, /^\/api\/v1\/environments\/([^/]+)\/access-grants$/, 1)
  if (!environmentId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的环境 ID', false)

  const instance = getEnvironment(environmentId)
  if (!instance) return problem(404, 'ENVIRONMENT_NOT_FOUND', `未找到环境 ${environmentId}`, false)

  const roleCheck = requireRole(actorResult, 'access_grant:write', { courseId: instance.courseId })
  if (roleCheck !== true) return roleCheck

  const idempotencyResult = requireIdempotencyKey(req)
  if (typeof idempotencyResult !== 'string') return idempotencyResult

  const body = req.body as CreateAccessGrantRequestSchema
  const result = createAccessGrant(body, actorResult, idempotencyResult)
  if (result === 'revision-mismatch') {
    return problem(412, 'PRECONDITION_FAILED', 'environmentRevision 不匹配', false)
  }
  if (result === 'endpoint-missing') {
    return problem(404, 'ENDPOINT_NOT_FOUND', '指定的 endpoint 不存在', false)
  }
  if (result === 'conflict') {
    return conflict('Idempotency-Key 已被用于不同的请求')
  }
  return { status: 200, data: result }
}
