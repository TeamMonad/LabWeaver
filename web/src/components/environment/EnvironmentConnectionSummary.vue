<template>
  <section
    class="connection-summary md-card"
    aria-labelledby="connection-summary-heading"
    data-testid="connection-summary"
    :data-readiness="readiness.kind"
  >
    <h4
      id="connection-summary-heading"
      class="connection-summary__title"
    >
      连接
    </h4>
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
    </p>
    <ul
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
          :disabled="!instanceIsReady(instance)"
          :title="instanceIsReady(instance) ? undefined : instanceBlockReason"
          @click="emit('open-console')"
        >
          {{ method.id === 'desktop' ? '打开桌面' : '打开 Web 终端' }}
        </button>
      </li>
    </ul>
  </section>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import type { AccessGrantSchema, EnvironmentEndpointSchema, EnvironmentInstanceSchema } from '@/generated/contracts'
import type { AsyncState } from '@/types/async'
import { connectionMethods, connectionReadiness, instanceIsReady } from '@/utils/environmentConnection'
import { environmentStateLabel } from '@/utils/stateLabels'

const props = defineProps<{
  instance: EnvironmentInstanceSchema
  endpoints: AsyncState<EnvironmentEndpointSchema[]>
  grant: AsyncState<AccessGrantSchema>
  /** Why the instance cannot be connected yet, worded by the console that owns lifecycle actions. */
  instanceBlockReason: string
}>()

const emit = defineEmits<{ 'open-console': [] }>()

const readiness = computed(() => connectionReadiness(props.instance, props.endpoints, props.grant, props.instanceBlockReason))
const methods = computed(() => connectionMethods(props.instance, props.endpoints))
</script>

<style scoped>
.connection-summary { display: grid; gap: 12px; padding: 16px; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-medium); }
.connection-summary__title { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.connection-summary__states { display: flex; flex-wrap: wrap; gap: 8px 32px; margin: 0; }
.connection-summary__states div { display: grid; gap: 2px; }
.connection-summary__states dt { color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-label-medium); }
.connection-summary__states dd { margin: 0; color: var(--md-sys-color-on-surface); font: var(--md-sys-title-small); }
.connection-summary__detail { margin: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-medium); }
.connection-summary__methods { display: grid; gap: 8px; margin: 0; padding: 0; list-style: none; }
.connection-method { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 8px 16px; padding: 8px 0; border-top: 1px solid var(--md-sys-color-outline-variant); }
.connection-method__text { display: grid; gap: 2px; min-width: 0; color: var(--md-sys-color-on-surface-variant); font: var(--md-sys-body-small); }
.connection-method__text strong { color: var(--md-sys-color-on-surface); font: var(--md-sys-label-large); }
.outlined-button { display: inline-flex; align-items: center; justify-content: center; min-height: 32px; padding: 0 16px; border: 1px solid var(--md-sys-color-outline); border-radius: var(--md-sys-shape-full); background: transparent; color: var(--md-sys-color-primary); font: var(--md-sys-label-large); cursor: pointer; }
.outlined-button:disabled { color: color-mix(in srgb, var(--md-sys-color-on-surface) 38%, transparent); cursor: not-allowed; }
</style>
