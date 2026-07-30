<template>
  <div class="console-panel">
    <AsyncStateView :state="capability.availability" @retry="capability.load">
      <template #success="{ data }">
        <DiagnosticBanner
          v-if="!data.kinds.includes(kind)"
          code="CONSOLE_KIND_NOT_ELIGIBLE"
          message="当前环境不支持此控制台能力。"
          :retryable="false"
          severity="warning"
        />
        <template v-else>
          <div class="console-header">
            <h5 class="console-title">
              <SvgIcon :name="kind === 'xterm' ? 'terminal' : 'desktop_windows'" size="sm" aria-hidden="true" />
              {{ kind === 'xterm' ? '浏览器终端' : '图形控制台' }}
            </h5>
            <div class="console-actions">
              <button
                v-if="!sessionActive"
                type="button"
                class="filled-button"
                :disabled="!isReady || capability.issuing"
                @click="openConsole"
              >
                {{ capability.issuing ? '签发中…' : kind === 'xterm' ? '打开终端' : '打开图形控制台' }}
              </button>
              <button v-else type="button" class="text-button" @click="closeConsole">
                断开
              </button>
              <button type="button" class="icon-button" aria-label="全屏" @click="toggleFullscreen">
                <SvgIcon name="fullscreen" size="sm" aria-hidden="true" />
              </button>
            </div>
          </div>

          <DiagnosticBanner
            v-if="issueError"
            :code="issueError.code"
            :message="issueError.message"
            :retryable="issueError.retryable"
            severity="error"
            @retry="openConsole"
          />
          <DiagnosticBanner
            v-else-if="session.diagnostic"
            :code="session.diagnostic.code"
            :message="session.diagnostic.message"
            :retryable="session.diagnostic.retryable"
            severity="error"
            @retry="openConsole"
          />
          <DiagnosticBanner
            v-else-if="session.status === 'expired'"
            code="CONSOLE_EXPIRED"
            message="控制台会话已过期，请重新签发。"
            :retryable="true"
            severity="warning"
            @retry="openConsole"
          />
          <DiagnosticBanner
            v-else-if="session.status === 'denied'"
            code="CONSOLE_DENIED"
            message="控制台访问被拒绝：授权已撤销或越权。"
            :retryable="false"
            severity="error"
          />

          <div ref="panelEl" class="console-body" :class="{ 'console-body--active': sessionActive }">
            <div v-if="session.status === 'connecting'" class="console-status">正在连接…</div>
            <div v-else-if="session.status === 'closed'" class="console-status">会话已断开。</div>
            <div v-else-if="!sessionActive" class="console-status">
              {{ isReady ? '点击按钮打开控制台。' : '当前环境状态不支持控制台。' }}
            </div>

            <XtermConsole
              v-if="kind === 'xterm' && sessionActive"
              ref="xtermRef"
              :send="session.send"
              :send-resize="session.sendResize"
            />
            <NoVncConsole
              v-else-if="kind === 'novnc' && vncCapability"
              :capability="vncCapability"
              @state-change="onVncStateChange"
            />
          </div>
        </template>
      </template>
    </AsyncStateView>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import type {
  AccessGrantSchema,
  ConsoleCapabilitySchema,
  ConsoleKind,
  EnvironmentInstanceSchema,
} from '@/generated/contracts'
import { useConsoleCapability } from '@/composables/useConsoleCapability'
import { useConsoleSession } from '@/composables/useConsoleSession'
import AsyncStateView from '@/components/common/AsyncStateView.vue'
import DiagnosticBanner from '@/components/common/DiagnosticBanner.vue'
import SvgIcon from '@/components/common/SvgIcon.vue'
import XtermConsole from './XtermConsole.vue'
import NoVncConsole from './NoVncConsole.vue'
import type { DiagnosticViewModel } from '@/types/async'

interface Props {
  kind: ConsoleKind
  grant: AccessGrantSchema
  environment: EnvironmentInstanceSchema
}

const props = defineProps<Props>()

const grantId = computed(() => props.grant.id)
const capability = useConsoleCapability(grantId)
const session = useConsoleSession()

const xtermRef = ref<InstanceType<typeof XtermConsole> | null>(null)
const panelEl = ref<HTMLDivElement | null>(null)
const issueError = ref<DiagnosticViewModel | null>(null)

// noVNC owns its handoff exclusively (ADR 0012 one-time locator): it is NOT
// routed through the shared useConsoleSession socket. Its lifecycle is
// mirrored into `vncStatus` so the banner/buttons stay consistent.
const vncCapability = ref<ConsoleCapabilitySchema | null>(null)
const vncStatus = ref<'idle' | 'connecting' | 'open' | 'closed' | 'error'>('idle')

const isReady = computed(() => props.environment.observedState === 'ready')

const sessionActive = computed(() => {
  if (props.kind === 'xterm') return session.status === 'connecting' || session.status === 'open'
  return vncStatus.value === 'connecting' || vncStatus.value === 'open'
})

session.onData((data) => {
  if (props.kind === 'xterm') xtermRef.value?.handleData(data)
})

async function openConsole() {
  issueError.value = null
  const leaseFence =
    props.environment.class === 'work' && props.environment.leaseId
      ? { leaseId: props.environment.leaseId, leaseRevision: props.environment.revision, expiresAt: props.environment.eligibilityExpiresAt }
      : null
  const result = await capability.issue(props.kind, {
    environmentRevision: props.environment.revision,
    leaseFence,
  })
  if (!result.ok || !result.capability) {
    issueError.value = result.diagnostic ?? null
    return
  }
  if (props.kind === 'xterm') {
    // xterm consumes the one-time locator through the shared session socket.
    await session.connect(result.capability)
  } else {
    // noVNC owns the handoff itself; do NOT also open it via the shared
    // session socket, or the second consumption is rejected by the proxy.
    vncStatus.value = 'connecting'
    vncCapability.value = result.capability
  }
}

function closeConsole() {
  if (props.kind === 'xterm') {
    session.disconnect()
  } else {
    vncCapability.value = null
    vncStatus.value = 'idle'
  }
}

function onVncStateChange(state: 'connecting' | 'open' | 'closed' | 'error', code?: string) {
  if (state === 'open') vncStatus.value = 'open'
  else if (state === 'connecting') vncStatus.value = 'connecting'
  else if (state === 'closed') vncStatus.value = 'closed'
  else if (state === 'error') {
    vncStatus.value = 'error'
    vncCapability.value = null
    session.diagnostic = { code: code ?? 'CONSOLE_UPSTREAM_UNAVAILABLE', message: '图形控制台上游不可用。', retryable: true }
  }
}

function toggleFullscreen() {
  if (!panelEl.value) return
  if (document.fullscreenElement) void document.exitFullscreen()
  else void panelEl.value.requestFullscreen?.()
}

watch(
  () => props.environment.observedState,
  (state, previous) => {
    // Fail closed: if the environment leaves ready while a session is active,
    // drop the session instead of letting it linger.
    if (previous === 'ready' && state !== 'ready') {
      closeConsole()
    }
  },
)

watch(
  () => props.grant.state,
  (state) => {
    if (state !== 'active') closeConsole()
  },
)

onMounted(() => {
  capability.load()
})
</script>

<style scoped>
.console-panel {
  border: 1px solid var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-medium);
  background: var(--md-sys-color-surface-container-low);
  overflow: hidden;
}

.console-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 12px 16px;
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
}

.console-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font: var(--md-sys-title-small);
  color: var(--md-sys-color-on-surface);
  margin: 0;
}

.console-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.filled-button,
.text-button {
  height: 36px;
  padding: 0 16px;
  border: none;
  border-radius: var(--md-sys-shape-full);
  font: var(--md-sys-label-large);
  cursor: pointer;
}

.filled-button {
  background: var(--md-sys-color-primary);
  color: var(--md-sys-color-on-primary);
}

.filled-button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.text-button {
  background: transparent;
  color: var(--md-sys-color-primary);
}

.icon-button {
  width: 36px;
  height: 36px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border: none;
  border-radius: var(--md-sys-shape-full);
  background: transparent;
  color: var(--md-sys-color-on-surface-variant);
  cursor: pointer;
}

.console-body {
  position: relative;
  min-height: 120px;
}

.console-status {
  padding: 24px;
  font: var(--md-sys-body-medium);
  color: var(--md-sys-color-on-surface-variant);
  text-align: center;
}

.console-body--active .console-status {
  display: none;
}

.console-body:fullscreen {
  background: #000;
}
</style>
