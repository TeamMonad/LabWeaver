/**
 * Console WebSocket transport.
 *
 * A `ConsoleCapability` hands the browser a one-time `connectionLocator` and a
 * versioned subprotocol; the handoff secret rides in a path-scoped HttpOnly
 * cookie and is never part of the URL or response body. This module owns the
 * real transport; fixture mode injects a deterministic in-memory substitute
 * through the same interface so E1/E2 stays honest without pretending the
 * upstream proxy exists.
 */

export type ConsoleSocketState =
  | 'connecting'
  | 'open'
  | 'closed'
  | 'error'

export interface ConsoleSocketHandlers {
  onStateChange: (state: ConsoleSocketState, diagnosticCode?: string) => void
  onData: (data: string | ArrayBuffer) => void
}

export interface ConsoleSocket {
  send: (data: string | ArrayBuffer) => void
  sendResize: (cols: number, rows: number) => void
  close: () => void
}

export interface ConsoleSocketFactory {
  (locator: string, subprotocol: string, handlers: ConsoleSocketHandlers): ConsoleSocket
}

function toWebSocketUrl(locator: string): string {
  const base = new URL(locator, window.location.origin)
  base.protocol = base.protocol === 'https:' ? 'wss:' : 'ws:'
  return base.toString()
}

export function createWebSocketConsoleSocket(
  locator: string,
  subprotocol: string,
  handlers: ConsoleSocketHandlers,
): ConsoleSocket {
  const url = toWebSocketUrl(locator)
  const ws = new WebSocket(url, subprotocol)

  ws.addEventListener('open', () => handlers.onStateChange('open'))
  ws.addEventListener('message', (event) => {
    handlers.onData(event.data as string | ArrayBuffer)
  })
  ws.addEventListener('error', () => handlers.onStateChange('error', 'CONSOLE_UPSTREAM_UNAVAILABLE'))
  ws.addEventListener('close', (event) => {
    const code = event.code === 4403 ? 'CONSOLE_DENIED' : event.code === 4401 ? 'CONSOLE_EXPIRED' : undefined
    handlers.onStateChange('closed', code)
  })

  return {
    send(data) {
      if (ws.readyState === WebSocket.OPEN) ws.send(data)
    },
    sendResize(cols, rows) {
      if (ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: 'resize', cols, rows }))
      }
    },
    close() {
      ws.close(1000)
    },
  }
}
