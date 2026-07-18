import { problem } from '../diagnostics'
import type { FixtureHandler } from '../types'
import { getActivePolicy, rotatePolicy } from '../stores/llmPolicyStore'
import { consumePolicyRotation } from '../scenarioFlags'
import { extractPathParam, requireActor, requireRole } from './index'

export const getActiveCourseLlmPolicy: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const courseId = extractPathParam(req.url, /^\/api\/v1\/courses\/([^/]+)\/llm-egress-policies\/active$/, 1)
  if (!courseId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的课程 ID', false)

  const roleCheck = requireRole(actorResult, 'llm_policy:read', { courseId })
  if (roleCheck !== true) return roleCheck

  // Deterministic conflict scenario: rotate the policy revision once when the
  // localStorage flag is set, so the next write sees a revision mismatch.
  if (consumePolicyRotation()) {
    const rotated = rotatePolicy(courseId)
    if (rotated) return { status: 200, data: rotated }
  }

  const policy = getActivePolicy(courseId)
  if (!policy) return problem(404, 'LLM_POLICY_NOT_FOUND', `课程 ${courseId} 没有生效中的 LLM 出站策略`, false)
  return { status: 200, data: policy }
}
