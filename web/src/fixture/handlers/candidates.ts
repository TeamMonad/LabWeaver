import type {
  CandidateDecisionRequestSchema,
  EnvironmentCandidateViewSchema,
  EvaluationCandidateViewSchema,
} from '@/generated/contracts'
import { conflict, problem } from '../diagnostics'
import { getEnvironmentCandidate, getEvaluationCandidate } from '../stores/candidateStore'
import { appendDecision, getApprovals, getLatestApproval } from '../stores/approvalStore'
import { consumeImageGateScenario } from '../scenarioFlags'
import { extractPathParam, requireActor, requireRole } from './index'
import type { FixtureHandler, FixtureRequest } from '../types'

function candidatePathParams(url: string): { courseId: string; candidateId: string } | null {
  const courseId = extractPathParam(url, /^\/api\/v1\/courses\/([^/]+)\/environment-candidates\/([^/]+)$/, 1)
  const candidateId = extractPathParam(url, /^\/api\/v1\/courses\/([^/]+)\/environment-candidates\/([^/]+)$/, 2)
  if (!courseId || !candidateId) return null
  return { courseId, candidateId }
}

function evaluationPathParams(url: string): { courseId: string; candidateId: string } | null {
  const courseId = extractPathParam(url, /^\/api\/v1\/courses\/([^/]+)\/evaluation-candidates\/([^/]+)$/, 1)
  const candidateId = extractPathParam(url, /^\/api\/v1\/courses\/([^/]+)\/evaluation-candidates\/([^/]+)$/, 2)
  if (!courseId || !candidateId) return null
  return { courseId, candidateId }
}

function decisionPathParams(url: string, kind: 'environment' | 'evaluation'): { courseId: string; candidateId: string } | null {
  const pattern = new RegExp(`^/api/v1/courses/([^/]+)/${kind}-candidates/([^/]+)/decisions$`)
  const courseId = extractPathParam(url, pattern, 1)
  const candidateId = extractPathParam(url, pattern, 2)
  if (!courseId || !candidateId) return null
  return { courseId, candidateId }
}

function parseDecisionBody(req: FixtureRequest): CandidateDecisionRequestSchema | null {
  const body = req.body as CandidateDecisionRequestSchema | undefined
  if (!body || typeof body !== 'object') return null
  if (typeof body.candidateRevision !== 'number') return null
  if (typeof body.candidateSha256 !== 'string') return null
  if (typeof body.decision !== 'string') return null
  if (typeof body.policyRevision !== 'number') return null
  if (typeof body.reason !== 'string') return null
  if (typeof body.schemaSha256 !== 'string') return null
  if (typeof body.trustRevision !== 'number') return null
  return body
}

export const getEnvironmentCandidateHandler: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const params = candidatePathParams(req.url)
  if (!params) return problem(400, 'FIXTURE_INVALID_PATH', '无效的 EnvironmentCandidate 路径', false)
  const roleCheck = requireRole(actorResult, 'candidate:read', { courseId: params.courseId })
  if (roleCheck !== true) return roleCheck

  const stored = getEnvironmentCandidate(params.candidateId)
  if (!stored) return problem(404, 'CANDIDATE_NOT_FOUND', `未找到 EnvironmentCandidate ${params.candidateId}`, false)

  const scenario = consumeImageGateScenario()
  const imageArtifact = scenario ? structuredClone(stored.imageArtifact) : stored.imageArtifact
  const imagePolicyEvaluation = scenario ? structuredClone(stored.imagePolicyEvaluation) : stored.imagePolicyEvaluation
  if (scenario) {
    switch (scenario) {
      case 'critical':
        imagePolicyEvaluation.vulnerabilities.critical = 1
        imagePolicyEvaluation.passed = false
        break
      case 'high':
        imagePolicyEvaluation.vulnerabilities.high = 1
        break
      case 'wrong-digest':
        if (imageArtifact.kind === 'container') {
          imageArtifact.digest = 'sha256:' + 'f'.repeat(64)
        }
        imagePolicyEvaluation.passed = false
        break
    }
  }

  return {
    status: 200,
    data: {
      approvals: getApprovals(params.candidateId),
      build: {
        artifact: imageArtifact,
        cleanupVerified: null,
        diagnosticCode: null,
        imagePolicyEvaluation,
        state: 'succeeded',
      },
      candidate: stored.candidate,
      trustRevision: stored.trustRevision,
    } satisfies EnvironmentCandidateViewSchema,
  }
}

export const appendEnvironmentCandidateDecision: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const params = decisionPathParams(req.url, 'environment')
  if (!params) return problem(400, 'FIXTURE_INVALID_PATH', '无效的 EnvironmentCandidate 决策路径', false)
  const roleCheck = requireRole(actorResult, 'candidate:approve', { courseId: params.courseId })
  if (roleCheck !== true) return roleCheck

  const body = parseDecisionBody(req)
  if (!body) return problem(422, 'UPLOAD_REQUEST_INVALID', '无效的决策请求体', false)

  const stored = getEnvironmentCandidate(params.candidateId)
  if (!stored) return problem(404, 'CANDIDATE_NOT_FOUND', `未找到 EnvironmentCandidate ${params.candidateId}`, false)

  const candidate = stored.candidate
  if (
    candidate.revision !== body.candidateRevision ||
    candidate.specSha256 !== body.candidateSha256 ||
    candidate.policyRevision !== body.policyRevision ||
    candidate.schemaSha256 !== body.schemaSha256 ||
    stored.trustRevision !== body.trustRevision
  ) {
    return conflict('候选 revision、hash、policy 或 trust 已变化，请刷新后重试')
  }

  const latest = getLatestApproval(params.candidateId)
  if (latest && latest.candidateRevision === body.candidateRevision && latest.decision === body.decision) {
    return conflict('相同 revision 的重复决策')
  }

  const approval = appendDecision(params.candidateId, body, actorResult.actorId)
  return { status: 201, data: approval }
}

export const getEvaluationCandidateHandler: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const params = evaluationPathParams(req.url)
  if (!params) return problem(400, 'FIXTURE_INVALID_PATH', '无效的 EvaluationCandidate 路径', false)
  const roleCheck = requireRole(actorResult, 'candidate:read', { courseId: params.courseId })
  if (roleCheck !== true) return roleCheck

  const stored = getEvaluationCandidate(params.candidateId)
  if (!stored) return problem(404, 'CANDIDATE_NOT_FOUND', `未找到 EvaluationCandidate ${params.candidateId}`, false)
  return {
    status: 200,
    data: {
      approvals: getApprovals(params.candidateId),
      candidate: stored.candidate,
      trustRevision: stored.trustRevision,
    } satisfies EvaluationCandidateViewSchema,
  }
}

export const appendEvaluationCandidateDecision: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const params = decisionPathParams(req.url, 'evaluation')
  if (!params) return problem(400, 'FIXTURE_INVALID_PATH', '无效的 EvaluationCandidate 决策路径', false)
  const roleCheck = requireRole(actorResult, 'candidate:approve', { courseId: params.courseId })
  if (roleCheck !== true) return roleCheck

  const body = parseDecisionBody(req)
  if (!body) return problem(422, 'UPLOAD_REQUEST_INVALID', '无效的决策请求体', false)

  const stored = getEvaluationCandidate(params.candidateId)
  if (!stored) return problem(404, 'CANDIDATE_NOT_FOUND', `未找到 EvaluationCandidate ${params.candidateId}`, false)

  const candidate = stored.candidate
  if (
    candidate.revision !== body.candidateRevision ||
    candidate.specSha256 !== body.candidateSha256 ||
    candidate.policyRevision !== body.policyRevision ||
    candidate.schemaSha256 !== body.schemaSha256 ||
    stored.trustRevision !== body.trustRevision
  ) {
    return conflict('候选 revision、hash、policy 或 trust 已变化，请刷新后重试')
  }

  const latest = getLatestApproval(params.candidateId)
  if (latest && latest.candidateRevision === body.candidateRevision && latest.decision === body.decision) {
    return conflict('相同 revision 的重复决策')
  }

  const approval = appendDecision(params.candidateId, body, actorResult.actorId)
  return { status: 201, data: approval }
}
