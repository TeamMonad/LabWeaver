import { describe, expect, it } from 'vitest'
import type { AccessGrantSchema, EnvironmentEndpointSchema } from '@/generated/contracts'
import type { AsyncState } from '@/types/async'
import { makeDiagnostic } from '@/types/async'
import { connectionMethods, connectionReadiness } from '@/utils/environmentConnection'

const running = { desiredState: 'running', observedState: 'ready' } as const

function endpoint(protocol: EnvironmentEndpointSchema['protocol'], health: EnvironmentEndpointSchema['health']): EnvironmentEndpointSchema {
  return { id: `${protocol}-${health}`, protocol, health, revision: 1, observedAt: '2026-07-11T10:00:00.000Z' }
}

function endpoints(...items: EnvironmentEndpointSchema[]): AsyncState<EnvironmentEndpointSchema[]> {
  return { kind: 'success', data: items }
}

function grant(state: AccessGrantSchema['state']): AsyncState<AccessGrantSchema> {
  return { kind: 'success', data: { state } as AccessGrantSchema }
}

describe('connectionReadiness', () => {
  it('uses the console wording when the instance itself is not ready', () => {
    const result = connectionReadiness({ desiredState: 'running', observedState: 'provisioning' }, endpoints(endpoint('ssh', 'healthy')), grant('active'), '环境就绪后才能签发访问授权。')
    expect(result).toMatchObject({ kind: 'instance-not-ready', detail: '环境就绪后才能签发访问授权。' })
  })

  it('treats a stop request on a ready instance as not connectable', () => {
    expect(connectionReadiness({ desiredState: 'stopped', observedState: 'ready' }, endpoints(endpoint('ssh', 'healthy')), grant('active'), 'blocked').kind).toBe('instance-not-ready')
  })

  it.each([
    [{ kind: 'loading' } as const, 'checking'],
    [{ kind: 'idle' } as const, 'checking'],
    [{ kind: 'empty' } as const, 'no-endpoints'],
    [{ kind: 'error', diagnostic: makeDiagnostic('ENDPOINT_LIST_FAILED', '加载环境入口失败', true) } as const, 'unknown'],
  ])('reads endpoint state %o as %s', (state, kind) => {
    expect(connectionReadiness(running, state, { kind: 'idle' }, '').kind).toBe(kind)
  })

  it('keeps an unreadable endpoint list distinct from having no endpoints', () => {
    const result = connectionReadiness(running, { kind: 'error', diagnostic: makeDiagnostic('X', '网络中断', true) }, { kind: 'idle' }, '')
    expect(result.detail).toBe('无法读取连接入口：网络中断')
  })

  it('distinguishes pending, unhealthy and removed endpoints', () => {
    expect(connectionReadiness(running, endpoints(endpoint('ssh', 'pending'), endpoint('https', 'unhealthy')), { kind: 'idle' }, '').kind).toBe('endpoints-pending')
    expect(connectionReadiness(running, endpoints(endpoint('https', 'unhealthy')), { kind: 'idle' }, '').kind).toBe('endpoints-unhealthy')
    expect(connectionReadiness(running, endpoints(endpoint('https', 'removed')), { kind: 'idle' }, '').kind).toBe('no-endpoints')
  })

  it('only reports ready once a healthy endpoint has an active grant', () => {
    const healthy = endpoints(endpoint('https', 'healthy'), endpoint('ssh', 'pending'))
    expect(connectionReadiness(running, healthy, { kind: 'idle' }, '').kind).toBe('needs-grant')
    expect(connectionReadiness(running, healthy, { kind: 'loading' }, '').kind).toBe('checking')
    expect(connectionReadiness(running, healthy, grant('requested'), '').kind).toBe('grant-pending')
    expect(connectionReadiness(running, healthy, grant('expired'), '').kind).toBe('needs-grant')
    expect(connectionReadiness(running, healthy, grant('active'), '')).toMatchObject({ kind: 'ready', label: '可连接' })
  })
})

describe('connectionMethods', () => {
  it('offers the web terminal for containers and the desktop for virtual machines', () => {
    expect(connectionMethods({ runtimeKind: 'container' }, { kind: 'empty' }).map((method) => method.id)).toEqual(['console'])
    expect(connectionMethods({ runtimeKind: 'virtual_machine' }, { kind: 'empty' }).map((method) => method.id)).toEqual(['desktop'])
  })

  it('adds SSH and application entries only for registered, non-removed endpoint protocols', () => {
    const methods = connectionMethods({ runtimeKind: 'virtual_machine' }, endpoints(endpoint('ssh', 'pending'), endpoint('http', 'removed')))
    expect(methods.map((method) => method.id)).toEqual(['desktop', 'ssh'])
    expect(connectionMethods({ runtimeKind: 'container' }, endpoints(endpoint('https', 'healthy'))).map((method) => method.id)).toEqual(['console', 'app'])
  })
})
