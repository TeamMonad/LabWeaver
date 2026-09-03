<template>
  <div class="gcp-action-bar" role="toolbar" :aria-label="ariaLabel || '操作工具栏'">
    <div class="gcp-action-bar__leading">
      <slot name="leading" />
    </div>

    <div class="gcp-action-bar__actions">
      <slot />

      <button
        v-if="showRefresh"
        type="button"
        class="action-button action-button--text"
        :disabled="refreshing"
        aria-label="刷新数据"
        @click="$emit('refresh')"
      >
        <SvgIcon
          name="refresh"
          size="sm"
          :class="{ 'spin-icon': refreshing }"
          aria-hidden="true"
        />
        <span class="action-button__label">{{ refreshing ? '刷新中…' : '刷新' }}</span>
      </button>

      <label v-if="showAutoRefresh" class="auto-refresh-toggle" title="开启后自动保持数据最新">
        <input
          type="checkbox"
          :checked="autoRefresh"
          @change="$emit('update:autoRefresh', ($event.target as HTMLInputElement).checked)"
        />
        <span>自动刷新</span>
      </label>
    </div>

    <div v-if="$slots.trailing" class="gcp-action-bar__trailing">
      <slot name="trailing" />
    </div>
  </div>
</template>

<script setup lang="ts">
import SvgIcon from '@/components/common/SvgIcon.vue'

interface Props {
  ariaLabel?: string
  showRefresh?: boolean
  refreshing?: boolean
  showAutoRefresh?: boolean
  autoRefresh?: boolean
}

withDefaults(defineProps<Props>(), {
  ariaLabel: undefined,
  showRefresh: true,
  refreshing: false,
  showAutoRefresh: false,
  autoRefresh: true,
})

defineEmits<{
  refresh: []
  'update:autoRefresh': [value: boolean]
}>()
</script>

<style scoped>
.gcp-action-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 8px 0 12px;
  border-bottom: 1px solid var(--md-sys-color-outline-variant);
  margin-bottom: 16px;
  flex-wrap: wrap;
}

.gcp-action-bar__leading,
.gcp-action-bar__actions,
.gcp-action-bar__trailing {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.action-button {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 32px;
  padding: 0 12px;
  border-radius: var(--md-sys-shape-small);
  font: var(--md-sys-label-medium);
  font-weight: 500;
  text-decoration: none;
  cursor: pointer;
  white-space: nowrap;
  transition: background-color 0.15s ease, opacity 0.15s ease;
  border: 1px solid transparent;
}

.action-button--text {
  background: transparent;
  color: var(--md-sys-color-on-surface-variant);
}

.action-button--text:hover:not(:disabled) {
  background: var(--md-sys-color-surface-container-high);
  color: var(--md-sys-color-on-surface);
}

.action-button:disabled {
  opacity: 0.45;
  cursor: not-allowed;
}

.auto-refresh-toggle {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font: var(--md-sys-body-small);
  color: var(--md-sys-color-on-surface-variant);
  cursor: pointer;
  user-select: none;
  margin-left: 4px;
}

.auto-refresh-toggle input {
  cursor: pointer;
}

.spin-icon {
  animation: spin 1s linear infinite;
}

@keyframes spin {
  from {
    transform: rotate(0deg);
  }
  to {
    transform: rotate(360deg);
  }
}

@media (prefers-reduced-motion: reduce) {
  .spin-icon {
    animation: none;
  }
}
</style>
