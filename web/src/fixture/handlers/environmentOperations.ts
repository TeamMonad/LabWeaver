import type { EnvironmentOperationPageSchema } from '@/generated/contracts'
import { nowIso } from '../utils/clock'
import { problem } from '../diagnostics'
import type { FixtureHandler } from '../types'
import { getEnvironment } from '../stores/environmentStore'
import { findOperation, findOperationsForEnvironment, toOperationSnapshot } from '../stores/operationStore'
import { extractPathParam, requireActor, requireRole } from './index'

export const listEnvironmentOperations: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const environmentId = extractPathParam(req.url, /^\/api\/v1\/environments\/([^/]+)\/operations$/, 1)
  if (!environmentId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的环境 ID', false)

  const instance = getEnvironment(environmentId)
  if (!instance) return problem(404, 'ENVIRONMENT_NOT_FOUND', `未找到环境 ${environmentId}`, false)

  const roleCheck = requireRole(actorResult, 'environment:read', { courseId: instance.courseId })
  if (roleCheck !== true) return roleCheck

  const operations = findOperationsForEnvironment(environmentId)
  const page: EnvironmentOperationPageSchema = {
    items: operations.map(toOperationSnapshot),
    snapshotAt: nowIso(),
    snapshotSequence: operations.length > 0 ? operations[operations.length - 1].currentRevision.toString(16).padStart(16, '0') : '0',
  }
  return { status: 200, data: page }
}

export const getEnvironmentOperation: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const environmentId = extractPathParam(req.url, /^\/api\/v1\/environments\/([^/]+)\/operations\/([^/]+)$/, 1)
  const operationId = extractPathParam(req.url, /^\/api\/v1\/environments\/([^/]+)\/operations\/([^/]+)$/, 2)
  if (!environmentId || !operationId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的路径参数', false)

  const instance = getEnvironment(environmentId)
  if (!instance) return problem(404, 'ENVIRONMENT_NOT_FOUND', `未找到环境 ${environmentId}`, false)

  const roleCheck = requireRole(actorResult, 'environment:read', { courseId: instance.courseId })
  if (roleCheck !== true) return roleCheck

  const operation = findOperation(operationId)
  if (!operation || operation.environmentId !== environmentId) {
    return problem(404, 'OPERATION_NOT_FOUND', `未找到操作 ${operationId}`, false)
  }

  return { status: 200, data: toOperationSnapshot(operation) }
}

