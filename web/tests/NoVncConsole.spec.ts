import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import NoVncConsole from '@/components/console/NoVncConsole.vue'
import type { ConsoleCapabilitySchema } from '@/generated/contracts'

const rfbState = vi.hoisted(() => {
  class FakeRfb {
    listeners = new Map<string, (event: CustomEvent) => void>()
    disconnect = vi.fn()

    constructor(
      public readonly target: Element,
      public readonly url: string,
      public readonly options: Record<string, unknown>,
    ) {
      state.instances.push(this)
    }

    addEventListener(name: string, listener: (event: CustomEvent) => void) {
      this.listeners.set(name, listener)
    }

    dispatch(name: string, detail: object) {
      this.listeners.get(name)?.(new CustomEvent(name, { detail }))
    }
  }

  const state = { instances: [] as FakeRfb[], FakeRfb }
  return state
})

vi.mock('@novnc/novnc/lib/rfb', () => ({ default: rfbState.FakeRfb }))

function capability(): ConsoleCapabilitySchema {
  return {
    id: 'capability-1',
    accessGrantId: 'grant-1',
    accessGrantRevision: 3,
    environmentClass: 'experiment',
    environmentId: 'environment-1',
    environmentRevision: 5,
    kind: 'novnc',
    connectionLocator: '/connect/console/opaque-locator',
    websocketSubprotocol: 'labweaver.console.novnc.v1',
    issuedAt: '2026-08-08T08:00:00.000Z',
    expiresAt: '2026-08-08T08:00:30.000Z',
    leaseFence: null,
  }
}

describe('NoVncConsole', () => {
  beforeEach(() => {
    rfbState.instances.length = 0
    vi.useFakeTimers()
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it('uses only the one-time locator and protocol, then requires manual reissuance after disconnect', () => {
    const wrapper = mount(NoVncConsole, { props: { capability: capability() } })
    expect(rfbState.instances).toHaveLength(1)
    const instance = rfbState.instances[0]
    expect(instance.url).toBe('ws://localhost:3000/connect/console/opaque-locator')
    expect(instance.options).toEqual({ wsProtocols: ['labweaver.console.novnc.v1'] })

    instance.dispatch('disconnect', { clean: false })
    vi.advanceTimersByTime(30_000)
    expect(rfbState.instances).toHaveLength(1)
    expect(wrapper.emitted('stateChange')).toContainEqual([
      'closed',
      'CONSOLE_UPSTREAM_UNAVAILABLE',
    ])
    wrapper.unmount()
    expect(instance.disconnect).toHaveBeenCalledOnce()
  })

  it('surfaces security denial without requesting a VNC credential or retrying', () => {
    const wrapper = mount(NoVncConsole, { props: { capability: capability() } })
    const instance = rfbState.instances[0]
    instance.dispatch('securityfailure', { reason: 1 })
    vi.advanceTimersByTime(30_000)

    expect(rfbState.instances).toHaveLength(1)
    expect(instance.options).not.toHaveProperty('credentials')
    expect(wrapper.emitted('stateChange')).toContainEqual(['error', 'CONSOLE_DENIED'])
    expect(wrapper.emitted('stateChange')?.filter(([state]) => state === 'error')).toHaveLength(1)
  })
})
