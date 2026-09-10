import { describe, expect, it } from 'vitest'
import { createMockConsoleSocketFactory } from './consoleSocketMock'
import type { ConsoleSocketState } from '@/console/socket'

function connectWith(factory: ReturnType<typeof createMockConsoleSocketFactory>, locator: string) {
  const states: Array<[ConsoleSocketState, string | undefined]> = []
  const socket = factory(locator, 'labweaver.console.xterm.v1', {
    onStateChange: (state, code) => states.push([state, code]),
    onData: () => {},
  })
  return { states, socket }
}

describe('console external transport boundary', () => {
  it('opens a mocked external console connection', async () => {
    const result = connectWith(createMockConsoleSocketFactory(), '/connect/console/session-1')
    await new Promise((r) => setTimeout(r, 0))
    expect(result.states).toContainEqual(['open', undefined])
  })
})
