import { describe, expect, it } from 'vitest'
import { useConsoleSession } from '@/composables/useConsoleSession'
import { createFixtureConsoleSocketFactory } from '@/fixture/consoleSocket'
import type { ConsoleCapabilitySchema } from '@/generated/contracts'
import type { ConsoleSocketHandlers } from '@/console/socket'

function makeCapability(overrides: Partial<ConsoleCapabilitySchema> = {}): ConsoleCapabilitySchema {
  return {
    id: 'cap-1',
    accessGrantId: 'grant-1',
    accessGrantRevision: 3,
    environmentClass: 'experiment',
    environmentId: 'env-1',
    environmentRevision: 5,
    kind: 'xterm',
    connectionLocator: '/api/v1/console-sessions/session-1',
    websocketSubprotocol: 'labweaver.console.xterm.v1',
    issuedAt: '2026-07-16T08:00:00.000Z',
    expiresAt: '2026-07-16T09:00:00.000Z',
    leaseFence: null,
    ...overrides,
  }
}

function factoryThat(states: Array<[Parameters<ConsoleSocketHandlers['onStateChange']>[0], string?]>) {
  return (_locator: string, _subprotocol: string, handlers: ConsoleSocketHandlers) => {
    queueMicrotask(() => {
      for (const [state, code] of states) handlers.onStateChange(state, code)
    })
    return {
      send: () => {},
      sendResize: () => {},
      close: () => {},
    }
  }
}

describe('useConsoleSession', () => {
  it('opens and receives data via the fixture socket', async () => {
    const session = useConsoleSession()
    const received: string[] = []
    session.onData((d) => { if (typeof d === 'string') received.push(d) })
    await session.connect(makeCapability(), createFixtureConsoleSocketFactory())
    expect(session.status).toBe('open')
    await new Promise((r) => setTimeout(r, 0))
    expect(received.join('')).toContain('LabWeaver fixture console')
  })

  it('maps closed-with-denied to denied status', async () => {
    const session = useConsoleSession()
    await session.connect(makeCapability(), factoryThat([['closed', 'CONSOLE_DENIED']]))
    expect(session.status).toBe('denied')
  })

  it('maps closed-with-expired to expired status', async () => {
    const session = useConsoleSession()
    await session.connect(makeCapability(), factoryThat([['closed', 'CONSOLE_EXPIRED']]))
    expect(session.status).toBe('expired')
  })

  it('maps socket error to error status with diagnostic', async () => {
    const session = useConsoleSession()
    await session.connect(makeCapability(), factoryThat([['error', 'CONSOLE_UPSTREAM_UNAVAILABLE']]))
    expect(session.status).toBe('error')
    expect(session.diagnostic?.code).toBe('CONSOLE_UPSTREAM_UNAVAILABLE')
  })
})

describe('createFixtureConsoleSocketFactory', () => {
  it('echoes input like a minimal shell', async () => {
    const factory = createFixtureConsoleSocketFactory()
    const received: string[] = []
    const socket = factory('/x', 'proto', {
      onStateChange: () => {},
      onData: (d) => { if (typeof d === 'string') received.push(d) },
    })
    await new Promise((r) => setTimeout(r, 0))
    socket.send('ls')
    socket.send('\r')
    const text = received.join('')
    expect(text).toContain('LabWeaver fixture console')
    expect(text).toContain('ls')
    expect(text).toContain('$')
  })
})
