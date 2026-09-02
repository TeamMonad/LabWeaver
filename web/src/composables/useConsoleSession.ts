import { computed, reactive, ref } from 'vue'
import type { ConsoleCapabilitySchema } from '@/generated/contracts'
import { IS_FIXTURE } from '@/config/dataMode'
import { createWebSocketConsoleSocket, type ConsoleSocket, type ConsoleSocketFactory } from '@/console/socket'
import { makeDiagnostic, type DiagnosticViewModel } from '@/types/async'

export type ConsoleSessionStatus =
  | 'idle'
  | 'connecting'
  | 'open'
  | 'closed'
  | 'expired'
  | 'denied'
  | 'error'

async function resolveFactory(): Promise<ConsoleSocketFactory> {
  if (IS_FIXTURE) {
    // Deterministic in-memory substitute, loaded only in fixture mode so the
    // production bundle carries no fixture code.
    const mod = await import('@/fixture/consoleSocket')
    return mod.createFixtureConsoleSocketFactory()
  }
  return createWebSocketConsoleSocket
}

export function useConsoleSession() {
  const status = ref<ConsoleSessionStatus>('idle')
  const diagnostic = ref<DiagnosticViewModel | null>(null)
  const capability = ref<ConsoleCapabilitySchema | null>(null)
  let socket: ConsoleSocket | null = null

  const isOpen = computed(() => status.value === 'open')

  const onDataHandlers: Array<(data: string | ArrayBuffer) => void> = []
  function onData(handler: (data: string | ArrayBuffer) => void) {
    onDataHandlers.push(handler)
  }

  async function connect(cap: ConsoleCapabilitySchema, injectedFactory?: ConsoleSocketFactory) {
    disconnect()
    capability.value = cap
    diagnostic.value = null
    status.value = 'connecting'
    const factory = injectedFactory ?? (await resolveFactory())
    socket = factory(cap.connectionLocator, cap.websocketSubprotocol, {
      onStateChange(state, code) {
        if (state === 'open') {
          status.value = 'open'
        } else if (state === 'closed') {
          status.value = code === 'CONSOLE_DENIED' ? 'denied' : code === 'CONSOLE_EXPIRED' ? 'expired' : 'closed'
        } else if (state === 'error') {
          status.value = 'error'
          diagnostic.value = makeDiagnostic(code ?? 'CONSOLE_UPSTREAM_UNAVAILABLE', '控制台上游不可用。', true)
        }
      },
      onData(data) {
        onDataHandlers.forEach((h) => h(data))
      },
    })
  }

  function send(data: string | ArrayBuffer) {
    socket?.send(data)
  }

  function sendResize(cols: number, rows: number) {
    socket?.sendResize(cols, rows)
  }

  function disconnect() {
    socket?.close()
    socket = null
    status.value = 'idle'
  }

  return reactive({
    status,
    diagnostic,
    capability,
    isOpen,
    connect,
    disconnect,
    send,
    sendResize,
    onData,
  })
}
