<template>
  <div class="diagnostic-banner" :class="`diagnostic-banner--${severity}`" role="alert">
    <SvgIcon :name="iconName" size="md" />
    <div class="diagnostic-content">
      <span v-if="code" class="diagnostic-code">{{ code }}</span>
      <span class="diagnostic-message">{{ message }}</span>
    </div>
    <button
      v-if="retryable"
      type="button"
      class="text-button"
      @click="$emit('retry')"
    >
      重试
    </button>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import SvgIcon from './SvgIcon.vue'

const props = withDefaults(
  defineProps<{
    code?: string
    message: string
    retryable?: boolean
    severity?: 'error' | 'warning' | 'info'
  }>(),
  {
    retryable: false,
    severity: 'error',
  }
)

defineEmits<{
  retry: []
}>()

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
.diagnostic-banner {
  display: flex;
  align-items: flex-start;
  gap: var(--space-3);
  padding: var(--space-3) var(--space-4);
  border-radius: var(--md-sys-shape-medium);
  font: var(--md-sys-body-medium);
}

.diagnostic-banner--error {
  background: var(--md-sys-color-error-container);
  color: var(--md-sys-color-on-error-container);
}

.diagnostic-banner--warning {
  background: var(--md-sys-color-warning-container);
  color: var(--md-sys-color-on-surface);
}

.diagnostic-banner--info {
  background: var(--md-sys-color-primary-container);
  color: var(--md-sys-color-on-primary-container);
}

.diagnostic-content {
  display: flex;
  flex-direction: column;
  flex: 1;
  gap: var(--space-1);
}

.diagnostic-code {
  font: var(--md-sys-label-small);
  text-transform: uppercase;
  opacity: 0.8;
}

.diagnostic-message {
  word-break: break-word;
}

</style>
