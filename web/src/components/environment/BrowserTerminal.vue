<template>
  <section class="browser-terminal" :class="{ 'browser-terminal--fullscreen': fullscreen }" aria-labelledby="browser-terminal-title">
    <header class="browser-terminal__header">
      <div>
        <h5 id="browser-terminal-title">容器终端</h5>
        <p class="browser-terminal__status" role="status" aria-live="polite">
          <span class="browser-terminal__dot" :class="`browser-terminal__dot--${status}`" />
          {{ statusLabel }}
          <code v-if="diagnostic">{{ diagnostic }}</code>
        </p>
      </div>
      <div class="browser-terminal__actions">
        <button v-if="status === 'connected' || status === 'connecting'" type="button" class="outlined-button small" @click="disconnect">
          断开
        </button>
        <button v-else type="button" class="filled-button small" :disabled="!terminalUrl" @click="connect">
          {{ hasConnected ? '手动重连' : '连接' }}
        </button>
        <button type="button" class="text-button small" @click="fullscreen = !fullscreen">
          {{ fullscreen ? '退出全屏' : '全屏' }}
        </button>
      </div>
    </header>
    <div ref="terminalElement" class="browser-terminal__viewport" tabindex="0" aria-label="容器交互终端" />
    <p class="browser-terminal__notice">终端内容仅在当前浏览器会话中显示，不会写入演示证据或服务日志。</p>
  </section>
</template>

<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import type { FitAddon as XtermFitAddon } from '@xterm/addon-fit'
import type { Terminal as XtermTerminal } from '@xterm/xterm'
import '@xterm/xterm/css/xterm.css'

type TerminalStatus = 'disconnected' | 'connecting' | 'connected' | 'failed'

const props = defineProps<{
  terminalUrl: string | null
}>()

const terminalElement = ref<HTMLElement | null>(null)
const status = ref<TerminalStatus>('disconnected')
const diagnostic = ref<string | null>(null)
const fullscreen = ref(false)
const hasConnected = ref(false)
let socket: WebSocket | null = null
let terminal: XtermTerminal | null = null
let fitAddon: XtermFitAddon | null = null
let resizeObserver: ResizeObserver | null = null
let intentionalClose = false

const statusLabel = computed(() => {
  switch (status.value) {
    case 'connecting': return '连接中'
    case 'connected': return '已连接'
    case 'failed': return '连接失败'
    default: return '未连接'
  }
})

function websocketUrl(path: string): string {
  const url = new URL(path, window.location.href)
  if (url.origin !== window.location.origin) throw new Error('LW_WEB_TERMINAL_CROSS_ORIGIN_REJECTED')
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:'
  return url.toString()
}

function sendControl(message: Record<string, unknown>) {
  if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(message))
}

function resize() {
  if (!terminal || !fitAddon) return
  fitAddon.fit()
  sendControl({ type: 'resize', cols: terminal.cols, rows: terminal.rows })
}

function connect() {
  if (!props.terminalUrl || socket) return
  diagnostic.value = null
  status.value = 'connecting'
  intentionalClose = false
  try {
    socket = new WebSocket(websocketUrl(props.terminalUrl), 'labweaver.terminal.v1')
    socket.binaryType = 'arraybuffer'
  } catch (error) {
    socket = null
    status.value = 'failed'
    diagnostic.value = error instanceof Error ? error.message : 'LW_WEB_TERMINAL_CONNECT_FAILED'
    return
  }
  const current = socket
  current.addEventListener('open', () => {
    if (current.protocol !== 'labweaver.terminal.v1') {
      diagnostic.value = 'LW_WEB_TERMINAL_SUBPROTOCOL_MISMATCH'
      current.close(1002, 'subprotocol required')
      return
    }
    hasConnected.value = true
    status.value = 'connected'
    sendControl({ type: 'open', cols: terminal?.cols ?? 80, rows: terminal?.rows ?? 24 })
    terminal?.focus()
  })
  current.addEventListener('message', (event) => {
    if (typeof event.data !== 'string') {
      terminal?.write(new Uint8Array(event.data as ArrayBuffer))
      return
    }
    try {
      const control = JSON.parse(event.data) as { type?: string; code?: string; exitCode?: number }
      if (control.type === 'diagnostic') diagnostic.value = control.code ?? 'LW_WEB_TERMINAL_REMOTE_ERROR'
      if (control.type === 'exit') {
        terminal?.writeln(`\r\n[process exited: ${control.exitCode ?? 'unknown'}]`)
        current.close(1000, 'process exited')
      }
    } catch {
      diagnostic.value = 'LW_WEB_TERMINAL_PROTOCOL_INVALID'
      current.close(1002, 'invalid control frame')
    }
  })
  current.addEventListener('close', (event) => {
    if (socket === current) socket = null
    if (intentionalClose || event.code === 1000) {
      status.value = 'disconnected'
      return
    }
    status.value = 'failed'
    diagnostic.value ||= `LW_WEB_TERMINAL_CLOSED_${event.code}`
  })
  current.addEventListener('error', () => {
    diagnostic.value = 'LW_WEB_TERMINAL_TRANSPORT_FAILED'
  })
}

function disconnect() {
  intentionalClose = true
  socket?.close(1000, 'user disconnected')
  socket = null
  status.value = 'disconnected'
}

onMounted(async () => {
  const [{ Terminal }, { FitAddon }] = await Promise.all([
    import('@xterm/xterm'),
    import('@xterm/addon-fit'),
  ])
  terminal = new Terminal({
    convertEol: true,
    cursorBlink: true,
    fontFamily: '"Roboto Mono", ui-monospace, monospace',
    fontSize: 14,
    scrollback: 2000,
    theme: { background: '#111318', foreground: '#e2e2e9', cursor: '#a8c7fa' },
  })
  fitAddon = new FitAddon()
  terminal.loadAddon(fitAddon)
  if (terminalElement.value) terminal.open(terminalElement.value)
  terminal.onData((data) => {
    if (socket?.readyState === WebSocket.OPEN) socket.send(new TextEncoder().encode(data))
  })
  resizeObserver = new ResizeObserver(() => resize())
  if (terminalElement.value) resizeObserver.observe(terminalElement.value)
  nextTick(resize)
})

watch(fullscreen, () => nextTick(resize))
watch(() => props.terminalUrl, () => disconnect())

onBeforeUnmount(() => {
  disconnect()
  resizeObserver?.disconnect()
  terminal?.dispose()
})
</script>

<style scoped>
.browser-terminal { margin-top: 1rem; overflow: hidden; border: 1px solid var(--md-sys-color-outline-variant); border-radius: 12px; background: #111318; color: #e2e2e9; }
.browser-terminal--fullscreen { position: fixed; z-index: 1200; inset: 0; border: 0; border-radius: 0; padding: 1rem; }
.browser-terminal__header { display: flex; align-items: center; justify-content: space-between; gap: 1rem; padding: .75rem 1rem; background: #1b1b1f; }
.browser-terminal__header h5 { margin: 0; }
.browser-terminal__status { display: flex; align-items: center; gap: .5rem; margin: .25rem 0 0; font-size: .8rem; }
.browser-terminal__dot { width: .5rem; height: .5rem; border-radius: 50%; background: #8e9099; }
.browser-terminal__dot--connected { background: #6dd58c; }
.browser-terminal__dot--connecting { background: #fdd663; }
.browser-terminal__dot--failed { background: #ffb4ab; }
.browser-terminal__actions { display: flex; gap: .5rem; }
.browser-terminal__viewport { height: 22rem; padding: .5rem; }
.browser-terminal--fullscreen .browser-terminal__viewport { height: calc(100vh - 8rem); }
.browser-terminal__notice { margin: 0; padding: .5rem 1rem; color: #c4c6d0; font-size: .75rem; background: #1b1b1f; }
@media (max-width: 640px) { .browser-terminal__header { align-items: flex-start; flex-direction: column; } }
</style>
