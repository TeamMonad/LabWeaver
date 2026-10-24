import { describe, expect, it } from 'vitest'
import type { AccessGrantSchema, EnvironmentConnectionReadiness, EnvironmentEndpointSchema } from '@/generated/contracts'
import type { AsyncState } from '@/types/async'
import { makeDiagnostic } from '@/types/async'
import { connectionMethods, connectionReadiness, grantIsUsable, type ConnectionFacts } from '@/utils/environmentConnection'

const NOW = Date.parse('2026-07-19T09:30:00.000Z')

function facts(connection: Partial<EnvironmentConnectionReadiness> = {}, desiredState: ConnectionFacts['desiredState'] = 'running'): ConnectionFacts {
  return { desiredState, connection: { instanceReady: true, state: 'ready', guestInstructions: [], ...connection } }
}

const running = facts()

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
  it('uses the console wording when the server reports the instance not ready', () => {
    const result = connectionReadiness(facts({ instanceReady: false, state: 'preparing' }), grant('active'), '环境就绪后才能签发访问授权。', NOW)
    expect(result).toMatchObject({ kind: 'instance-not-ready', detail: '环境就绪后才能签发访问授权。' })
  })

  it('treats a stop request on a ready instance as not connectable', () => {
    expect(connectionReadiness(facts({}, 'stopped'), grant('active'), 'blocked', NOW).kind).toBe('instance-not-ready')
  })

  it.each([
    ['preparing', 'connection-preparing', '连接准备中'],
    ['unknown', 'unknown', '无法确认'],
    ['unavailable', 'connection-unavailable', '入口不可用'],
  ] as const)('keeps a running instance with connection %s apart from a connectable one', (state, kind, label) => {
    // An active grant does not make the connection ready: only the server's connection state does.
    expect(connectionReadiness(facts({ state }), grant('active'), '', NOW)).toMatchObject({ kind, label })
  })

  it('explains an unconfirmed VM SSH handshake instead of claiming the entry is healthy', () => {
    expect(connectionReadiness(facts({ state: 'unknown' }), { kind: 'idle' }, '', NOW).detail).toContain('SSH 还没有完成握手')
  })

  it('only reports ready once the connection is ready and an unexpired active grant exists', () => {
    expect(connectionReadiness(running, { kind: 'idle' }, '', NOW).kind).toBe('needs-grant')
    expect(connectionReadiness(running, { kind: 'loading' }, '', NOW).kind).toBe('checking')
    expect(connectionReadiness(running, grant('requested'), '', NOW).kind).toBe('grant-pending')
    expect(connectionReadiness(running, grant('expired'), '', NOW).kind).toBe('needs-grant')
    expect(connectionReadiness(running, grant('active'), '', NOW)).toMatchObject({ kind: 'ready', label: '可连接' })
  })

  it('fails closed once the grant passes its own expiry, before the server is read again', () => {
    const result = connectionReadiness(running, grant('active', '2026-07-19T09:29:59.000Z'), '', NOW)
    expect(result).toMatchObject({ kind: 'needs-regrant', label: '待重新授权' })
    expect(grantIsUsable(grant('active', '2026-07-19T09:29:59.000Z'), NOW)).toBe(false)
  })

  it('asks for a new grant when the current grant cannot be confirmed', () => {
    const result = connectionReadiness(
      running,
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
