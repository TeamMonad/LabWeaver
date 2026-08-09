import type {
  CreateEvaluationReleaseRequestSchema,
  WithdrawEvaluationReleaseRequestSchema,
} from '@/generated/contracts'
import { conflict, preconditionFailed, problem } from '../diagnostics'
import { evaluationResultsScenario } from '../scenarioFlags'
import { getLatestApproval } from '../stores/approvalStore'
import {
  createEvaluationRelease,
  getEvaluationRelease,
  getStudentResult,
  listEvaluationReleases,
  listStudentResults,
  withdrawEvaluationRelease,
} from '../stores/evaluationStore'
import {
  parseIfMatchRevision,
  requireActor,
  requireIdempotencyKey,
  requireIfMatch,
  requireRole,
} from './index'
import type { FixtureHandler, FixtureRequest } from '../types'

function releasePath(url: string): { courseId: string; releaseId?: string; withdraw: boolean } | null {
  const pathname = url.split('?')[0]
  const match = /^\/api\/v1\/courses\/([^/]+)\/evaluation-releases(?:\/([^/]+)(\/withdraw)?)?$/.exec(pathname)
  if (!match) return null
  return { courseId: match[1], releaseId: match[2], withdraw: match[3] === '/withdraw' }
}

function resultPath(url: string): { courseId: string; runId?: string } | null {
  const pathname = url.split('?')[0]
  const match = /^\/api\/v1\/courses\/([^/]+)\/me\/evaluation-results(?:\/([^/]+))?$/.exec(pathname)
  return match ? { courseId: match[1], runId: match[2] } : null
}

function exactObject(value: unknown, keys: string[]): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value)
    && Object.keys(value).sort().join(',') === [...keys].sort().join(',')
}

function publicationBody(req: FixtureRequest): CreateEvaluationReleaseRequestSchema | null {
  if (!exactObject(req.body, ['approvalId', 'candidateId', 'candidateRevision', 'evaluationSpecSha256'])) return null
  const body = req.body
  return typeof body.approvalId === 'string'
    && typeof body.candidateId === 'string'
    && Number.isSafeInteger(body.candidateRevision)
    && Number(body.candidateRevision) > 0
    && typeof body.evaluationSpecSha256 === 'string'
    && /^[0-9a-f]{64}$/.test(body.evaluationSpecSha256)
    ? body as unknown as CreateEvaluationReleaseRequestSchema
    : null
}

function withdrawalBody(req: FixtureRequest): WithdrawEvaluationReleaseRequestSchema | null {
  if (!exactObject(req.body, ['expectedRevision', 'reasonCode'])) return null
  const body = req.body
  return Number.isSafeInteger(body.expectedRevision)
    && Number(body.expectedRevision) > 0
    && typeof body.reasonCode === 'string'
    && /^LW_[A-Z0-9_]{1,92}$/.test(body.reasonCode)
    ? body as unknown as WithdrawEvaluationReleaseRequestSchema
    : null
}

function pageQuery(url: string): { cursor?: string; limit: number } | null {
  const query = new URL(url, 'https://fixture.invalid').searchParams
  const cursor = query.get('cursor') ?? undefined
  const rawLimit = query.get('limit')
  const limit = rawLimit === null ? 50 : Number(rawLimit)
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > 100) return null
  return { cursor, limit }
}

export const listReleases: FixtureHandler = (req) => {
  const actor = requireActor(req)
  if (!('role' in actor)) return actor
  const path = releasePath(req.url)
  if (!path || path.releaseId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的 EvaluationRelease 列表路径')
  const allowed = requireRole(actor, 'evaluation_release:read', { courseId: path.courseId })
  if (allowed !== true) return allowed
  const query = pageQuery(req.url)
  if (!query) return problem(400, 'LW_CONTRACT_DOCUMENT_INVALID', '分页参数无效')
  const page = listEvaluationReleases(path.courseId, query.cursor, query.limit)
  return page === 'invalid-cursor'
    ? problem(400, 'LW_CONTRACT_DOCUMENT_INVALID', '分页 cursor 不属于当前课程')
    : { status: 200, data: page }
}

export const createRelease: FixtureHandler = (req) => {
  const actor = requireActor(req)
  if (!('role' in actor)) return actor
  const path = releasePath(req.url)
  if (!path || path.releaseId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的 EvaluationRelease 发布路径')
  const allowed = requireRole(actor, 'evaluation_release:publish', { courseId: path.courseId })
  if (allowed !== true) return allowed
  const key = requireIdempotencyKey(req)
  if (typeof key !== 'string') return key
  const body = publicationBody(req)
  if (!body) return problem(422, 'LW_CONTRACT_DOCUMENT_INVALID', '发布请求字段无效')
  const approval = getLatestApproval(body.candidateId)
  if (!approval || approval.id !== body.approvalId) {
    return preconditionFailed('审批不存在、已失效或不属于该候选')
  }
  const result = createEvaluationRelease(path.courseId, body, approval, actor.actorId, key)
  if (result.kind === 'conflict') return conflict('Idempotency-Key 已绑定不同发布 payload')
  if (result.kind === 'not-found') return problem(404, 'LW_EVALUATION_RELEASE_NOT_FOUND', '发布记录不存在')
  if (result.kind === 'precondition') return preconditionFailed('候选 revision、hash 或审批状态已变化')
  return { status: 201, data: result.release, headers: { ETag: `"rev-${result.release.revision}"` } }
}

export const getRelease: FixtureHandler = (req) => {
  const actor = requireActor(req)
  if (!('role' in actor)) return actor
  const path = releasePath(req.url)
  if (!path?.releaseId || path.withdraw) return problem(400, 'FIXTURE_INVALID_PATH', '无效的 EvaluationRelease 详情路径')
  const allowed = requireRole(actor, 'evaluation_release:read', { courseId: path.courseId })
  if (allowed !== true) return allowed
  const release = getEvaluationRelease(path.courseId, path.releaseId)
  return release
    ? { status: 200, data: release, headers: { ETag: `"rev-${release.revision}"` } }
    : problem(404, 'LW_EVALUATION_RELEASE_NOT_FOUND', 'EvaluationRelease 不存在')
}

export const withdrawRelease: FixtureHandler = (req) => {
  const actor = requireActor(req)
  if (!('role' in actor)) return actor
  const path = releasePath(req.url)
  if (!path?.releaseId || !path.withdraw) return problem(400, 'FIXTURE_INVALID_PATH', '无效的 EvaluationRelease 撤回路径')
  const allowed = requireRole(actor, 'evaluation_release:withdraw', { courseId: path.courseId })
  if (allowed !== true) return allowed
  const key = requireIdempotencyKey(req)
  if (typeof key !== 'string') return key
  const match = requireIfMatch(req)
  if (typeof match !== 'string') return match
  const revision = parseIfMatchRevision(match)
  const body = withdrawalBody(req)
  if (!revision || !body || revision !== body.expectedRevision) {
    return preconditionFailed('If-Match 与 expectedRevision 不一致')
  }
  const result = withdrawEvaluationRelease(
    path.courseId,
    path.releaseId,
    revision,
    body.reasonCode,
    key,
  )
  if (result.kind === 'conflict') return conflict('Idempotency-Key 已绑定不同撤回 payload')
  if (result.kind === 'not-found') return problem(404, 'LW_EVALUATION_RELEASE_NOT_FOUND', 'EvaluationRelease 不存在')
  if (result.kind === 'precondition') return preconditionFailed('Release revision 已变化或已撤回')
  return { status: 200, data: result.release, headers: { ETag: `"rev-${result.release.revision}"` } }
}

export const listResults: FixtureHandler = (req) => {
  const actor = requireActor(req)
  if (!('role' in actor)) return actor
  const path = resultPath(req.url)
  if (!path || path.runId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的评测结果列表路径')
  const allowed = requireRole(actor, 'evaluation_result:read_own', { courseId: path.courseId, actorId: actor.actorId })
  if (allowed !== true) return allowed
  if (evaluationResultsScenario() === 'error') {
    return problem(503, 'LW_EVALUATION_UNAVAILABLE', '评测结果服务暂不可用', true)
  }
  if (evaluationResultsScenario() === 'empty') return { status: 200, data: { items: [], nextCursor: null } }
  const query = pageQuery(req.url)
  if (!query) return problem(400, 'LW_CONTRACT_DOCUMENT_INVALID', '分页参数无效')
  const page = listStudentResults(path.courseId, actor.actorId, query.cursor, query.limit)
  return page === 'invalid-cursor'
    ? problem(400, 'LW_CONTRACT_DOCUMENT_INVALID', '分页 cursor 不属于当前学生')
    : { status: 200, data: page }
}

export const getResult: FixtureHandler = (req) => {
  const actor = requireActor(req)
  if (!('role' in actor)) return actor
  const path = resultPath(req.url)
  if (!path?.runId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的评测结果详情路径')
  const allowed = requireRole(actor, 'evaluation_result:read_own', { courseId: path.courseId, actorId: actor.actorId })
  if (allowed !== true) return allowed
  if (evaluationResultsScenario() === 'error') {
    return problem(503, 'LW_EVALUATION_UNAVAILABLE', '评测结果服务暂不可用', true)
  }
  const result = getStudentResult(path.courseId, actor.actorId, path.runId)
  return result
    ? { status: 200, data: result }
    : problem(404, 'LW_EVALUATION_RUN_NOT_FOUND', '终态评测结果不存在或不属于当前学生')
}
