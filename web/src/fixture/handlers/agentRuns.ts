import type { CreateAgentRunRequestSchema } from '@/generated/contracts'
import { problem } from '../diagnostics'
import type { FixtureHandler } from '../types'
import * as agentRunStore from '../stores/agentRunStore'
import { consumeAgentRunPollFailure } from '../scenarioFlags'
import { extractPathParam, parseIfMatchRevision, requireActor, requireIdempotencyKey, requireIfMatch, requireRole } from './index'

export const createAgentRun: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const courseId = extractPathParam(req.url, /^\/api\/v1\/courses\/([^/]+)\/agent-runs$/, 1)
  if (!courseId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的课程 ID', false)

  const roleCheck = requireRole(actorResult, 'agent_run:write', { courseId })
  if (roleCheck !== true) return roleCheck

  const idempotencyResult = requireIdempotencyKey(req)
  if (typeof idempotencyResult !== 'string') return idempotencyResult

  const body = req.body as CreateAgentRunRequestSchema
  const result = agentRunStore.createAgentRun(courseId, body, idempotencyResult)
  if (result.kind === 'package-not-found') {
    return problem(404, 'PROBLEM_PACKAGE_NOT_FOUND', '未找到材料包，请先完成上传归档', false)
  }
  if (result.kind === 'revision-mismatch') return problem(409, 'REVISION_MISMATCH', result.detail, false)
  return { status: 201, data: result.run }
}

export const getAgentRun: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const match = /^\/api\/v1\/courses\/([^/]+)\/agent-runs\/([^/]+)$/.exec(req.url)
  const courseId = match?.[1]
  const runId = match?.[2]
  if (!courseId || !runId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的 AgentRun 路径', false)

  const roleCheck = requireRole(actorResult, 'agent_run:read', { courseId })
  if (roleCheck !== true) return roleCheck

  // Deterministic transient failure scenario for poll-gap recovery demos.
  if (consumeAgentRunPollFailure()) {
    return problem(500, 'AGENT_RUN_POLL_TRANSIENT', 'AgentRun 状态刷新失败（fixture 模拟瞬时故障）', true)
  }

  const run = agentRunStore.getAgentRun(courseId, runId)
  if (!run) return problem(404, 'AGENT_RUN_NOT_FOUND', `未找到 AgentRun ${runId}`, false)
  return { status: 200, data: run }
}

export const cancelAgentRun: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const match = /^\/api\/v1\/courses\/([^/]+)\/agent-runs\/([^/]+)\/cancel$/.exec(req.url)
  const courseId = match?.[1]
  const runId = match?.[2]
  if (!courseId || !runId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的 AgentRun 路径', false)

  const roleCheck = requireRole(actorResult, 'agent_run:write', { courseId })
  if (roleCheck !== true) return roleCheck

  const idempotencyResult = requireIdempotencyKey(req)
  if (typeof idempotencyResult !== 'string') return idempotencyResult
  const ifMatchResult = requireIfMatch(req)
  if (typeof ifMatchResult !== 'string') return ifMatchResult
  const expectedRevision = parseIfMatchRevision(ifMatchResult)
  if (expectedRevision === null) {
    return problem(412, 'PRECONDITION_FAILED', 'If-Match header 不是有效的强 ETag revision', false)
  }

  const result = agentRunStore.cancelAgentRun(courseId, runId, expectedRevision)
  if (!result) return problem(404, 'AGENT_RUN_NOT_FOUND', `未找到 AgentRun ${runId}`, false)
  if (result === 'revision-mismatch') return problem(412, 'PRECONDITION_FAILED', 'If-Match revision 不匹配', false)
  if (result === 'not-cancellable') return problem(409, 'AGENT_RUN_NOT_CANCELLABLE', '当前状态的 AgentRun 不可取消', false)
  return { status: 200, data: result }
}

export const retryAgentRunTrack: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const match = /^\/api\/v1\/courses\/([^/]+)\/agent-runs\/([^/]+)\/tracks\/(environment|evaluation)\/retry$/.exec(req.url)
  const courseId = match?.[1]
  const runId = match?.[2]
  const track = match?.[3] as 'environment' | 'evaluation' | undefined
  if (!courseId || !runId || !track) return problem(400, 'FIXTURE_INVALID_PATH', '无效的 AgentRun 轨道路径', false)

  const roleCheck = requireRole(actorResult, 'agent_run:write', { courseId })
  if (roleCheck !== true) return roleCheck

  const idempotencyResult = requireIdempotencyKey(req)
  if (typeof idempotencyResult !== 'string') return idempotencyResult
  const ifMatchResult = requireIfMatch(req)
  if (typeof ifMatchResult !== 'string') return ifMatchResult
  const expectedRevision = parseIfMatchRevision(ifMatchResult)
  if (expectedRevision === null) {
    return problem(412, 'PRECONDITION_FAILED', 'If-Match header 不是有效的强 ETag revision', false)
  }

  const result = agentRunStore.retryAgentRunTrack(courseId, runId, track, expectedRevision)
  if (!result) return problem(404, 'AGENT_RUN_NOT_FOUND', `未找到 AgentRun ${runId}`, false)
  if (result === 'revision-mismatch') return problem(412, 'PRECONDITION_FAILED', 'If-Match revision 不匹配', false)
  if (result === 'not-retryable') return problem(409, 'AGENT_RUN_NOT_RETRYABLE', '当前状态的 AgentRun 轨道不可重试', false)
  return { status: 200, data: result }
}
