<template>
  <span
    class="gcp-status-pill"
    :class="[`gcp-status-pill--${category}`, `gcp-status-pill--${size}`, state ? `gcp-status-pill--${state}` : '']"
    :title="titleText"
  >
    <span class="status-indicator" :class="{ 'status-indicator--pulse': isPulsing }" aria-hidden="true">
      <span class="status-dot" />
    </span>
    <span class="status-label">{{ displayLabel }}</span>
    <span v-if="diagnostic" class="status-diagnostic" aria-hidden="true">[{{ diagnostic }}]</span>
  </span>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import {
  environmentStateLabel,
  agentRunStateLabel,
  resourceRequestStateLabel,
  accessGrantStateLabel,
  endpointHealthLabel,
  evaluationStateLabel,
} from '@/utils/stateLabels'

interface Props {
  state: string | null | undefined
  domain?: 'environment' | 'agent' | 'resource' | 'access' | 'endpoint' | 'evaluation' | 'custom'
  label?: string
  diagnostic?: string | null
  size?: 'sm' | 'md'
}

const props = withDefaults(defineProps<Props>(), {
  domain: 'environment',
  label: undefined,
  diagnostic: null,
  size: 'md',
})

const displayLabel = computed(() => {
  if (props.label) return props.label
  const s = props.state
  if (!s) return '—'
  switch (props.domain) {
    case 'environment':
      return environmentStateLabel(s)
    case 'agent':
      return agentRunStateLabel(s)
    case 'resource':
      return resourceRequestStateLabel(s)
    case 'access':
      return accessGrantStateLabel(s)
    case 'endpoint':
      return endpointHealthLabel(s)
    case 'evaluation':
      return evaluationStateLabel(s)
    default:
      return s
  }
})

const category = computed<'success' | 'running' | 'warning' | 'error' | 'neutral'>(() => {
  const s = (props.state ?? '').toLowerCase()
  if (['ready', 'running', 'active', 'succeeded', 'healthy', 'approved'].includes(s)) {
    return 'success'
  }
  if (['building', 'provisioning', 'validating', 'allocating', 'updating', 'requested', 'reviewing', 'planning', 'aggregating'].includes(s)) {
    return 'running'
  }
  if (['expiring', 'partially_succeeded', 'awaiting_teacher_review'].includes(s)) {
    return 'warning'
  }
  if (['failed', 'error', 'rejected', 'expired', 'revoked', 'timed_out', 'infrastructure_error'].includes(s)) {
    return 'error'
  }
  return 'neutral'
})

const isPulsing = computed(() => category.value === 'running')

const titleText = computed(() => {
  const base = `${displayLabel.value} (${props.state ?? 'unknown'})`
  return props.diagnostic ? `${base} - ${props.diagnostic}` : base
})
</script>

<style scoped>
.gcp-status-pill {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 2px 8px;
  border-radius: var(--md-sys-shape-full);
  font: var(--md-sys-label-small);
  font-weight: 500;
  line-height: 1.4;
  white-space: nowrap;
  vertical-align: middle;
  border: 1px solid transparent;
}

.gcp-status-pill--sm {
  padding: 1px 6px;
  font-size: 11px;
}

.status-indicator {
  position: relative;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 8px;
  height: 8px;
  flex-shrink: 0;
}

.status-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
}

/* Pulse animation for running/provisioning states */
.status-indicator--pulse::after {
  content: '';
  position: absolute;
  inset: -2px;
  border-radius: 50%;
  border: 1.5px solid currentColor;
  opacity: 0.8;
  animation: gcp-pulse 1.8s cubic-bezier(0.24, 0, 0.38, 1) infinite;
}

@keyframes gcp-pulse {
  0% {
    transform: scale(0.6);
    opacity: 0.9;
  }
  70% {
    transform: scale(1.6);
    opacity: 0;
  }
  100% {
    transform: scale(1.6);
    opacity: 0;
  }
}

@media (prefers-reduced-motion: reduce) {
  .status-indicator--pulse::after {
    animation: none;
    opacity: 0.4;
  }
}

/* Theme states */
.gcp-status-pill--success {
  background: rgba(23, 107, 58, 0.1);
  color: var(--md-sys-color-success);
  border-color: rgba(23, 107, 58, 0.25);
}
.gcp-status-pill--success .status-dot {
  background: var(--md-sys-color-success);
}

.gcp-status-pill--running {
  background: rgba(0, 106, 106, 0.1);
  color: var(--md-sys-color-primary);
  border-color: rgba(0, 106, 106, 0.25);
}
.gcp-status-pill--running .status-dot {
  background: var(--md-sys-color-primary);
}

.gcp-status-pill--warning {
  background: rgba(138, 79, 0, 0.1);
  color: var(--md-sys-color-warning);
  border-color: rgba(138, 79, 0, 0.25);
}
.gcp-status-pill--warning .status-dot {
  background: var(--md-sys-color-warning);
}

.gcp-status-pill--error {
  background: rgba(186, 26, 26, 0.1);
  color: var(--md-sys-color-error);
  border-color: rgba(186, 26, 26, 0.25);
}
.gcp-status-pill--error .status-dot {
  background: var(--md-sys-color-error);
}

.gcp-status-pill--neutral {
  background: var(--md-sys-color-surface-container);
  color: var(--md-sys-color-on-surface-variant);
  border-color: var(--md-sys-color-outline-variant);
}
.gcp-status-pill--neutral .status-dot {
  background: var(--md-sys-color-outline);
}

.status-diagnostic {
  font-family: monospace;
  font-size: 10px;
  opacity: 0.8;
  margin-left: 2px;
}
</style>
