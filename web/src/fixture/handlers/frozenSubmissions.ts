import { problem } from '../diagnostics'
import type { FixtureHandler } from '../types'
import { getFrozenSubmission } from '../stores/environmentStore'
import { extractPathParam, requireActor, requireRole } from './index'

export const getFrozenSubmissionHandler: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const submissionId = extractPathParam(req.url, /^\/api\/v1\/frozen-submissions\/([^/]+)$/, 1)
  if (!submissionId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的冻结提交 ID', false)

  const submission = getFrozenSubmission(submissionId)
  if (!submission) {
    return problem(404, 'LW_COLLECT_SUBMISSION_NOT_FOUND', `未找到冻结提交 ${submissionId}`, true)
  }

  const roleCheck = requireRole(actorResult, 'environment:read', { courseId: submission.courseId })
  if (roleCheck !== true) return roleCheck
  return { status: 200, data: submission }
}
