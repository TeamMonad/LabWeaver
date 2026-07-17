import type { CompleteProblemPackageUploadRequestSchema, CreateProblemPackageUploadRequestSchema } from '@/generated/contracts'
import { problem } from '../diagnostics'
import type { FixtureHandler } from '../types'
import { completeUploadSession, createUploadSession } from '../stores/problemPackageStore'
import { extractPathParam, parseIfMatchRevision, requireActor, requireIdempotencyKey, requireIfMatch, requireRole } from './index'

export const createProblemPackageUpload: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const courseId = extractPathParam(req.url, /^\/api\/v1\/courses\/([^/]+)\/problem-package-uploads$/, 1)
  if (!courseId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的课程 ID', false)

  const roleCheck = requireRole(actorResult, 'problem_package:write', { courseId })
  if (roleCheck !== true) return roleCheck

  const idempotencyResult = requireIdempotencyKey(req)
  if (typeof idempotencyResult !== 'string') return idempotencyResult

  const body = req.body as CreateProblemPackageUploadRequestSchema
  const result = createUploadSession(courseId, body, idempotencyResult)
  if (result.kind === 'invalid') return problem(422, 'UPLOAD_REQUEST_INVALID', result.detail, false)
  if (result.kind === 'policy-revision-mismatch') {
    return problem(409, 'POLICY_REVISION_MISMATCH', 'LLM 出站策略 revision 已变化，请刷新后重试', false)
  }
  return { status: 201, data: result.session }
}

export const completeProblemPackageUpload: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const match = /^\/api\/v1\/courses\/([^/]+)\/problem-package-uploads\/([^/]+)\/complete$/.exec(req.url)
  const courseId = match?.[1]
  const uploadId = match?.[2]
  if (!courseId || !uploadId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的上传会话路径', false)

  const roleCheck = requireRole(actorResult, 'problem_package:write', { courseId })
  if (roleCheck !== true) return roleCheck

  const idempotencyResult = requireIdempotencyKey(req)
  if (typeof idempotencyResult !== 'string') return idempotencyResult
  const ifMatchResult = requireIfMatch(req)
  if (typeof ifMatchResult !== 'string') return ifMatchResult
  const expectedRevision = parseIfMatchRevision(ifMatchResult)
  if (expectedRevision === null) {
    return problem(412, 'PRECONDITION_FAILED', 'If-Match header 不是有效的强 ETag revision', false)
  }

  const body = req.body as CompleteProblemPackageUploadRequestSchema
  const result = completeUploadSession(courseId, uploadId, expectedRevision, body?.manifestSha256, idempotencyResult)
  if (result.kind === 'not-found') return problem(404, 'UPLOAD_SESSION_NOT_FOUND', `未找到上传会话 ${uploadId}`, false)
  if (result.kind === 'revision-mismatch') return problem(412, 'PRECONDITION_FAILED', 'If-Match revision 不匹配', false)
  if (result.kind === 'invalid') return problem(422, 'UPLOAD_REQUEST_INVALID', result.detail, false)
  return { status: 200, data: result.package }
}
