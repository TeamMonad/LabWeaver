import type { RevokeAccessGrantRequestSchema } from '@/generated/contracts'
import { problem } from '../diagnostics'
import type { FixtureHandler } from '../types'
import { nextUuid7 } from '../utils/identity'
import { getAccessGrant, revokeAccessGrant } from '../stores/accessGrantStore'
import { extractPathParam, requireActor, requireIdempotencyKey, requireIfMatch, requireRole } from './index'

export const getAccessGrant: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const grantId = extractPathParam(req.url, /^\/api\/v1\/access-grants\/([^/]+)$/, 1)
  if (!grantId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的授权 ID', false)

  const grant = getAccessGrant(grantId)
  if (!grant) return problem(404, 'ACCESS_GRANT_NOT_FOUND', `未找到授权 ${grantId}`, false)

  const roleCheck = requireRole(actorResult, 'access_grant:read', { courseId: grant.courseId })
  if (roleCheck !== true) return roleCheck

  return { status: 200, data: grant }
}

export const revokeAccessGrant: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const grantId = extractPathParam(req.url, /^\/api\/v1\/access-grants\/([^/]+)\/revoke$/, 1)
  if (!grantId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的授权 ID', false)

  const grant = getAccessGrant(grantId)
  if (!grant) return problem(404, 'ACCESS_GRANT_NOT_FOUND', `未找到授权 ${grantId}`, false)

  const roleCheck = requireRole(actorResult, 'access_grant:revoke', { courseId: grant.courseId })
  if (roleCheck !== true) return roleCheck

  const idempotencyResult = requireIdempotencyKey(req)
  if (typeof idempotencyResult !== 'string') return idempotencyResult
  const ifMatchResult = requireIfMatch(req)
  if (typeof ifMatchResult !== 'string') return ifMatchResult

  if (String(grant.revision) !== ifMatchResult) {
    return problem(412, 'PRECONDITION_FAILED', 'If-Match revision 不匹配', false)
  }

  const body = req.body as RevokeAccessGrantRequestSchema
  const revoked = revokeAccessGrant(grantId, body?.reasonCode ?? 'fixture-revoke')
  if (!revoked) return problem(409, 'FIXTURE_INVALID_STATE', '授权无法撤销', false)
  return {
    status: 202,
    data: {
      operationId: nextUuid7('op'),
      revision: revoked.revision,
      statusUrl: `/api/v1/access-grants/${grantId}`,
    },
  }
}
