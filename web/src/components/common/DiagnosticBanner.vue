<template>
  <div class="diagnostic-banner" :class="`diagnostic-banner--${severity}`" role="alert" aria-live="assertive">
    <SvgIcon class="diagnostic-icon" :name="iconName" size="md" />
    <div class="diagnostic-content">
      <span class="diagnostic-message">{{ message }}</span>
      <span v-if="code" class="diagnostic-code" :title="code">{{ code }}</span>
    </div>
    <button
      v-if="retryable"
      type="button"
      class="text-button small diagnostic-retry"
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
/* Calm, tinted surface with a severity accent; the human message leads and the
   machine code trails as quiet monospace metadata. */
.diagnostic-banner {
  --diagnostic-accent: var(--md-sys-color-error);
  display: flex;
  align-items: flex-start;
  gap: var(--space-3);
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
  flex: 1;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.diagnostic-message {
  overflow-wrap: anywhere;
}

.diagnostic-code {
  overflow: hidden;
  color: var(--md-sys-color-on-surface-variant);
  font: var(--md-sys-label-small);
  font-family: var(--md-sys-font-family-mono);
  letter-spacing: 0.02em;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.diagnostic-retry {
  flex-shrink: 0;
  align-self: center;
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
