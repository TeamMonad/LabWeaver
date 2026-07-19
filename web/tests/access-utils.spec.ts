import { describe, expect, it } from 'vitest'
import { buildSshCommand, formatExpiry, resolveConnectUrl } from '@/types/access'
import type { AccessGrantWithGateway } from '@/types/access'
import type { EndpointGrant } from '@/generated/contracts'

function makeGrant(overrides: Partial<AccessGrantWithGateway> = {}): AccessGrantWithGateway {
  return {
    id: 'grant-1',
    environmentId: 'env-1',
    courseId: 'course-1',
    actorId: 'student',
    environmentRevision: 1,
    state: 'active',
    issuedAt: '2026-07-16T08:00:00.000Z',
    expiresAt: '2026-07-16T09:00:00.000Z',
    revokedAt: null,
    reasonCode: null,
    revision: 1,
    endpointGrants: [],
    gatewayHostname: 'gateway.labweaver.local',
    gatewayFingerprintSha256: 'sha256:' + '9'.repeat(64),
    ...overrides,
  }
}

function makeEndpointGrant(overrides: Partial<EndpointGrant> = {}): EndpointGrant {
  return {
    id: 'eg-1',
    accessGrantId: 'grant-1',
    endpointId: 'ep-1',
    endpointRevision: 1,
    action: 'connect',
    protocol: 'ssh',
    expiresAt: '2026-07-16T09:00:00.000Z',
    health: 'healthy',
    alias: 'lw-0123456789abcdef0123',
    ...overrides,
  }
}

describe('buildSshCommand', () => {
  it('builds the single-line command', () => {
    const command = buildSshCommand(makeGrant(), makeEndpointGrant())
    expect(command).toBe('ssh lw-0123456789abcdef0123@gateway.labweaver.local')
  })

  it('returns null when alias is missing', () => {
    expect(buildSshCommand(makeGrant(), makeEndpointGrant({ alias: null }))).toBeNull()
  })

  it('returns null when gateway hostname is missing', () => {
    expect(buildSshCommand(makeGrant({ gatewayHostname: undefined }), makeEndpointGrant())).toBeNull()
  })
})

describe('resolveConnectUrl', () => {
  it('returns the connect url when present', () => {
    expect(resolveConnectUrl(makeEndpointGrant({ connectUrl: '/connect/eg-1/' }))).toBe('/connect/eg-1/')
  })

  it('returns null when absent', () => {
    expect(resolveConnectUrl(makeEndpointGrant())).toBeNull()
  })
})

describe('formatExpiry', () => {
  const now = new Date('2026-07-16T08:00:00.000Z')

  it('reports minutes', () => {
    expect(formatExpiry('2026-07-16T08:58:00.000Z', now)).toBe('58 分钟后过期')
  })

  it('reports hours', () => {
    expect(formatExpiry('2026-07-16T20:00:00.000Z', now)).toBe('12 小时后过期')
  })

  it('reports days', () => {
    expect(formatExpiry('2026-07-18T08:00:00.000Z', now)).toBe('2 天后过期')
  })

  it('reports expired', () => {
    expect(formatExpiry('2026-07-16T07:00:00.000Z', now)).toBe('已过期')
  })
})
