import type { ConsoleKind, IssueConsoleCapabilityRequestSchema } from '@/generated/contracts'
import { conflict, problem } from '../diagnostics'
import { getAccessGrant } from '../stores/accessGrantStore'
import { getEnvironment } from '../stores/environmentStore'
import { availabilityFor, issueCapability } from '../stores/consoleCapabilityStore'
import { consumeImageGateScenario } from '../scenarioFlags'
import { extractPathParam, requireActor, requireIdempotencyKey, requireIfMatch, requireRole } from './index'
import type { FixtureHandler } from '../types'

function grantIdFrom(url: string): string | null {
  return extractPathParam(url, /^\/api\/v1\/access-grants\/([^/]+)\/console-capabilities$/, 1)
}

function resolveGrantAndEnvironment(req: Parameters<FixtureHandler>[0]) {
  const grantId = grantIdFrom(req.url)
  if (!grantId) return { error: problem(400, 'FIXTURE_INVALID_PATH', '无效的 AccessGrant 路径', false) }
  const grant = getAccessGrant(grantId)
  if (!grant) return { error: problem(404, 'ACCESS_GRANT_NOT_FOUND', `未找到授权 ${grantId}`, false) }
  const environment = getEnvironment(grant.environmentId)
  if (!environment) return { error: problem(404, 'ENVIRONMENT_NOT_FOUND', `未找到环境 ${grant.environmentId}`, false) }
  return { grant, environment }
}

function kindsFor(environment: { runtimeKind: 'container' | 'virtual_machine' }): ConsoleKind[] {
  return environment.runtimeKind === 'container' ? ['xterm'] : ['novnc']
}

export const listConsoleCapabilities: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const resolved = resolveGrantAndEnvironment(req)
  if ('error' in resolved) return resolved.error
  const { grant, environment } = resolved

  const roleCheck = requireRole(actorResult, 'console_capability:read', { courseId: environment.courseId })
  if (roleCheck !== true) return roleCheck
  if (grant.state !== 'active') return problem(409, 'ACCESS_GRANT_NOT_ACTIVE', '授权已撤销或过期，无法发现控制台能力', false)
  if (environment.observedState !== 'ready') return problem(409, 'ENVIRONMENT_NOT_READY', '环境未就绪，无法发现控制台能力', false)

  const availability = availabilityFor(
    grant.id,
    grant.revision,
    environment,
    kindsFor(environment),
    environment.class === 'work' && environment.leaseId
      ? { leaseId: environment.leaseId, leaseRevision: environment.revision, expiresAt: environment.eligibilityExpiresAt }
      : null,
  )
  return { status: 200, data: availability }
}

export const issueConsoleCapability: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const resolved = resolveGrantAndEnvironment(req)
  if ('error' in resolved) return resolved.error
  const { grant, environment } = resolved

  const roleCheck = requireRole(actorResult, 'console_capability:issue', { courseId: environment.courseId })
  if (roleCheck !== true) return roleCheck

  const idempotencyResult = requireIdempotencyKey(req)
  if (typeof idempotencyResult !== 'string') return idempotencyResult
  const ifMatchResult = requireIfMatch(req)
  if (typeof ifMatchResult !== 'string') return ifMatchResult

  const body = req.body as IssueConsoleCapabilityRequestSchema | undefined
  if (!body || typeof body.kind !== 'string') return problem(422, 'UPLOAD_REQUEST_INVALID', '无效的签发请求体', false)

  if (grant.state !== 'active') return problem(409, 'ACCESS_GRANT_NOT_ACTIVE', '授权已撤销或过期，无法签发控制台能力', false)
  if (environment.observedState !== 'ready') return problem(409, 'ENVIRONMENT_NOT_READY', '环境未就绪，无法签发控制台能力', false)
  if (body.expectedAccessGrantRevision !== grant.revision) return conflict('AccessGrant revision 已变化，请刷新后重试')
  if (body.expectedEnvironmentRevision !== environment.revision) return conflict('环境 revision 已变化，请刷新后重试')

  const eligible = kindsFor(environment)
  if (!eligible.includes(body.kind)) return problem(409, 'CONSOLE_KIND_NOT_ELIGIBLE', `当前环境不支持 ${body.kind} 控制台`, false)

  // Deterministic upstream scenario for E1/E2: `fixture:imageGate` value
  // `console-upstream` makes issuance report an unavailable upstream.
  if (consumeImageGateScenario() === 'console-upstream') {
    return problem(503, 'CONSOLE_UPSTREAM_UNAVAILABLE', '控制台上游不可用（fixture 确定性场景）', true)
  }

  const leaseFence =
    environment.class === 'work' && environment.leaseId
      ? { leaseId: environment.leaseId, leaseRevision: environment.revision, expiresAt: environment.eligibilityExpiresAt }
      : null

  const capability = issueCapability(
    grant.id,
    grant.revision,
    environment,
    body.kind as ConsoleKind,
    leaseFence,
  )
  return { status: 201, data: capability }
}
