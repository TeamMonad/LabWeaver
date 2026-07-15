<template>
  <div class="async-state-view">
    <slot v-if="state.kind === 'success'" name="success" :data="state.data" />

    <div v-else-if="state.kind === 'loading'" class="state-message" role="status">
      <span class="spinner" aria-hidden="true" />
      <span>{{ state.message ?? loadingText }}</span>
    </div>

    <div v-else-if="state.kind === 'empty'" class="state-message" role="status">
      <SvgIcon name="info" size="lg" aria-hidden="true" />
      <span>{{ emptyText }}</span>
    </div>

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

<script setup lang="ts">
import DiagnosticBanner from './DiagnosticBanner.vue'
import SvgIcon from './SvgIcon.vue'
import type { AsyncState } from '@/types/async'

interface Props {
  state: AsyncState<unknown>
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
  gap: 16px;
  min-height: 200px;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-medium);
}

.spinner {
  width: 32px;
  height: 32px;
  border: 3px solid var(--md-sys-color-outline-variant);
  border-top-color: var(--md-sys-color-primary);
  border-radius: 50%;
  animation: spin 1s linear infinite;
}

@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}

@media (prefers-reduced-motion: reduce) {
  .spinner {
    animation: none;
  }
}
</style>
