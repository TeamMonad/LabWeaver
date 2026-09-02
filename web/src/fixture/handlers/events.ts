import { problem } from '../diagnostics'
import type { FixtureHandler } from '../types'
import { toSseResponse } from '../stores/eventLog'
import { requireActor, requireRole } from './index'

export const streamEvents: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const url = new URL(req.url, 'http://localhost')
  const courseId = url.searchParams.get('courseId')
  if (!courseId) {
    return problem(400, 'FIXTURE_MISSING_QUERY', '缺少必需查询参数 courseId', false)
  }

  const roleCheck = requireRole(actorResult, 'events:read', { courseId })
  if (roleCheck !== true) return roleCheck

  const after = url.searchParams.get('after') ?? undefined
  const lastEventId = req.headers['Last-Event-ID'] ?? req.headers['last-event-id']
  return toSseResponse({
    courseId,
    after,
    lastEventId: typeof lastEventId === 'string' ? lastEventId : undefined,
  })
}
