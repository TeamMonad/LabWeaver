<template>
  <div class="async-state-view">
    <slot v-if="state.kind === 'success'" name="success" :data="state.data" />

    <!-- Cloudscape status indicator: inline spinner + text inside the content area. -->
    <div v-else-if="state.kind === 'loading'" class="state-loading" role="status" aria-live="polite" aria-busy="true">
      <span class="state-spinner" aria-hidden="true" />
      <span class="state-loading__text">{{ state.message ?? loadingText }}</span>
    </div>

    <slot v-else-if="state.kind === 'empty'" name="empty">
      <!-- Cloudscape empty state: bold title, optional description and action. -->
      <div class="state-message" role="status">
        <strong class="state-message__title">{{ emptyText }}</strong>
        <span v-if="$slots['empty-description']" class="state-message__description"><slot name="empty-description" /></span>
        <div v-if="$slots['empty-action']" class="state-message__action"><slot name="empty-action" /></div>
      </div>
    </slot>

    <DiagnosticBanner
      v-else-if="state.kind === 'error' || isFailureState(state.kind)"
      :code="state.diagnostic.code"
      :message="state.diagnostic.message"
      :retryable="state.diagnostic.retryable"
      :severity="bannerSeverity(state.kind)"
      @retry="$emit('retry')"
    />
  </div>
</template>

<script setup lang="ts" generic="T">
import DiagnosticBanner from './DiagnosticBanner.vue'
import type { AsyncState } from '@/types/async'

interface Props {
  state: AsyncState<T>
  loadingText?: string
  emptyText?: string
}

withDefaults(defineProps<Props>(), {
  loadingText: '加载中…',
  emptyText: '暂无数据',
})

defineEmits<{
  retry: []
}>()

type FailureKind = 'error' | 'blocked' | 'timeout' | 'conflict' | 'unauthorized' | 'revoked' | 'sse-gap'

function isFailureState(kind: string): kind is FailureKind {
  return ['error', 'blocked', 'timeout', 'conflict', 'unauthorized', 'revoked', 'sse-gap'].includes(kind)
}

function bannerSeverity(kind: FailureKind): 'error' | 'warning' | 'info' {
  switch (kind) {
    case 'unauthorized':
    case 'revoked':
      return 'warning'
    case 'timeout':
    case 'conflict':
      return 'warning'
    case 'sse-gap':
      return 'info'
    default:
      return 'error'
  }
}
</script>

<style scoped>
.async-state-view {
  width: 100%;
}

.state-message {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: var(--space-1);
  padding: var(--space-8) var(--space-6);
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-medium);
  text-align: center;
}

.state-message__title {
  color: var(--md-sys-color-on-surface);
  font: var(--md-sys-title-small);
}

.state-message__action {
  margin-top: var(--space-3);
}

.state-loading {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: var(--space-2);
  padding: var(--space-8) var(--space-4);
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-medium);
}

.state-spinner {
  width: 16px;
  height: 16px;
  border: 2px solid var(--md-sys-color-outline-variant);
  border-top-color: var(--md-sys-color-primary);
  border-radius: 50%;
  animation: state-spin 0.7s linear infinite;
}

@keyframes state-spin {
  to {
    transform: rotate(360deg);
  }
}
</style>
