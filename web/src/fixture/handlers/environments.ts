import type {
  CreateEnvironmentRequestSchema,
  EnvironmentOperationAcceptedSchema,
  EnvironmentSummaryPageSchema,
} from '@/generated/contracts'
import { nowIso } from '../utils/clock'
import { problem } from '../diagnostics'
import type { FixtureHandler } from '../types'
import * as environmentStore from '../stores/environmentStore'
import { extractPathParam, requireActor, requireIdempotencyKey, requireIfMatch, requireRole } from './index'

export const listEnvironmentsHandler: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const url = new URL(req.url, 'http://localhost')
  const courseId = url.searchParams.get('courseId')
  if (!courseId) {
    return problem(400, 'FIXTURE_MISSING_QUERY', '缺少必需查询参数 courseId', false)
  }

  const roleCheck = requireRole(actorResult, 'environment:read', { courseId })
  if (roleCheck !== true) return roleCheck

  const items = environmentStore.listEnvironments(courseId)
  const page: EnvironmentSummaryPageSchema = {
    items,
    snapshotAt: nowIso(),
    snapshotSequence: items.length > 0 ? items[items.length - 1].lastChangedStreamSequence : '0',
  }
  return { status: 200, data: page }
}

export const createEnvironmentHandler: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const body = req.body as CreateEnvironmentRequestSchema
  if (!body?.courseId || !body?.releaseId || body?.releaseVersion === undefined) {
    return problem(400, 'FIXTURE_INVALID_BODY', '请求体缺少必需字段', false)
  }

  const roleCheck = requireRole(actorResult, 'environment:write', { courseId: body.courseId })
  if (roleCheck !== true) return roleCheck

  const idempotencyResult = requireIdempotencyKey(req)
  if (typeof idempotencyResult !== 'string') return idempotencyResult

  const accepted = environmentStore.createEnvironment(body, actorResult, idempotencyResult)
  return { status: 202, data: accepted }
}

export const getEnvironmentHandler: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const environmentId = extractPathParam(req.url, /^\/api\/v1\/environments\/([^/]+)$/, 1)
  if (!environmentId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的环境 ID', false)

  const instance = environmentStore.getEnvironment(environmentId)
  if (!instance) return problem(404, 'ENVIRONMENT_NOT_FOUND', `未找到环境 ${environmentId}`, false)

  const roleCheck = requireRole(actorResult, 'environment:read', { courseId: instance.courseId })
  if (roleCheck !== true) return roleCheck

  return { status: 200, data: instance }
}

export const deleteEnvironmentHandler: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const environmentId = extractPathParam(req.url, /^\/api\/v1\/environments\/([^/]+)$/, 1)
  if (!environmentId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的环境 ID', false)

  const instance = environmentStore.getEnvironment(environmentId)
  if (!instance) return problem(404, 'ENVIRONMENT_NOT_FOUND', `未找到环境 ${environmentId}`, false)

  const roleCheck = requireRole(actorResult, 'environment:delete', { courseId: instance.courseId })
  if (roleCheck !== true) return roleCheck

  const idempotencyResult = requireIdempotencyKey(req)
  if (typeof idempotencyResult !== 'string') return idempotencyResult
  const ifMatchResult = requireIfMatch(req)
  if (typeof ifMatchResult !== 'string') return ifMatchResult

  const accepted = environmentStore.deleteEnvironment(environmentId, ifMatchResult, idempotencyResult)
  if (!accepted) return problem(412, 'PRECONDITION_FAILED', 'If-Match revision 不匹配', false)
  return { status: 202, data: accepted }
}

function createOperationHandler(kind: 'start' | 'stop' | 'restart' | 'reset' | 'recover' | 'retry'): FixtureHandler {
  return (req) => {
    const actorResult = requireActor(req)
    if (!('role' in actorResult)) return actorResult

    const environmentId = extractPathParam(req.url, /^\/api\/v1\/environments\/([^/]+)\/(start|stop|restart|reset|recover|retry)$/, 1)
    if (!environmentId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的环境 ID', false)

    const instance = environmentStore.getEnvironment(environmentId)
    if (!instance) return problem(404, 'ENVIRONMENT_NOT_FOUND', `未找到环境 ${environmentId}`, false)

    const roleCheck = requireRole(actorResult, 'environment:write', { courseId: instance.courseId })
    if (roleCheck !== true) return roleCheck

    let accepted: EnvironmentOperationAcceptedSchema | null = null
    switch (kind) {
      case 'start':
        accepted = environmentStore.startEnvironment(environmentId)
        break
      case 'stop':
        accepted = environmentStore.stopEnvironment(environmentId)
        break
      case 'restart':
        accepted = environmentStore.restartEnvironment(environmentId)
        break
      case 'reset':
        accepted = environmentStore.resetEnvironment(environmentId)
        break
      case 'recover':
        accepted = environmentStore.recoverEnvironment(environmentId)
        break
      case 'retry':
        accepted = environmentStore.retryEnvironment(environmentId)
        break
    }
    if (!accepted) return problem(409, 'FIXTURE_INVALID_STATE', '当前状态不允许该操作', false)
    return { status: 202, data: accepted }
  }
}

export const startEnvironment = createOperationHandler('start')
export const stopEnvironment = createOperationHandler('stop')
export const restartEnvironment = createOperationHandler('restart')
export const resetEnvironment = createOperationHandler('reset')
export const recoverEnvironment = createOperationHandler('recover')
export const retryEnvironment = createOperationHandler('retry')

export const cancelEnvironmentHandler: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const environmentId = extractPathParam(req.url, /^\/api\/v1\/environments\/([^/]+)\/cancel$/, 1)
  if (!environmentId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的环境 ID', false)

  const instance = environmentStore.getEnvironment(environmentId)
  if (!instance) return problem(404, 'ENVIRONMENT_NOT_FOUND', `未找到环境 ${environmentId}`, false)

  const roleCheck = requireRole(actorResult, 'environment:write', { courseId: instance.courseId })
  if (roleCheck !== true) return roleCheck

  const idempotencyResult = requireIdempotencyKey(req)
  if (typeof idempotencyResult !== 'string') return idempotencyResult
  const ifMatchResult = requireIfMatch(req)
  if (typeof ifMatchResult !== 'string') return ifMatchResult

  const accepted = environmentStore.cancelEnvironment(environmentId)
  if (!accepted) return problem(409, 'FIXTURE_INVALID_STATE', '当前没有可取消的操作', false)
  return { status: 202, data: accepted }
}

export const freezeEnvironmentHandler: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const environmentId = extractPathParam(req.url, /^\/api\/v1\/environments\/([^/]+)\/freeze$/, 1)
  if (!environmentId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的环境 ID', false)

  const instance = environmentStore.getEnvironment(environmentId)
  if (!instance) return problem(404, 'ENVIRONMENT_NOT_FOUND', `未找到环境 ${environmentId}`, false)

  const roleCheck = requireRole(actorResult, 'submission:freeze', { courseId: instance.courseId })
  if (roleCheck !== true) return roleCheck

  const idempotencyResult = requireIdempotencyKey(req)
  if (typeof idempotencyResult !== 'string') return idempotencyResult
  const ifMatchResult = requireIfMatch(req)
  if (typeof ifMatchResult !== 'string') return ifMatchResult

  const accepted = environmentStore.freezeEnvironment(environmentId, ifMatchResult, idempotencyResult)
  if (!accepted) return problem(412, 'PRECONDITION_FAILED', 'If-Match revision 不匹配', false)
  return { status: 202, data: accepted }
}
