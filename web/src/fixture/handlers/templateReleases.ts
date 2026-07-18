import type { CreateEnvironmentTemplateReleaseRequestSchema, EnvironmentTemplateReleaseViewSchema } from '@/generated/contracts'
import { conflict, problem } from '../diagnostics'
import { getEnvironmentCandidate } from '../stores/candidateStore'
import { getLatestApproval } from '../stores/approvalStore'
import { listTemplateReleases, publishRelease } from '../stores/templateReleaseStore'
import { nowIso } from '../utils/clock'
import { nextUuid7 } from '../utils/identity'
import { nextRevision } from '../utils/sequence'
import { extractPathParam, requireActor, requireIdempotencyKey, requireRole } from './index'
import type { FixtureHandler } from '../types'

function parseCourseId(url: string): string | null {
  return extractPathParam(url, /^\/api\/v1\/courses\/([^/]+)\/environment-template-releases$/, 1)
}

export const listEnvironmentTemplateReleases: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const courseId = parseCourseId(req.url)
  if (!courseId) {
    return {
      status: 400,
      data: {
        type: 'about:blank',
        title: 'Bad Request',
        status: 400,
        detail: '无效的课程 ID',
        instance: req.url,
        diagnosticCode: 'FIXTURE_INVALID_PATH',
        requestId: 'fixture-request',
        retryable: false,
      },
    }
  }

  const items = listTemplateReleases(courseId).filter((release) =>
    actorResult.courseIds.includes(release.courseId),
  )
  const page = {
    items: items as EnvironmentTemplateReleaseViewSchema[],
    nextCursor: null,
  }
  return { status: 200, data: page }
}

function parseReleaseBody(req: unknown): CreateEnvironmentTemplateReleaseRequestSchema | null {
  const body = req as CreateEnvironmentTemplateReleaseRequestSchema | undefined
  if (!body || typeof body !== 'object') return null
  if (typeof body.approvalId !== 'string') return null
  if (!body.artifact || typeof body.artifact !== 'object') return null
  if (typeof body.candidateId !== 'string') return null
  if (typeof body.candidateRevision !== 'number') return null
  if (typeof body.environmentSpecSha256 !== 'string') return null
  if (!body.imagePolicyEvaluation || typeof body.imagePolicyEvaluation !== 'object') return null
  if (typeof body.runtimeKind !== 'string') return null
  return body
}

export const createEnvironmentTemplateRelease: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const courseId = parseCourseId(req.url)
  if (!courseId) return problem(400, 'FIXTURE_INVALID_PATH', '无效的课程 ID', false)
  const roleCheck = requireRole(actorResult, 'release:publish', { courseId })
  if (roleCheck !== true) return roleCheck

  const idempotencyResult = requireIdempotencyKey(req)
  if (typeof idempotencyResult !== 'string') return idempotencyResult

  const body = parseReleaseBody(req.body)
  if (!body) return problem(422, 'UPLOAD_REQUEST_INVALID', '无效的发布请求体', false)

  const stored = getEnvironmentCandidate(body.candidateId)
  if (!stored) return problem(404, 'CANDIDATE_NOT_FOUND', `未找到 EnvironmentCandidate ${body.candidateId}`, false)

  const candidate = stored.candidate
  if (
    candidate.revision !== body.candidateRevision ||
    candidate.specSha256 !== body.environmentSpecSha256
  ) {
    return conflict('候选 revision 或 spec hash 已变化，请刷新后重试')
  }

  const approval = getLatestApproval(body.candidateId)
  if (!approval || approval.id !== body.approvalId || approval.decision !== 'approved') {
    return conflict('未找到已批准的候选审批')
  }
  if (
    approval.candidateRevision !== body.candidateRevision ||
    approval.candidateSha256 !== body.environmentSpecSha256
  ) {
    return conflict('审批与候选不匹配')
  }

  const evaluation = body.imagePolicyEvaluation
  const artifact = body.artifact
  const signature = artifact.signature
  if (!signature) {
    return conflict('镜像缺少 Sigstore 签名证据，禁止发布')
  }
  if (
    evaluation.expectedFulcioIssuer !== signature.fulcioIssuer ||
    evaluation.expectedCertificateSubject !== signature.certificateSubject
  ) {
    return conflict('签名 issuer 或 subject 与策略不符，禁止发布')
  }
  if (artifact.kind === 'container' && artifact.digest !== evaluation.artifactSha256) {
    return conflict('镜像 digest 与扫描结果不匹配，禁止发布')
  }
  if (evaluation.vulnerabilities.critical > 0) {
    return conflict('镜像存在 Critical 漏洞，禁止发布')
  }

  const release: EnvironmentTemplateReleaseViewSchema = {
    id: nextUuid7('release'),
    courseId,
    candidateId: body.candidateId,
    candidateRevision: body.candidateRevision,
    version: nextRevision(),
    runtimeKind: body.runtimeKind,
    environmentSpecSha256: body.environmentSpecSha256,
    publishedAt: nowIso(),
    publishedBy: actorResult.actorId,
    artifact,
    approval,
    imagePolicyEvaluation: evaluation,
    withdrawal: null,
  }

  publishRelease(release)
  return { status: 202, data: { operationId: nextUuid7('operation'), revision: release.version, statusUrl: `/api/v1/courses/${courseId}/environment-template-releases/${release.id}` } }
}
