import { beforeEach, describe, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import BrowserTerminal from '@/components/environment/BrowserTerminal.vue'

const terminalWrite = vi.fn()
const terminalFocus = vi.fn()
let terminalInput: ((value: string) => void) | undefined

vi.mock('@xterm/xterm', () => ({
  Terminal: class {
    cols = 80
    rows = 24
    loadAddon() {}
    open() {}
    onData(callback: (value: string) => void) { terminalInput = callback }
    write = terminalWrite
    writeln = terminalWrite
    focus = terminalFocus
    dispose() {}
  },
}))

vi.mock('@xterm/addon-fit', () => ({
  FitAddon: class { fit() {} },
}))

class FakeResizeObserver {
  observe() {}
  disconnect() {}
}

class FakeWebSocket {
  static readonly OPEN = 1
  static instances: FakeWebSocket[] = []
  readyState = 0
  protocol = 'labweaver.terminal.v1'
  binaryType = ''
  sent: unknown[] = []
  listeners = new Map<string, Array<(event: unknown) => void>>()

  constructor(public url: string, public requestedProtocol: string) {
    FakeWebSocket.instances.push(this)
  }

  addEventListener(name: string, callback: (event: unknown) => void) {
    const callbacks = this.listeners.get(name) ?? []
    callbacks.push(callback)
    this.listeners.set(name, callbacks)
  }

  emit(name: string, event: unknown = {}) {
    if (name === 'open') this.readyState = FakeWebSocket.OPEN
    for (const callback of this.listeners.get(name) ?? []) callback(event)
  }

  send(value: unknown) { this.sent.push(value) }
  close(code: number) {
    this.readyState = 3
    this.emit('close', { code })
  }
}

describe('BrowserTerminal', () => {
  beforeEach(() => {
    FakeWebSocket.instances = []
    terminalWrite.mockReset()
    terminalFocus.mockReset()
    terminalInput = undefined
    vi.stubGlobal('ResizeObserver', FakeResizeObserver)
    vi.stubGlobal('WebSocket', FakeWebSocket)
  })

  it('uses the fixed same-origin subprotocol and reconnects only on user action', async () => {
    const wrapper = mount(BrowserTerminal, {
      props: { terminalUrl: '/connect/eg-1/terminal' },
    })
    await vi.waitFor(() => expect(terminalInput).toBeTypeOf('function'))
    await wrapper.get('button.filled-button').trigger('click')
    const socket = FakeWebSocket.instances[0]
    expect(socket?.url).toBe('ws://localhost:3000/connect/eg-1/terminal')
    expect(socket?.requestedProtocol).toBe('labweaver.terminal.v1')

    socket?.emit('open')
    expect(socket?.sent[0]).toBe(JSON.stringify({ type: 'open', cols: 80, rows: 24 }))
    terminalInput?.('pwd\n')
    expect(ArrayBuffer.isView(socket?.sent[1])).toBe(true)

    socket?.emit('close', { code: 1011 })
    await wrapper.vm.$nextTick()
    expect(wrapper.text()).toContain('连接失败')
    expect(FakeWebSocket.instances).toHaveLength(1)

    await wrapper.get('button.filled-button').trigger('click')
    expect(FakeWebSocket.instances).toHaveLength(2)
  })

  it('rejects a cross-origin terminal URL before opening a socket', async () => {
    const wrapper = mount(BrowserTerminal, {
      props: { terminalUrl: 'https://other.invalid/connect/eg-1/terminal' },
    })
    await wrapper.get('button.filled-button').trigger('click')
    expect(FakeWebSocket.instances).toHaveLength(0)
    expect(wrapper.text()).toContain('LW_WEB_TERMINAL_CROSS_ORIGIN_REJECTED')
  })
})
