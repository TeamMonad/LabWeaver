<template>
  <div class="console-preview">
    <header class="preview-header">
      <h2>控制台布局预览（Fixture）</h2>
      <p class="preview-subtitle">确定性布局预览，不创建环境、不签发授权、不调用任何后端接口。</p>
    </header>

    <section class="preview-section" aria-labelledby="xterm-preview-heading">
      <h3 id="xterm-preview-heading" class="preview-title">xterm.js 浏览器终端</h3>
      <div class="preview-panel">
        <div class="preview-panel__header">
          <span>浏览器终端</span>
        </div>
        <XtermConsole ref="xtermRef" :send="sendXterm" :send-resize="noopResize" />
      </div>
    </section>

    <section class="preview-section" aria-labelledby="novnc-preview-heading">
      <h3 id="novnc-preview-heading" class="preview-title">noVNC 图形控制台</h3>
      <div class="preview-panel preview-panel--novnc">
        <div class="preview-panel__header">
          <span>图形控制台</span>
        </div>
        <div class="novnc-canvas" role="img" aria-label="noVNC 控制台占位（上游不可用）">
          <p class="novnc-note">CONSOLE_UPSTREAM_UNAVAILABLE — 上游不可用时不渲染 VNC 帧。</p>
        </div>
      </div>
    </section>
  </div>
</template>

<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import XtermConsole from '@/components/console/XtermConsole.vue'
import { createFixtureConsoleSocketFactory, type ConsoleSocket } from '@/fixture/consoleSocket'

// Static preview locator. The fixture socket is deterministic and purely
// in-memory; no capability issuance, grant, or backend request is made.
const PREVIEW_LOCATOR = '/fixture/console-preview/xterm'

const xtermRef = ref<InstanceType<typeof XtermConsole> | null>(null)
let socket: ConsoleSocket | null = null

function sendXterm(data: string | ArrayBuffer) {
  socket?.send(data)
}

function noopResize() {
  // Preview does not report geometry anywhere.
}

onMounted(() => {
  const factory = createFixtureConsoleSocketFactory()
  socket = factory(PREVIEW_LOCATOR, 'labweaver.console.xterm.v1', {
    onStateChange: () => {},
    onData: (data) => xtermRef.value?.handleData(data),
  })
})

onBeforeUnmount(() => {
  socket?.close()
  socket = null
})
</script>

<style scoped>
.console-preview {
  display: flex;
  flex-direction: column;
  gap: 28px;
}

.preview-header h2 {
  font: var(--md-sys-headline-small);
  color: var(--md-sys-color-on-surface);
  margin: 0;
}

.preview-subtitle {
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
  margin: 4px 0 0;
}

.preview-section {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.preview-title {
  font: var(--md-sys-title-medium);
  color: var(--md-sys-color-on-surface);
  margin: 0;
}

.preview-panel {
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
  overflow: hidden;
}

.preview-panel__header {
  padding: 12px 16px;
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
  font: var(--md-sys-title-small);
  color: var(--md-sys-color-on-surface);
}

.novnc-canvas {
  display: flex;
  align-items: center;
  justify-content: center;
  height: 400px;
  background: #000;
}

.novnc-note {
  font: var(--md-sys-body-medium);
  color: #a3a3a3;
  text-align: center;
  padding: 24px;
}

@media (max-width: 599px) {
  .novnc-canvas {
    height: 360px;
  }
}
</style>
