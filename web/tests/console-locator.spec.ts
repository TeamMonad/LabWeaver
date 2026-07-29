import { beforeEach, describe, expect, it } from 'vitest'
import { createFixtureConsoleSocketFactory } from '@/fixture/consoleSocket'
import { consumeLocator, resetConsoleCapabilityStore } from '@/fixture/stores/consoleCapabilityStore'
import type { ConsoleSocketState } from '@/console/socket'

function connectWith(factory: ReturnType<typeof createFixtureConsoleSocketFactory>, locator: string) {
  const states: Array<[ConsoleSocketState, string | undefined]> = []
  const socket = factory(locator, 'labweaver.console.xterm.v1', {
    onStateChange: (state, code) => states.push([state, code]),
    onData: () => {},
  })
  return { states, socket }
}

describe('console locator one-time consumption (ADR 0012)', () => {
  beforeEach(() => {
    resetConsoleCapabilityStore()
  })

  it('allows the first consumption and denies the second', async () => {
    const factory = createFixtureConsoleSocketFactory()
    const locator = '/api/v1/console-sessions/session-1'

    const first = connectWith(factory, locator)
    await new Promise((r) => setTimeout(r, 0))
    expect(first.states).toContainEqual(['open', undefined])

    const second = connectWith(factory, locator)
    await new Promise((r) => setTimeout(r, 0))
    expect(second.states).toContainEqual(['error', 'CONSOLE_LOCATOR_CONSUMED'])
    expect(second.states).not.toContainEqual(['open', undefined])
  })

  it('consumeLocator returns true once and false afterwards', () => {
    expect(consumeLocator('/api/v1/console-sessions/session-9')).toBe(true)
    expect(consumeLocator('/api/v1/console-sessions/session-9')).toBe(false)
  })
})
