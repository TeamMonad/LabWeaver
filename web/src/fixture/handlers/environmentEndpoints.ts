import type { EnvironmentEndpointSchema } from '@/generated/contracts'
import { problem } from '../diagnostics'
import type { FixtureHandler } from '../types'
import { getEnvironment } from '../stores/environmentStore'
import { listEndpoints } from '../stores/endpointStore'
import { extractPathParam, requireActor, requireRole } from './index'

export const listEnvironmentEndpoints: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const environmentId = extractPathParam(req.url, /^\/api\/v1\/environments\/([^/]+)\/endpoints$/, 1)
  if (!environmentId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的环境 ID', false)

  const instance = getEnvironment(environmentId)
  if (!instance) return problem(404, 'ENVIRONMENT_NOT_FOUND', `未找到环境 ${environmentId}`, false)

  const roleCheck = requireRole(actorResult, 'environment:read', { courseId: instance.courseId })
  if (roleCheck !== true) return roleCheck

  const endpoints: EnvironmentEndpointSchema[] = listEndpoints(environmentId)
  return { status: 200, data: { items: endpoints } }
}
