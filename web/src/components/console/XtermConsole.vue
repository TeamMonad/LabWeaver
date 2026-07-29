<template>
  <div class="xterm-console">
    <div ref="terminalEl" class="xterm-host" />
  </div>
</template>

<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { Terminal } from '@xterm/xterm'
import '@xterm/xterm/css/xterm.css'

interface Props {
  send: (data: string | ArrayBuffer) => void
  sendResize: (cols: number, rows: number) => void
}

const props = defineProps<Props>()

const emit = defineEmits<{
  data: [data: string | ArrayBuffer]
}>()

const terminalEl = ref<HTMLDivElement | null>(null)
let terminal: Terminal | null = null
let resizeObserver: ResizeObserver | null = null

function handleData(data: string | ArrayBuffer) {
  if (typeof data === 'string') terminal?.write(data)
  else terminal?.write(new Uint8Array(data))
}

defineExpose({ handleData })

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
  terminal.open(terminalEl.value)
  terminal.onData((data) => props.send(data))

  resizeObserver = new ResizeObserver(() => {
    if (!terminal || !terminalEl.value) return
    const cols = terminal.cols
    const rows = terminal.rows
    if (cols > 0 && rows > 0) props.sendResize(cols, rows)
  })
  resizeObserver.observe(terminalEl.value)
})

onBeforeUnmount(() => {
  resizeObserver?.disconnect()
  terminal?.dispose()
  terminal = null
})
</script>

<style scoped>
.xterm-console {
  width: 100%;
  min-height: 320px;
  border-radius: var(--md-sys-shape-medium);
  overflow: hidden;
  background: #1e1e1e;
}

.xterm-host {
  width: 100%;
  height: 100%;
  min-height: 320px;
  padding: 8px;
}
</style>
