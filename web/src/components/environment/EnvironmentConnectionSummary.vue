<template>
  <section
    class="connection-summary md-card"
    aria-labelledby="connection-summary-heading"
    data-testid="connection-summary"
    :data-readiness="readiness.kind"
  >
    <div class="connection-summary__head">
      <h4
        id="connection-summary-heading"
        class="connection-summary__title"
      >
        连接
      </h4>
      <button
        type="button"
        class="text-button small"
        :disabled="checking"
        @click="emit('recheck')"
      >
        {{ checking ? '检查中…' : '重新检查' }}
      </button>
    </div>
    <dl class="connection-summary__states">
      <div>
        <dt>实例</dt>
        <dd data-testid="instance-state">
          {{ environmentStateLabel(instance.observedState) }}
        </dd>
      </div>
      <div>
        <dt>连接</dt>
        <dd data-testid="connection-state">
          {{ readiness.label }}
        </dd>
      </div>
    </dl>
    <p
      class="connection-summary__detail"
      role="status"
    >
      {{ readiness.detail }}
      <span v-if="autoChecking"> 页面每隔几秒自动重新检查。</span>
    </p>
    <div
      v-if="readiness.kind === 'needs-grant' || readiness.kind === 'needs-regrant'"
      class="connection-summary__actions"
    >
      <button
        type="button"
        class="filled-button small"
        :disabled="issuing"
        @click="emit('issue-grant')"
      >
        {{ issuing ? '签发中…' : readiness.kind === 'needs-regrant' ? '重新签发并连接' : '签发授权并连接' }}
      </button>
    </div>
    <ul
      v-if="methods.length > 0"
      class="connection-summary__methods"
      aria-label="连接方式"
    >
      <li
        v-for="method in methods"
        :key="method.id"
        class="connection-method"
        :data-method="method.id"
      >
        <span class="connection-method__text">
          <strong>{{ method.label }}</strong>
          <span>{{ method.description }}</span>
        </span>
        <button
          v-if="method.id === 'console' || method.id === 'desktop'"
          type="button"
          class="outlined-button small"
          :disabled="readiness.kind !== 'ready'"
          @click="emit('open-console', method.id === 'desktop' ? 'novnc' : 'xterm')"
        >
          {{ method.id === 'desktop' ? '打开桌面' : '打开 Web 终端' }}
        </button>
      </li>
    </ul>
    <p
      class="connection-summary__hint"
      data-testid="console-capability-hint"
    >
      {{ consoleHint }}
    </p>
  </section>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import type {
  AccessGrantSchema,
  ConsoleCapabilityAvailabilitySchema,
  EnvironmentEndpointSchema,
  EnvironmentInstanceSchema,
} from '@/generated/contracts'
import type { AsyncState } from '@/types/async'
import { connectionMethods, connectionReadiness } from '@/utils/environmentConnection'
import { environmentStateLabel } from '@/utils/stateLabels'

const props = defineProps<{
  instance: EnvironmentInstanceSchema
  endpoints: AsyncState<EnvironmentEndpointSchema[]>
  grant: AsyncState<AccessGrantSchema>
  /** Browser console kinds the current access grant permits; read only while a grant is usable. */
  consoleCapability: AsyncState<ConsoleCapabilityAvailabilitySchema>
  /** Why the instance cannot be connected yet, worded by the console that owns lifecycle actions. */
  instanceBlockReason: string
  now: number
  autoChecking: boolean
  checking: boolean
  issuing: boolean
}>()

const emit = defineEmits<{ 'open-console': [kind: 'xterm' | 'novnc']; recheck: []; 'issue-grant': [] }>()

const readiness = computed(() => connectionReadiness(props.instance, props.endpoints, props.grant, props.instanceBlockReason, props.now))
const consoleKinds = computed(() => readiness.value.kind === 'ready' && props.consoleCapability.kind === 'success'
  ? props.consoleCapability.data.kinds
  : [])
const methods = computed(() => connectionMethods(props.endpoints, consoleKinds.value))
const consoleHint = computed(() => {
  if (readiness.value.kind !== 'ready') return '浏览器终端和桌面在访问授权生效后，按平台返回的控制台能力列出。'
  const capability = props.consoleCapability
  if (capability.kind === 'success') {
    return capability.data.kinds.length > 0 ? '浏览器控制台按平台返回的能力列出。' : '这个环境没有浏览器终端或桌面，请使用上面列出的其他连接方式。'
  }
  if (capability.kind === 'idle' || capability.kind === 'loading') return '正在读取浏览器控制台能力。'
  if (capability.kind === 'empty') return '这个环境没有浏览器终端或桌面，请使用上面列出的其他连接方式。'
  return `无法读取浏览器控制台能力：${capability.diagnostic.message}`
})
</script>

<style scoped>
.connection-summary { display: grid; gap: var(--space-3); padding: var(--space-4); border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
.connection-summary__head { display: flex; align-items: center; justify-content: space-between; gap: var(--space-2); }
.connection-summary__title { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.connection-summary__states { display: flex; flex-wrap: wrap; gap: var(--space-2) var(--space-8); margin: 0; }
.connection-summary__states div { display: grid; gap: 2px; }
.connection-summary__states dt { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.connection-summary__states dd { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.connection-summary__detail { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.connection-summary__hint { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.connection-summary__methods { display: grid; gap: var(--space-2); margin: 0; padding: 0; list-style: none; }
.connection-method { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: var(--space-2) var(--space-4); padding: var(--space-2) 0; border-top: 1px solid var(--md-sys-color-outline-variant); }
.connection-method__text { display: grid; gap: 2px; min-width: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.connection-summary__actions { display: flex; flex-wrap: wrap; gap: var(--space-2); }
</style>
