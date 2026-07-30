<template>
  <div class="xterm-console">
    <div ref="terminalEl" class="xterm-host" />
  </div>
</template>

<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import '@xterm/xterm/css/xterm.css'

interface Props {
  send: (data: string | ArrayBuffer) => void
  sendResize: (cols: number, rows: number) => void
}

const props = defineProps<Props>()

const terminalEl = ref<HTMLDivElement | null>(null)
let terminal: Terminal | null = null
let fitAddon: FitAddon | null = null
let resizeObserver: ResizeObserver | null = null

function handleData(data: string | ArrayBuffer) {
  if (typeof data === 'string') terminal?.write(data)
  else terminal?.write(new Uint8Array(data))
}

defineExpose({ handleData })

function fitAndNotify() {
  if (!terminal || !fitAddon) return
  try {
    fitAddon.fit()
    if (terminal.cols > 0 && terminal.rows > 0) {
      props.sendResize(terminal.cols, terminal.rows)
    }
  } catch {
    // fit() can throw before layout settles; the ResizeObserver will retry.
  }
}

onMounted(() => {
  if (!terminalEl.value) return
  terminal = new Terminal({
    cursorBlink: true,
    fontSize: 13,
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
    // High-contrast theme so dim/bright ANSI text stays above the WCAG AA
    // 4.5:1 ratio on the dark terminal background.
    theme: {
      background: '#1e1e1e',
      foreground: '#d4d4d4',
      black: '#1e1e1e',
      brightBlack: '#a3a3a3',
      red: '#f48771',
      brightRed: '#f48771',
      green: '#7ec699',
      brightGreen: '#7ec699',
      yellow: '#e5c07b',
      brightYellow: '#e5c07b',
      blue: '#7aa2f7',
      brightBlue: '#7aa2f7',
      magenta: '#c586c0',
      brightMagenta: '#c586c0',
      cyan: '#56b6c2',
      brightCyan: '#56b6c2',
      white: '#d4d4d4',
      brightWhite: '#ffffff',
      cursor: '#d4d4d4',
    },
  })
  fitAddon = new FitAddon()
  terminal.loadAddon(fitAddon)
  terminal.open(terminalEl.value)
  terminal.onData((data) => props.send(data))

  // Bind terminal rows/cols to the container's measured geometry so the
  // viewport is deterministic instead of relying on xterm's internal sizing.
  fitAndNotify()

  resizeObserver = new ResizeObserver(() => fitAndNotify())
  resizeObserver.observe(terminalEl.value)
})

onBeforeUnmount(() => {
  resizeObserver?.disconnect()
  terminal?.dispose()
  terminal = null
  fitAddon = null
})
</script>

<style scoped>
.xterm-console {
  width: 100%;
  /* Deterministic responsive geometry: a fixed viewport height so the panel
     and terminal never collapse or grow an internal scrollbar across runs. */
  height: 400px;
  border-radius: var(--md-sys-shape-medium);
  overflow: hidden;
  background: #1e1e1e;
}

.xterm-host {
  width: 100%;
  height: 100%;
  padding: 8px;
  box-sizing: border-box;
  overflow: hidden;
}

@media (max-width: 599px) {
  .xterm-console {
    height: 360px;
  }
}
</style>
