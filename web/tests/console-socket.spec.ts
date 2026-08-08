import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createWebSocketConsoleSocket } from '@/console/socket'

class FakeWebSocket {
  static OPEN = 1
  static last: FakeWebSocket
  readyState = FakeWebSocket.OPEN
  binaryType = ''
  sent: Array<string | ArrayBuffer | ArrayBufferView> = []
  listeners = new Map<string, (event: never) => void>()
  constructor(public url: string, public protocol: string) { FakeWebSocket.last = this }
  addEventListener(name: string, listener: (event: never) => void) { this.listeners.set(name, listener) }
  send(value: string | ArrayBuffer | ArrayBufferView) { this.sent.push(value) }
  close = vi.fn()
}

describe('console WebSocket framing', () => {
  beforeEach(() => { vi.stubGlobal('WebSocket', FakeWebSocket) })

  it('encodes terminal input as binary and keeps resize as JSON control text', () => {
    const socket = createWebSocketConsoleSocket('/connect/console/opaque', 'labweaver.console.xterm.v1', {
      onStateChange: vi.fn(), onData: vi.fn(),
    })
    socket.send('pwd\r')
    socket.sendResize(120, 40)
    expect(FakeWebSocket.last.binaryType).toBe('arraybuffer')
    expect(ArrayBuffer.isView(FakeWebSocket.last.sent[0])).toBe(true)
    expect(Array.from(FakeWebSocket.last.sent[0] as Uint8Array)).toEqual([112, 119, 100, 13])
    expect(FakeWebSocket.last.sent[1]).toBe('{"type":"resize","cols":120,"rows":40}')
  })
})
