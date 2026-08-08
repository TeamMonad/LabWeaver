<template>
  <div class="novnc-console">
    <div ref="rfbEl" class="novnc-host" />
  </div>
</template>

<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import RFB from '@novnc/novnc/lib/rfb'
import type { ConsoleCapabilitySchema } from '@/generated/contracts'

interface Props {
  capability: ConsoleCapabilitySchema
}

const props = defineProps<Props>()

const emit = defineEmits<{
  stateChange: [state: 'connecting' | 'open' | 'closed' | 'error', diagnosticCode?: string]
}>()

const rfbEl = ref<HTMLDivElement | null>(null)
let rfb: RFB | null = null
let connected = false
let connectTimer: ReturnType<typeof setTimeout> | null = null

// If the upstream never completes the WebSocket handshake (e.g. an
// unavailable proxy), surface a stable diagnostic instead of hanging on a
// black canvas forever.
const CONNECT_TIMEOUT_MS = 5000

function toWebSocketUrl(locator: string): string {
  const base = new URL(locator, window.location.origin)
  base.protocol = base.protocol === 'https:' ? 'wss:' : 'ws:'
  return base.toString()
}

function clearConnectTimer() {
  if (connectTimer) clearTimeout(connectTimer)
  connectTimer = null
}

onMounted(() => {
  if (!rfbEl.value) return
  emit('stateChange', 'connecting')
  connectTimer = setTimeout(() => {
    if (!connected) emit('stateChange', 'error', 'CONSOLE_UPSTREAM_UNAVAILABLE')
  }, CONNECT_TIMEOUT_MS)
  try {
    rfb = new RFB(rfbEl.value, toWebSocketUrl(props.capability.connectionLocator), {
      wsProtocols: [props.capability.websocketSubprotocol],
      // The handoff secret rides in a path-scoped HttpOnly cookie; no VNC
      // password or credential is ever requested from or shown to the browser.
    })
    rfb.addEventListener('connect', () => {
      connected = true
      clearConnectTimer()
      emit('stateChange', 'open')
    })
    rfb.addEventListener('disconnect', (e) => {
      clearConnectTimer()
      const detail = (e as CustomEvent<{ clean?: boolean }>).detail
      emit('stateChange', 'closed', detail?.clean ? undefined : 'CONSOLE_UPSTREAM_UNAVAILABLE')
    })
    rfb.addEventListener('securityfailure', (e) => {
      clearConnectTimer()
      const detail = (e as CustomEvent<{ reason?: number }>).detail
      emit('stateChange', 'error', detail?.reason === 1 ? 'CONSOLE_DENIED' : 'CONSOLE_UPSTREAM_UNAVAILABLE')
    })
  } catch {
    clearConnectTimer()
    emit('stateChange', 'error', 'CONSOLE_UPSTREAM_UNAVAILABLE')
  }
})

onBeforeUnmount(() => {
  clearConnectTimer()
  rfb?.disconnect()
  rfb = null
})
</script>

<style scoped>
.novnc-console {
  width: 100%;
  min-height: 320px;
  border-radius: var(--md-sys-shape-medium);
  overflow: hidden;
  background: #000;
}

.novnc-host {
  width: 100%;
  height: 100%;
  min-height: 320px;
}
</style>
