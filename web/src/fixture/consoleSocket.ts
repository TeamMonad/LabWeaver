/**
 * Deterministic in-memory console socket used only in fixture mode. It speaks
 * the same `ConsoleSocketFactory` interface as the real WebSocket transport so
 * the UI is exercised end-to-end; it is never a substitute for the upstream
 * proxy and never pretends to reach a real runtime.
 */
import type { ConsoleSocket, ConsoleSocketFactory } from '@/console/socket'

const BANNER = 'LabWeaver fixture console\r\n\x1b[90m(deterministic in-memory terminal; no real runtime attached)\x1b[0m\r\n$ '

export function createFixtureConsoleSocket(): ConsoleSocketFactory {
  return (locator, subprotocol, handlers) => {
    let open = true
    const state: ConsoleSocket = {
      send(data) {
        if (!open) return
        if (typeof data === 'string') {
          // Echo printable input like a minimal shell would.
          handlers.onData(data === '\r' ? '\r\n$ ' : data)
        }
      },
      sendResize() {
        // Resizing is a no-op for the in-memory terminal.
      },
      close() {
        if (!open) return
        open = false
        handlers.onStateChange('closed')
      },
    }

    queueMicrotask(() => {
      handlers.onStateChange('open')
      handlers.onData(`${BANNER}`)
    })

    return state
  }
}

export function createFixtureConsoleSocketFactory(): ConsoleSocketFactory {
  return createFixtureConsoleSocket()
}
