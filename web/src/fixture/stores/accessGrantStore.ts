import type {
  AccessGrantSchema,
  AccessGrantSnapshot,
  CreateAccessGrantRequestSchema,
  EndpointGrantSnapshot,
} from '@/generated/contracts'
import { nowIso } from '../utils/clock'
import { addHoursIso } from '../utils/clock'
import { nextStreamSequence, nextUuid7 } from '../utils/identity'
import { nextRevision } from '../utils/sequence'
import { appendEvent } from './eventLog'
import { getEndpoint, listEndpoints } from './endpointStore'
import { getEnvironment } from './environmentStore'
import type { FixtureActor } from './actorStore'

const grants = new Map<string, AccessGrantSchema>()
const idempotencyMap = new Map<string, AccessGrantSchema>()

export function seedAccessGrants(environmentIds: string[]): void {
  grants.clear()
  idempotencyMap.clear()

  const activeGrant = createGrantInternal({
    id: nextUuid7('grant'),
    environmentId: environmentIds[0],
    courseId: 'course-101',
    actorId: 'fixture-actor-student',
    state: 'active',
    expiresAt: addHoursIso(24),
  })
  grants.set(activeGrant.id, activeGrant)

  const expiredGrant = createGrantInternal({
    id: nextUuid7('grant'),
    environmentId: environmentIds[1],
    courseId: 'course-101',
    actorId: 'fixture-actor-student',
    state: 'expired',
    expiresAt: nowIso(),
  })
  grants.set(expiredGrant.id, expiredGrant)

  const revokedGrant = createGrantInternal({
    id: nextUuid7('grant'),
    environmentId: environmentIds[2],
    courseId: 'course-101',
    actorId: 'fixture-actor-student',
    state: 'revoked',
    expiresAt: addHoursIso(24),
    revokedAt: nowIso(),
  })
  grants.set(revokedGrant.id, revokedGrant)
}

interface GrantSeedOptions {
  id: string
  environmentId: string
  courseId: string
  actorId: string
  state: AccessGrantSchema['state']
  expiresAt: string
  revokedAt?: string | null
}

function createGrantInternal(options: GrantSeedOptions): AccessGrantSchema {
  const env = getEnvironment(options.environmentId)
  const endpoints = listEndpoints(options.environmentId)
  const endpointGrants = endpoints.map((ep) => ({
    id: nextUuid7('epgrant'),
    accessGrantId: options.id,
    endpointId: ep.id,
    endpointRevision: ep.revision,
    action: 'connect' as const,
    protocol: ep.protocol,
    expiresAt: options.expiresAt,
    health: ep.health,
    alias: null,
  }))

  return {
    id: options.id,
    environmentId: options.environmentId,
    courseId: options.courseId,
    actorId: options.actorId,
    environmentRevision: env?.revision ?? 1,
    state: options.state,
    issuedAt: nowIso(),
    expiresAt: options.expiresAt,
    revokedAt: options.revokedAt ?? null,
    reasonCode: null,
    revision: nextRevision(),
    endpointGrants,
  }
}

export function createAccessGrant(
  request: CreateAccessGrantRequestSchema,
  actor: FixtureActor,
  idempotencyKey: string,
): AccessGrantSchema | 'revision-mismatch' | 'endpoint-missing' | 'conflict' {
  const cached = idempotencyMap.get(idempotencyKey)
  if (cached) return cached

  const env = getEnvironment(request.environmentId)
  if (!env) return 'endpoint-missing'
  if (env.revision !== request.environmentRevision) return 'revision-mismatch'

  const endpoints = request.endpointIds
    .map((id) => getEndpoint(request.environmentId, id))
    .filter((ep): ep is NonNullable<typeof ep> => ep !== undefined)

  if (endpoints.length !== request.endpointIds.length) return 'endpoint-missing'

  const id = nextUuid7('grant')
  // 授权有效期不得超过环境资格截止时间（AccessGrant fail-closed 语义）。
  // fixture 环境资格时间基于固定时钟，clamp 后授权输出保持确定性。
  const expiresAt =
    request.expiresAt <= env.eligibilityExpiresAt ? request.expiresAt : env.eligibilityExpiresAt
  const endpointGrants: AccessGrantSchema['endpointGrants'] = endpoints.map((ep) => ({
    id: nextUuid7('epgrant'),
    accessGrantId: id,
    endpointId: ep.id,
    endpointRevision: ep.revision,
    action: 'connect',
    protocol: ep.protocol,
    expiresAt,
    health: ep.health,
    alias: null,
  }))

  const grant: AccessGrantSchema = {
    id,
    environmentId: request.environmentId,
    courseId: env.courseId,
    actorId: actor.actorId,
    environmentRevision: env.revision,
    state: 'active',
    issuedAt: nowIso(),
    expiresAt,
    revokedAt: null,
    reasonCode: null,
    revision: nextRevision(),
    endpointGrants,
  }

  grants.set(id, grant)
  idempotencyMap.set(idempotencyKey, grant)

  appendEvent({
    courseId: env.courseId,
    projectId: null,
    streamSequence: nextStreamSequence(),
    eventId: nextUuid7('evt'),
    effectiveAt: nowIso(),
    data: {
      kind: 'access_grant_changed',
      environmentId: grant.environmentId,
      accessGrantId: grant.id,
      state: grant.state,
      revision: grant.revision,
    },
  })

  return grant
}

export function getAccessGrant(grantId: string): AccessGrantSchema | undefined {
  return grants.get(grantId)
}

export function listEnvironmentAccessGrants(environmentId: string): AccessGrantSchema[] {
  return Array.from(grants.values()).filter((g) => g.environmentId === environmentId)
}

export function revokeAccessGrant(grantId: string, reasonCode: string): AccessGrantSchema | undefined {
  const grant = grants.get(grantId)
  if (!grant || grant.state !== 'active') return undefined
  grant.state = 'revoked'
  grant.revokedAt = nowIso()
  grant.reasonCode = reasonCode
  grant.revision = nextRevision()

  const env = getEnvironment(grant.environmentId)
  if (env) {
    appendEvent({
      courseId: env.courseId,
      projectId: null,
      streamSequence: nextStreamSequence(),
      eventId: nextUuid7('evt'),
      effectiveAt: nowIso(),
      data: {
        kind: 'access_grant_changed',
        environmentId: grant.environmentId,
        accessGrantId: grant.id,
        state: grant.state,
        revision: grant.revision,
      },
    })
  }

  return grant
}

export function toAccessGrantSnapshot(grant: AccessGrantSchema): AccessGrantSnapshot {
  const endpointSnapshots: EndpointGrantSnapshot[] = grant.endpointGrants.map((ep) => ({
    id: ep.id,
    alias: ep.alias,
    endpointId: ep.endpointId,
    endpointRevision: ep.endpointRevision,
    action: ep.action,
    protocol: ep.protocol,
    expiresAt: ep.expiresAt,
    health: ep.health,
  }))

  return {
    id: grant.id,
    environmentId: grant.environmentId,
    environmentRevision: grant.environmentRevision,
    state: grant.state,
    issuedAt: grant.issuedAt,
    expiresAt: grant.expiresAt,
    revokedAt: grant.revokedAt,
    reasonCode: grant.reasonCode,
    revision: grant.revision,
    lastChangedStreamSequence: nextStreamSequence(),
    endpointGrants: endpointSnapshots,
    decision: {
      decision: grant.state === 'active' ? 'allow' : 'deny',
      evaluatedAt: nowIso(),
      reasonCode: grant.state === 'revoked' ? 'REVOKED' : grant.state === 'expired' ? 'EXPIRED' : 'ACTIVE',
    },
  }
}

export function resetAccessGrantStore(): void {
  grants.clear()
  idempotencyMap.clear()
}
