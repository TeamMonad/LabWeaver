import { describe, expect, it } from 'vitest'
import type { AccessGrantSchema, EnvironmentEndpointSchema } from '@/generated/contracts'
import type { AsyncState } from '@/types/async'
import { makeDiagnostic } from '@/types/async'
import { connectionMethods, connectionReadiness, grantIsUsable } from '@/utils/environmentConnection'

const running = { desiredState: 'running', observedState: 'ready' } as const
const NOW = Date.parse('2026-07-19T09:30:00.000Z')

function endpoint(protocol: EnvironmentEndpointSchema['protocol'], health: EnvironmentEndpointSchema['health']): EnvironmentEndpointSchema {
  return { id: `${protocol}-${health}`, protocol, health, revision: 1, observedAt: '2026-07-11T10:00:00.000Z' }
}

function endpoints(...items: EnvironmentEndpointSchema[]): AsyncState<EnvironmentEndpointSchema[]> {
  return { kind: 'success', data: items }
}

function grant(state: AccessGrantSchema['state'], expiresAt = '2026-07-19T10:00:00.000Z'): AsyncState<AccessGrantSchema> {
  return { kind: 'success', data: { state, expiresAt } as AccessGrantSchema }
}

describe('connectionReadiness', () => {
  it('uses the console wording when the instance itself is not ready', () => {
    const result = connectionReadiness({ desiredState: 'running', observedState: 'provisioning' }, endpoints(endpoint('ssh', 'healthy')), grant('active'), '环境就绪后才能签发访问授权。', NOW)
    expect(result).toMatchObject({ kind: 'instance-not-ready', detail: '环境就绪后才能签发访问授权。' })
  })

  it('treats a stop request on a ready instance as not connectable', () => {
    expect(connectionReadiness({ desiredState: 'stopped', observedState: 'ready' }, endpoints(endpoint('ssh', 'healthy')), grant('active'), 'blocked', NOW).kind).toBe('instance-not-ready')
  })

  it.each([
    [{ kind: 'loading' } as const, 'checking'],
    [{ kind: 'idle' } as const, 'checking'],
    [{ kind: 'empty' } as const, 'no-endpoints'],
    [{ kind: 'error', diagnostic: makeDiagnostic('ENDPOINT_LIST_FAILED', '加载环境入口失败', true) } as const, 'unknown'],
  ])('reads endpoint state %o as %s', (state, kind) => {
    expect(connectionReadiness(running, state, { kind: 'idle' }, '', NOW).kind).toBe(kind)
  })

  it('keeps an unreadable endpoint list distinct from having no endpoints', () => {
    const result = connectionReadiness(running, { kind: 'error', diagnostic: makeDiagnostic('X', '网络中断', true) }, { kind: 'idle' }, '', NOW)
    expect(result.detail).toBe('无法读取连接入口：网络中断')
  })

  it('distinguishes pending, unhealthy and removed endpoints', () => {
    expect(connectionReadiness(running, endpoints(endpoint('ssh', 'pending'), endpoint('https', 'unhealthy')), { kind: 'idle' }, '', NOW).kind).toBe('endpoints-pending')
    expect(connectionReadiness(running, endpoints(endpoint('https', 'unhealthy')), { kind: 'idle' }, '', NOW).kind).toBe('endpoints-unhealthy')
    expect(connectionReadiness(running, endpoints(endpoint('https', 'removed')), { kind: 'idle' }, '', NOW).kind).toBe('no-endpoints')
  })

  it('only reports ready once a healthy endpoint has an unexpired active grant', () => {
    const healthy = endpoints(endpoint('https', 'healthy'), endpoint('ssh', 'pending'))
    expect(connectionReadiness(running, healthy, { kind: 'idle' }, '', NOW).kind).toBe('needs-grant')
    expect(connectionReadiness(running, healthy, { kind: 'loading' }, '', NOW).kind).toBe('checking')
    expect(connectionReadiness(running, healthy, grant('requested'), '', NOW).kind).toBe('grant-pending')
    expect(connectionReadiness(running, healthy, grant('expired'), '', NOW).kind).toBe('needs-grant')
    expect(connectionReadiness(running, healthy, grant('active'), '', NOW)).toMatchObject({ kind: 'ready', label: '可连接' })
  })

  it('fails closed once the grant passes its own expiry, before the server is read again', () => {
    const healthy = endpoints(endpoint('https', 'healthy'))
    const result = connectionReadiness(running, healthy, grant('active', '2026-07-19T09:29:59.000Z'), '', NOW)
    expect(result).toMatchObject({ kind: 'needs-regrant', label: '待重新授权' })
    expect(grantIsUsable(grant('active', '2026-07-19T09:29:59.000Z'), NOW)).toBe(false)
  })

  it('asks for a new grant when the current grant cannot be confirmed', () => {
    const result = connectionReadiness(
      running,
      endpoints(endpoint('https', 'healthy')),
      { kind: 'error', diagnostic: makeDiagnostic('ACCESS_GRANT_LIST_FAILED', '加载当前访问授权失败。', true) },
      '',
      NOW,
    )
    expect(result.kind).toBe('needs-regrant')
    expect(result.detail).toContain('加载当前访问授权失败。')
  })
})

describe('connectionMethods', () => {
  it('offers no browser console without server console capabilities, whatever the runtime', () => {
    expect(connectionMethods({ kind: 'empty' }, [])).toEqual([])
  })

  it('lists only SSH for a VM whose grant permits no desktop', () => {
    expect(connectionMethods(endpoints(endpoint('ssh', 'healthy')), []).map((method) => method.id)).toEqual(['ssh'])
  })

  it('lists the desktop for a VM only when the grant permits noVNC', () => {
    expect(connectionMethods(endpoints(endpoint('ssh', 'healthy')), ['novnc']).map((method) => method.id)).toEqual(['desktop', 'ssh'])
  })

  it('omits the terminal for a container whose grant permits no xterm', () => {
    expect(connectionMethods(endpoints(endpoint('https', 'healthy')), []).map((method) => method.id)).toEqual(['app'])
    expect(connectionMethods(endpoints(endpoint('https', 'healthy')), ['xterm']).map((method) => method.id)).toEqual(['console', 'app'])
  })

  it('skips removed endpoints', () => {
    expect(connectionMethods(endpoints(endpoint('ssh', 'pending'), endpoint('http', 'removed')), []).map((method) => method.id)).toEqual(['ssh'])
  })
})
