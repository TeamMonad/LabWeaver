<template>
  <div class="async-state-view">
    <slot v-if="state.kind === 'success'" name="success" :data="state.data" />

    <div v-else-if="state.kind === 'loading'" class="state-loading" role="status" aria-busy="true">
      <div class="state-skeleton" aria-hidden="true">
        <span class="skeleton state-skeleton__line state-skeleton__line--title" />
        <span class="skeleton state-skeleton__line" />
        <span class="skeleton state-skeleton__line state-skeleton__line--short" />
      </div>
      <span class="state-loading__text">{{ state.message ?? loadingText }}</span>
    </div>

    <slot v-else-if="state.kind === 'empty'" name="empty">
      <div class="state-message" role="status">
        <span class="state-message__icon" aria-hidden="true">
          <SvgIcon name="inbox" size="lg" aria-hidden="true" />
        </span>
        <span>{{ emptyText }}</span>
      </div>
    </slot>

    <DiagnosticBanner
      v-else-if="state.kind === 'error' || isFailureState(state.kind)"
      :code="state.diagnostic.code"
      :request-id="state.diagnostic.requestId"
      :next-step="state.diagnostic.nextStep"
      :message="state.diagnostic.message"
      :retryable="state.diagnostic.retryable"
      :severity="bannerSeverity(state.kind)"
      @retry="$emit('retry')"
    />
  </div>
</template>

<script setup lang="ts" generic="T">
import DiagnosticBanner from './DiagnosticBanner.vue'
import SvgIcon from './SvgIcon.vue'
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
  gap: var(--space-3);
  min-height: 160px;
  padding: var(--space-8) var(--space-6);
  border: 1px dashed var(--md-sys-color-outline-variant);
  border-radius: var(--md-sys-shape-large);
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-medium);
  text-align: center;
}

.state-message__icon {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 48px;
  height: 48px;
  border-radius: var(--md-sys-shape-full);
  background: var(--md-sys-color-surface-container);
  color: var(--md-sys-color-on-surface-variant);
}

.state-loading {
  display: grid;
  gap: var(--space-3);
  padding: var(--space-4) 0;
}

.state-skeleton {
  display: grid;
  gap: var(--space-2);
}

.state-skeleton__line {
  display: block;
  height: 14px;
}

.state-skeleton__line--title {
  width: 40%;
  height: 18px;
}

.state-skeleton__line--short {
  width: 64%;
}

.state-loading__text {
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-body-small);
}
</style>
