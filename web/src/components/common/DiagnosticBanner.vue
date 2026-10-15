<template>
  <div class="diagnostic-banner" :class="`diagnostic-banner--${severity}`" role="alert" aria-live="assertive">
    <SvgIcon class="diagnostic-icon" :name="iconName" size="md" aria-hidden="true" />
    <div class="diagnostic-content">
      <span class="diagnostic-message">{{ resolvedMessage }}</span>
      <span v-if="resolvedNextStep" class="diagnostic-next-step">{{ resolvedNextStep }}</span>
    </div>
    <button
      v-if="resolvedRetryable"
      type="button"
      class="text-button small diagnostic-retry"
      @click="$emit('retry')"
    >
      重试
    </button>
    <details v-if="resolvedCode || resolvedRequestId" class="diagnostic-details">
      <summary>诊断信息</summary>
      <dl class="diagnostic-fields">
        <template v-if="resolvedCode">
          <dt>诊断码</dt>
          <dd><code class="diagnostic-code">{{ resolvedCode }}</code></dd>
        </template>
        <template v-if="resolvedRequestId">
          <dt>请求 ID</dt>
          <dd><code class="diagnostic-request-id">{{ resolvedRequestId }}</code></dd>
        </template>
      </dl>
      <CopyButton
        class="diagnostic-copy"
        :text="copyText"
        label="复制诊断信息"
        aria-label="复制诊断码和请求 ID"
      />
    </details>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import CopyButton from './CopyButton.vue'
import SvgIcon from './SvgIcon.vue'
import type { DiagnosticViewModel } from '@/types/async'

/**
 * Reason first, then the next step; code and requestId stay copyable in a
 * collapsed detail so they never become the primary text.
 */
const props = withDefaults(
  defineProps<{
    diagnostic?: DiagnosticViewModel | null
    code?: string
    message?: string
    requestId?: string
    nextStep?: string
    retryable?: boolean
    severity?: 'error' | 'warning' | 'info'
  }>(),
  {
    diagnostic: null,
    code: undefined,
    message: undefined,
    requestId: undefined,
    nextStep: undefined,
    retryable: undefined,
    severity: 'error',
  }
)

defineEmits<{
  retry: []
}>()

const resolvedCode = computed(() => props.code ?? props.diagnostic?.code ?? '')
const resolvedMessage = computed(() => props.message ?? props.diagnostic?.message ?? '')
const resolvedRequestId = computed(() => props.requestId ?? props.diagnostic?.requestId ?? '')
const resolvedNextStep = computed(() => props.nextStep ?? props.diagnostic?.nextStep ?? '')
const resolvedRetryable = computed(() => props.retryable ?? props.diagnostic?.retryable ?? false)
const copyText = computed(() => [
  resolvedCode.value && `诊断码：${resolvedCode.value}`,
  resolvedRequestId.value && `请求 ID：${resolvedRequestId.value}`,
  resolvedMessage.value && `说明：${resolvedMessage.value}`,
].filter(Boolean).join('\n'))

const iconName = computed(() => {
  switch (props.severity) {
    case 'warning':
      return 'warning'
    case 'info':
      return 'info'
    default:
      return 'error'
  }
})
</script>

<style scoped>
/* Calm, tinted surface with a severity accent; the human reason and next step
   lead, while code/requestId stay as quiet monospace metadata in the details. */
.diagnostic-banner {
  --diagnostic-accent: var(--md-sys-color-error);
  display: grid;
  grid-template-columns: auto minmax(0, 1fr) auto;
  align-items: start;
  gap: var(--space-1) var(--space-3);
  padding: var(--space-3) var(--space-4);
  border: 1px solid color-mix(in srgb, var(--diagnostic-accent) 22%, transparent);
  border-inline-start: 3px solid var(--diagnostic-accent);
  border-radius: var(--md-sys-shape-medium);
  background: color-mix(in srgb, var(--diagnostic-accent) 6%, var(--md-sys-color-surface));
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-body-medium);
  animation: diagnostic-in var(--motion-duration-base) var(--motion-ease-out);
}

.diagnostic-banner--warning {
  --diagnostic-accent: var(--md-sys-color-warning);
}

.diagnostic-banner--info {
  --diagnostic-accent: var(--md-sys-color-primary);
}

.diagnostic-icon {
  margin-top: 1px;
  color: var(--diagnostic-accent);
}

.diagnostic-content {
  display: flex;
  min-width: 0;
  flex-direction: column;
  flex: 1;
  gap: var(--space-1);
}

.diagnostic-message {
  font-weight: 500;
  overflow-wrap: anywhere;
}

.diagnostic-next-step {
  color: var(--md-sys-color-on-surface-variant);
  overflow-wrap: anywhere;
}

.diagnostic-retry {
  flex-shrink: 0;
  align-self: center;
}

.diagnostic-details {
  grid-column: 2 / -1;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}

.diagnostic-details summary {
  width: fit-content;
  cursor: pointer;
}

.diagnostic-details summary:focus-visible {
  outline: 2px solid currentColor;
  outline-offset: 2px;
}

.diagnostic-fields {
  display: grid;
  grid-template-columns: auto 1fr;
  gap: 2px var(--space-3);
  margin: var(--space-2) 0;
}

.diagnostic-fields dd {
  min-width: 0;
  margin: 0;
}

.diagnostic-fields code {
  font-family: var(--md-sys-font-family-mono);
  letter-spacing: 0.02em;
  overflow-wrap: anywhere;
}

/* Flashbar stacking: consecutive notifications sit tightly together. */
.diagnostic-banner + .diagnostic-banner {
  margin-top: var(--layout-flashbar-gap);
}

@keyframes diagnostic-in {
  from {
    opacity: 0;
    transform: translateY(-2px);
  }
}
</style>
